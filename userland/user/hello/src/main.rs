//! hello —— v0.7 起的参考用户程序（也是 `/init`）
//!
//! 它不是「演示」而是**验收基准**：这段 Rust 编译出的 ELF 会由 `kernel/build.rs`
//! 内嵌，启动时写进 RAMFS 的 `/init`，再由内核的 ELF Loader 装载到 ring3 运行。
//! `make test-user` / `make test-fs` 的硬断言就落在这段程序的串口轨迹上：
//!
//! | 步骤 | 验证的能力 | 可断言的日志 |
//! | ---- | ---- | ---- |
//! | `println!` | `write` → 串口 | `user: write fd=1 len=22` |
//! | `getpid` | PID 传递 | `user: getpid -> 1` |
//! | `open/read/close /etc/motd` | VFS 读路径 | `user: open path=/etc/motd flags=0x0 fd=3`、`user: read fd=3 want=64 got=23`、首次读完后第二次读回 0（EOF） |
//! | `open(O_CREAT)/write /tmp/hello.txt` | VFS 写路径 | `user: write fd=3 len=24` + 内核读回 `fs: readback /tmp/hello.txt len=24` |
//! | `static mut` 自增 | `RW` 段对 ring3 可写 | `hello: static counter=1` |
//! | 非法指针 `write` | syscall 参数校验 | `user: EFAULT write ...` / `hello: bad write ret=-14` |
//! | `brk` / `mmap` / `munmap` | **用户堆**（v0.9 第一步） | `user: brk base=0x8000020000`、`user: mmap len=8192 ... -> 0x8000060000`、`hello: heap PASS` |
//! | `Vec` / `format!` | `#[global_allocator]` 真的落到内核 | `hello: vec len=200 sum=20100` |
//! | `mprotect` | **运行时改保护属性**（v1.0 第一步）：`PROT_NONE` 关页 → 内核拒收该缓冲 → 恢复后内容原样 → 换 `RX` 并真的执行 | `user: mprotect addr=0x8000060000 len=4096 prot=0x0 -> pages=1`、`hello: mprotect none write ret=-14`、`hello: mprotect exec ret=42`、`hello: mprotect PASS=1` |
//! | `nanosleep` | tick 真实推进 | `user: sleep done ticks=2` |
//! | `exit(42)` | 退出码回收 | `user: exit code=42` |
//!
//! 编写新的用户程序时把它当作模板复制即可（`Cargo.toml` + `build.rs` +
//! `user.ld` + `src/main.rs`），约定见 `libc/src/lib.rs` 顶部文档。

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;

use libc::syscall::{raw, SYS_WRITE};
use libc::{
    brk, ckm_audit_read, ckm_component_state, ckm_policy_query, ckm_record_is_deny,
    ckm_record_need, close, component_id, component_kind, component_kind_name, component_state,
    component_state_name, getpid, mmap, mprotect, munmap, nanosleep, open, println, read,
    storage_rescan, write, AUDIT_RECORD_SIZE, CAP_CKM_SERVICE, CKM_OP_FS_WRITE, CKM_OP_POLICY_QUERY,
    CKM_OP_STATE_WRITE, CKM_PREFIX_ETC, COMPONENT_RECORD_SIZE, EPERM, MAP_ANONYMOUS, MAP_PRIVATE,
    O_CREAT, O_RDONLY, O_WRONLY, PROT_EXEC, PROT_NONE, PROT_READ, PROT_WRITE, STORAGE_RESCAN_ALL,
};

/// 可写的静态变量：v0.7 的 ELF Loader 把 `RW` 段映射成 ring3 可写，
/// 因此这里在运行期被改写（v0.6 会 #PF —— 代码页整体只读）。
static mut COUNTER: u32 = 0;

/// 内核提供的文件（`fs::init` 写入，23 字节）。
const MOTD_PATH: &str = "/etc/motd";
/// 用户程序写文件的落点（内核随后读回并打印内容）。
const OUT_PATH: &str = "/tmp/hello.txt";
/// CKM-1：`/init` 以 `CAP_FS_SYSTEM_WRITE` 在**受保护前缀**里建的文件。
///
/// 它与 `/bin/hello`（uid 2000）对同一个前缀的写入构成一对对照：
/// 同一个路径、同一个 flags，一个成功、一个 `EPERM` ——
/// 差别只在凭证，不在路径（`docs/ckm.md` 3.4 判定点 1）。
const INIT_FLAG_PATH: &str = "/etc/init.flag";

/// `brk` 一次增长两页（8 KiB）。
const HEAP_STEP: u64 = 2 * 4096;
/// 匿名映射长度（两页 = 8 KiB）。
const MMAP_LEN: usize = 2 * 4096;
/// 内核 mmap 窗口上限（256 KiB，见 `address_space::USER_MMAP_MAX`）。
const MMAP_MAX: usize = 64 * 4096;
/// `mprotect` 自测里要从 ring3 **执行**的机器码：`mov eax, 42; ret`。
///
/// 先以 `mmap(PROT_READ|PROT_WRITE)` 写进页里，再 `mprotect(RX)` 改成只读可执行 ——
/// 这就是 W^X 要求的「先写、再换只执行」，也是这份自测存在的理由。
static WX_CODE: [u8; 6] = [0xb8, 0x2a, 0x00, 0x00, 0x00, 0xc3];

/// 内核通过 `_start` 调用该入口；返回值即退出码。
#[no_mangle]
pub extern "C" fn main() -> i32 {
    // 22 字节，一次 write —— `make test-user` 断言 `user: write fd=1 len=22`。
    println!("hello from user space");

    // 展示 `{}` 格式化（PID 由 syscall 返回，再经 libc 格式化后写出）。
    let pid = getpid();
    println!("hello: pid={}", pid);

    // ---- 1. 从 RAMFS 读文件：open → read → write → read(EOF) → close ----
    let fd = open(MOTD_PATH, O_RDONLY);
    println!("hello: open {} fd={}", MOTD_PATH, fd);
    if fd >= 0 {
        let mut buf = [0u8; 64];
        let n = read(fd as i32, &mut buf);
        println!("hello: read n={}", n);
        if n > 0 {
            // 把读到的内容原样写到串口（内核日志里能看到 23 字节的那一次 write）
            let _ = write(1, &buf[..n as usize]);
        }
        // 第二次读：offset 已经在文件尾 → 0（EOF），证明偏移量真的在推进
        let eof = read(fd as i32, &mut buf);
        println!("hello: eof read n={}", eof);
        let _ = close(fd as i32);
    }

    // ---- 2. 往 RAMFS 写文件：内核在用户态结束后读回并打印 ----
    let wfd = open(OUT_PATH, O_CREAT | O_WRONLY);
    println!("hello: create {} fd={}", OUT_PATH, wfd);
    if wfd >= 0 {
        let msg = b"written by user program\n";
        let written = write(wfd as i32, msg);
        println!("hello: wrote {} bytes", written);
        let _ = close(wfd as i32);
    }

    // ---- 3. 可写静态数据（RW 段对 ring3 可写） ----
    let counter = unsafe {
        COUNTER += 1;
        COUNTER
    };
    println!("hello: static counter={}", counter);

    // ---- 4. 非法用户指针：内核校验用户缓冲，应返回 -EFAULT（-14） ----
    let bad = unsafe { raw(SYS_WRITE, 1, 0xdead_0000_0000, 1, 0, 0) };
    println!("hello: bad write ret={}", bad);

    // ---- 5. 用户堆：brk / mmap / munmap（v0.9 运行时前置条件） ----
    let heap_ok = heap_selftest();

    // ---- 6. mprotect：PROT_NONE 关页 + RX 转换（v1.0 第一步） ----
    let prot_ok = mprotect_selftest();

    // ---- 7. nanosleep：2 个 tick = 20ms，内核在等待期间 `sti; hlt` ----
    let slept = nanosleep(2);
    println!("hello: slept={}", slept);

    if !heap_ok {
        println!("hello: heap FAIL");
        return 43;
    }
    if !prot_ok {
        println!("hello: mprotect FAIL");
        return 44;
    }

    // ---- 8. CKM-1：凭证与能力判定（`/init` = uid 0 + CAP_FS_SYSTEM_WRITE） ----
    if !ckm_selftest() {
        println!("hello: ckm FAIL");
        return 45;
    }
    42
}

/// CKM-1 自测：把「能力判定」在 ring3 上的结果留成可断言的证据。
///
/// `/init` 的固定身份是 **uid 0 + [`CAP_FS_SYSTEM_WRITE`]**（`docs/ckm.md` 3.1），
/// 所以每个期望值都是确定的，而不是「跑起来看看」：
///
/// | 操作 | 需要 | /init 的结果 |
/// | ---- | ---- | ---- |
/// | `fs_write`（受保护前缀） | `CAP_FS_SYSTEM_WRITE` | 允许（且有端到端写入） |
/// | `state_write` | `CAP_CKM_SERVICE` | `-EPERM`（需要 `0x2`） |
/// | `audit_read` | `CAP_AUDIT_READ` | `-EPERM` |
/// | `storage_rescan` | `CAP_STORAGE_ADMIN` | `-EPERM` |
/// | `policy_query` / `component_state` | 无 | 允许（只读） |
///
/// 最后一条是刻意加的**最小权限对照**：`/init` 能建立系统视图（写 `/etc`），
/// 但不能读审计、不能改策略、不能动存储 —— 这正是「不给 CKM 管理能力」的含义。
fn ckm_selftest() -> bool {
    let mut ok = true;
    let mut rec = [0u8; AUDIT_RECORD_SIZE];
    println!("hello: ckm selftest begin");

    // 1) 查询接口本身无能力要求（它只能问自己，不能枚举别人）
    let q = ckm_policy_query(CKM_OP_POLICY_QUERY, 0, &mut rec);
    println!(
        "hello: ckm policy_query(policy_query) -> {} need=0x{:x}",
        q,
        ckm_record_need(&rec, 0)
    );
    ok &= q == 0 && !ckm_record_is_deny(&rec, 0);

    // 2) 写受保护前缀：/init 持有 CAP_FS_SYSTEM_WRITE → 允许
    let q = ckm_policy_query(CKM_OP_FS_WRITE, CKM_PREFIX_ETC, &mut rec);
    println!(
        "hello: ckm policy_query(fs_write,etc) -> {} need=0x{:x}",
        q,
        ckm_record_need(&rec, 0)
    );
    ok &= q == 0;

    // 3) 写扩展状态树：需要 CAP_CKM_SERVICE，/init 没有 → 拒绝
    let q = ckm_policy_query(CKM_OP_STATE_WRITE, 0, &mut rec);
    println!(
        "hello: ckm policy_query(state_write) -> {} need=0x{:x}",
        q,
        ckm_record_need(&rec, 0)
    );
    ok &= q == EPERM && ckm_record_need(&rec, 0) == CAP_CKM_SERVICE;

    // 4) 审计读取需要 CAP_AUDIT_READ：/init 没有 → -1（审计环因此不是谁都能读的）
    let mut audit = [0u8; AUDIT_RECORD_SIZE * 4];
    let mut audit_total = 0u64;
    let n = ckm_audit_read(&mut audit, 0, Some(&mut audit_total));
    println!("hello: ckm audit_read -> {}", n);
    ok &= n == EPERM;

    // 5) 组件状态表：只读、无能力要求 → 允许（枚举顺序由内核保证确定性）
    let mut comps = [0u8; COMPONENT_RECORD_SIZE * 4];
    let mut comp_count = 0u64;
    let n = ckm_component_state(&mut comps, Some(&mut comp_count));
    println!("hello: ckm component_state -> {} count={}", n, comp_count);
    ok &= n == 3 && comp_count == 3;
    for i in 0..n as usize {
        println!(
            "hello: ckm component[{}] id={} kind={} state={}",
            i,
            component_id(&comps, i),
            component_kind_name(component_kind(&comps, i)),
            component_state_name(component_state(&comps, i))
        );
    }

    // 6) 存储管理（重扫设备）需要 CAP_STORAGE_ADMIN：/init 也不持有 → -1
    let rescan = storage_rescan(STORAGE_RESCAN_ALL);
    println!("hello: ckm storage_rescan -> {}", rescan);
    ok &= rescan == EPERM;

    // 7) 端到端允许：能力位不是「只在查询里返回 0」—— /init 真的写进了 /etc
    let fd = open(INIT_FLAG_PATH, O_CREAT | O_WRONLY);
    println!("hello: ckm open {} fd={}", INIT_FLAG_PATH, fd);
    ok &= fd >= 3;
    if fd >= 3 {
        let msg = b"init: system view established\n";
        let written = write(fd as i32, msg);
        println!("hello: ckm wrote {} bytes to {}", written, INIT_FLAG_PATH);
        let _ = close(fd as i32);
    }

    println!("hello: ckm selftest ok={}", ok as u8);
    ok
}

/// 用户堆自测：`brk` 增长、`mmap` 匿名映射、`munmap` 归还、越界/非法参数的负例，
/// 以及「全局分配器真的能用」（`Vec` + `format!`）。
fn heap_selftest() -> bool {
    let mut ok = true;

    // --- brk：查询 → 增长两页 → 新页必须是零 → 写入模式并读回 ---
    let base = brk(0);
    println!("hello: brk base={:#x}", base);
    let grown = brk(base + HEAP_STEP);
    println!("hello: brk grown={:#x}", grown);
    if grown != base + HEAP_STEP {
        return false;
    }
    let heap = base as *mut u8;
    let zeroed = unsafe {
        core::ptr::read_volatile(heap).eq(&0)
            && core::ptr::read_volatile(heap.add(4095)).eq(&0)
            && core::ptr::read_volatile(heap.add(8191)).eq(&0)
    };
    println!("hello: heap zeroed={}", zeroed as u8);
    ok &= zeroed;
    for i in 0..1024usize {
        unsafe { core::ptr::write_volatile(heap.add(i), (i & 0xff) as u8) };
    }
    let mut pattern_ok = true;
    for i in 0..1024usize {
        if unsafe { core::ptr::read_volatile(heap.add(i)) } != (i & 0xff) as u8 {
            pattern_ok = false;
        }
    }
    println!("hello: heap pattern={}", pattern_ok as u8);
    ok &= pattern_ok;

    // 越出堆窗口（256 KiB）：Linux 语义 → 返回**原值**，调用方据此判断失败
    let over = brk(base + 0x10_0000);
    let refused = over == grown;
    println!("hello: brk over ret={:#x} refused={}", over, refused as u8);
    ok &= refused;

    // --- mmap：匿名两页 → 零 → 模式 → 交给内核写串口 → munmap 归还 ---
    let m = mmap(MMAP_LEN, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS);
    println!("hello: mmap ret={}", m);
    if m <= 0 {
        return false;
    }
    let map = m as usize as *mut u8;
    let map_zeroed = unsafe {
        core::ptr::read_volatile(map).eq(&0) && core::ptr::read_volatile(map.add(MMAP_LEN - 1)).eq(&0)
    };
    println!("hello: mmap zeroed={}", map_zeroed as u8);
    ok &= map_zeroed;
    for i in 0..MMAP_LEN {
        unsafe { core::ptr::write_volatile(map.add(i), (i ^ 0xa5) as u8) };
    }
    let mut map_ok = true;
    for i in 0..MMAP_LEN {
        if unsafe { core::ptr::read_volatile(map.add(i)) } != (i ^ 0xa5) as u8 {
            map_ok = false;
        }
    }
    println!("hello: mmap pattern={}", map_ok as u8);
    ok &= map_ok;

    // 把**堆上的**缓冲区交给内核写出去：证明 `write` 的缓冲校验（按页查 `page_flags`）
    // 认可用户堆页，而不是只认映像窗口与栈。
    let msg = b"heap buffer through write\n";
    unsafe { core::ptr::copy_nonoverlapping(msg.as_ptr(), map, msg.len()) };
    let n = write(1, unsafe { core::slice::from_raw_parts(map, msg.len()) });
    println!("hello: heap write ret={}", n);
    ok &= n == msg.len() as isize;

    let un = munmap(m as u64, MMAP_LEN);
    println!("hello: munmap ret={}", un);
    ok &= un == 0;

    // --- 负例：每个错误码都要具体（不是「都失败就行」） ---
    // 1) 缺 MAP_ANONYMOUS（文件映射未实现）→ EINVAL(-22)
    let file_backed = mmap(4096, PROT_READ, MAP_PRIVATE);
    println!("hello: mmap file-backed ret={}", file_backed);
    ok &= file_backed == -22;
    // 2) MAP_SHARED 不支持 → EINVAL(-22)
    let shared = mmap(4096, PROT_READ, 0x1 | MAP_ANONYMOUS);
    println!("hello: mmap shared ret={}", shared);
    ok &= shared == -22;
    // 3) 超过 mmap 窗口 → ENOMEM(-12)
    let too_big = mmap(MMAP_MAX + 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS);
    println!("hello: mmap too big ret={}", too_big);
    ok &= too_big == -12;
    // 4) W^X：可写 + 可执行 → EINVAL(-22)
    let wx = mmap(4096, PROT_WRITE | 0x4, MAP_PRIVATE | MAP_ANONYMOUS);
    println!("hello: mmap wx ret={}", wx);
    ok &= wx == -22;
    // 5) munmap 未页对齐 → EINVAL(-22)
    let misaligned = munmap(m as u64 + 1, MMAP_LEN);
    println!("hello: munmap misaligned ret={}", misaligned);
    ok &= misaligned == -22;
    // 6) 同一段再次 munmap（已经不映射）→ EINVAL(-22)
    let twice = munmap(m as u64, MMAP_LEN);
    println!("hello: munmap twice ret={}", twice);
    ok &= twice == -22;

    // --- 全局分配器：`Vec` 增长（多次分配/重新分配）+ `format!`（String） ---
    let mut v: Vec<u32> = Vec::new();
    for i in 1..=200u32 {
        v.push(i);
    }
    let sum: u32 = v.iter().sum();
    println!("hello: vec len={} sum={}", v.len(), sum);
    ok &= v.len() == 200 && sum == 20100;
    let s = alloc::format!("heap string bytes={}", v.len());
    println!("hello: {}", s);
    ok &= s.len() == 21;

    println!("hello: heap PASS={}", ok as u8);
    ok
}

/// `mprotect` 自测（v1.0 第一步）：`PROT_NONE` 关页 → 内核拒收该缓冲 → 恢复后内容原样
/// → 换成 `RX` 并**真的执行**页里的机器码（W^X 的「先写后换只执行」）。
///
/// 每一类拒绝都给**具体**退出码（与 `mmap`/`munmap` 的负例同一风格）：
/// 未页对齐 / `len=0` / 未知保护位 / `W^X` / 未映射的页 / 用户栈（保护属性由内核定死）。
fn mprotect_selftest() -> bool {
    let mut ok = true;

    // 一页 RW：写下机器码 —— 此刻这页还不能执行
    let m = mmap(4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS);
    println!("hello: mprotect mmap ret={}", m);
    if m <= 0 {
        return false;
    }
    let page = m as usize;
    let ptr = page as *mut u8;
    unsafe { core::ptr::copy_nonoverlapping(WX_CODE.as_ptr(), ptr, WX_CODE.len()) };

    // 1) PROT_NONE：页帧与内容都还在，但本进程不可访问；内核也不再把这段当合法用户缓冲
    //    （用 `raw` 直接发 syscall，避免在不可读的页上构造 Rust 切片）
    let none = mprotect(page as u64, 4096, PROT_NONE);
    println!("hello: mprotect none ret={}", none);
    ok &= none == 0;
    let refused = unsafe { raw(SYS_WRITE, 1, page as u64, 1, 0, 0) };
    println!("hello: mprotect none write ret={}", refused);
    ok &= refused == -14; // EFAULT：按页校验认定为「不是用户缓冲」

    // 2) 改回 RW：内容必须原样（mprotect 不搬家、不清零）
    let back = mprotect(page as u64, 4096, PROT_READ | PROT_WRITE);
    println!("hello: mprotect rw ret={}", back);
    ok &= back == 0;
    let kept = unsafe { core::ptr::read_volatile(ptr) } == WX_CODE[0];
    println!("hello: mprotect data kept={}", kept as u8);
    ok &= kept;

    // 3) 换成 RX 并真的调用：JIT 走的就是这条路
    let rx = mprotect(page as u64, 4096, PROT_READ | PROT_EXEC);
    println!("hello: mprotect rx ret={}", rx);
    ok &= rx == 0;
    let entry: extern "C" fn() -> u32 = unsafe { core::mem::transmute(page) };
    let ret = entry();
    println!("hello: mprotect exec ret={}", ret);
    ok &= ret == 42;

    // 4) 负例：每一类拒绝都要具体
    let misaligned = mprotect(page as u64 + 8, 4096, PROT_READ);
    println!("hello: mprotect misaligned ret={}", misaligned);
    ok &= misaligned == -22;
    let len0 = mprotect(page as u64, 0, PROT_READ);
    println!("hello: mprotect len0 ret={}", len0);
    ok &= len0 == -22;
    let unknown = mprotect(page as u64, 4096, 0x8);
    println!("hello: mprotect unknown prot ret={}", unknown);
    ok &= unknown == -22;
    let wx = mprotect(page as u64, 4096, PROT_WRITE | PROT_EXEC);
    println!("hello: mprotect wx ret={}", wx);
    ok &= wx == -22;
    // 邻页没有映射（本程序只映射了这一页）→ 不做事，而不是「部分成功」
    let unmapped = mprotect(page as u64 + 4096, 4096, PROT_READ);
    println!("hello: mprotect unmapped ret={}", unmapped);
    ok &= unmapped == -22;
    // 用户栈：保护属性（RW + NX）由内核在建立地址空间时定死，不在支持范围
    let local = 0u8;
    let stack_page = (&local as *const u8 as u64) & !0xfff;
    let stack = mprotect(stack_page, 4096, PROT_READ);
    println!("hello: mprotect stack ret={}", stack);
    ok &= stack == -22;

    // 5) brk 堆窗口也在支持范围内（逐页标志位 `heap_flags` 的意义）——
    //    heap_selftest 增长了两页，所以当前 break 减 HEAP_STEP 就是堆的第一页
    let heap_page = brk(0) - HEAP_STEP;
    let heap_none = mprotect(heap_page, 4096, PROT_NONE);
    println!("hello: mprotect heap none ret={}", heap_none);
    ok &= heap_none == 0;
    let heap_refused = unsafe { raw(SYS_WRITE, 1, heap_page, 1, 0, 0) };
    println!("hello: mprotect heap none write ret={}", heap_refused);
    ok &= heap_refused == -14;
    let heap_back = mprotect(heap_page, 4096, PROT_READ | PROT_WRITE);
    println!("hello: mprotect heap back ret={}", heap_back);
    ok &= heap_back == 0;
    // 堆里第一页的第一个字节是 heap_selftest 写的 `0 & 0xff`
    let heap_kept = unsafe { core::ptr::read_volatile(heap_page as *const u8) } == 0;
    println!("hello: mprotect heap data kept={}", heap_kept as u8);
    ok &= heap_kept;

    // 归还：`mprotect` 过（含 PROT_NONE）的页帧仍在，munmap 必须成功
    let un = munmap(page as u64, 4096);
    println!("hello: mprotect munmap ret={}", un);
    ok &= un == 0;

    println!("hello: mprotect PASS={}", ok as u8);
    ok
}
