//! 内嵌用户程序与 ring3 运行驱动（v0.6 建立，v0.7 改为「从 VFS 读 ELF 装载」）
//!
//! v0.6 的路径是「构建期摊平成连续映像 + `include_bytes!` + 内核原样拷贝」。
//! v0.7 换成了真实的加载链：
//!
//! ```text
//! include_bytes!(user_hello.elf)         内核映像里内嵌的用户程序 ELF
//!        │  fs::init（main.rs 调用）
//!        ▼
//! RAMFS /init                            ← VFS 的第一个文件
//!        │  vfs::read_all("/init")        ← 走挂载表路由 + 文件读
//!        ▼
//! elf::load → UserSpace（按段权限逐页映射）
//!        │  trampoline::enter（切 CR3 + iretq 到 ring3）
//!        ▼
//! ring3 用户程序：write / getpid / open / read / close / write / nanosleep / exit
//! ```
//!
//! 于是「用户程序从文件加载」这件事在 v0.7 是可断言的串口轨迹，而不是隐含在
//! 构建脚本里：`fs: create /init` → `user: program=/init source=elf`。
//!
//! 两个被运行的程序覆盖两条验收路径：
//!
//! - `/init`（`user/hello`，Rust + `libc`）：写串口、读 `/etc/motd`、写
//!   `/tmp/hello.txt`（内核随后读回并打印）、`exit(42)`；
//! - `probe`：**隔离探针** —— 位置无关的手写机器码（`global_asm!`），
//!   直接读内核映像地址（0x200_0000），必须被 ring3 #PF 拦下。
//!
//! 用户态执行期间的内核行为已先做过两处收敛（见 [`enter_ring3`]）：
//! - 调度器被挂起（`scheduler::suspend`，由 `main.rs` 调用），避免 PIT 中断把
//!   内核任务切进「用户页表 + 用户栈」的中间状态；
//! - `TSS.RSP0` 指向专用陷入栈，避免 ring3 中断压到活动的内核调用栈上。

use core::arch::global_asm;
use core::ptr::addr_of;

use crate::arch::x86_64::{cpu, gdt, paging};
use crate::driver::serial;
use crate::fs::{self, vfs};
use crate::memory::frame;
use crate::syscall::address_space::UserSpace;
use crate::syscall::trampoline;

/// 内嵌的 `/init`：`user/hello` 的 ELF，由 `kernel/build.rs` 构建并写入
/// `$OUT_DIR/user_hello.elf`（**不再摊平** —— 解析与映射是 [`UserSpace::load_elf`] 的事）。
static USER_INIT_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/user_hello.elf"));

/// 用户程序在 RAMFS 上的路径。
pub const INIT_PATH: &str = "/init";
/// 用户程序写文件的路径；内核在用户态结束后读回并打印（用户写入确实落到 RAMFS 的证据）。
pub const USER_FILE: &str = "/tmp/hello.txt";

global_asm!(
    ".align 16",
    // ---- probe：越权读取内核映像（0x200_0000）→ 应被 #PF 拦截 ----
    // 位置无关（只用立即数与寄存器间接寻址），因此整段字节可拷到任意用户代码页执行。
    ".global cer_user_probe_start",
    "cer_user_probe_start:",
    "mov rax, 0x2000000",                             // 内核链接地址
    "mov rbx, [rax]",                                 // ring3 读内核页 → #PF
    "mov rax, 60",                                    // 只有隔离失效时才会到这里
    "mov rdi, 7",
    "syscall",
    "ud2",
    ".global cer_user_probe_end",
    "cer_user_probe_end:",
);

extern "C" {
    static cer_user_probe_start: u8;
    static cer_user_probe_end: u8;
}

/// 当前用户程序的 PID（v0.7 一次只跑一个）。
static mut USER_PID: u64 = 0;
/// 最近一次 `sys_exit` 的退出码；`-1` 表示「没有走到 exit」。
static mut EXIT_CODE: i64 = -1;

/// 内嵌的用户程序 ELF（`fs::init` 把它写进 RAMFS 的 `/init`）。
pub fn init_image() -> &'static [u8] {
    USER_INIT_ELF
}

/// 记录 `sys_exit` 的退出码（由 `syscall::process::sys_exit` 调用）。
pub fn set_exit_code(code: i64) {
    unsafe {
        EXIT_CODE = code;
    }
}

/// 最近一次用户程序的退出码（未到达 exit 时为 `-1`）。
pub fn last_exit_code() -> i64 {
    unsafe { EXIT_CODE }
}

/// 当前用户程序 PID（`sys_getpid` 使用）。
pub fn current_pid() -> u64 {
    unsafe { USER_PID }
}

/// 取一个内嵌汇编程序的字节区间 `[start, end)`。
fn asm_program(start: *const u8, end: *const u8) -> &'static [u8] {
    let len = end as usize - start as usize;
    unsafe { core::slice::from_raw_parts(start, len) }
}

/// 在独立用户地址空间 + ring3 中运行已装载的程序。
///
/// `source` 只用于日志：`elf` 表示映像来自 VFS 上的 ELF 文件（走 ELF Loader），
/// `asm` 表示内嵌的手写机器码（隔离探针）—— 让串口日志能区分两条路径。
///
/// # Safety
/// `space` 必须在调用期间持续有效（它在栈上，`Drop` 在返回后执行）。
unsafe fn enter_ring3(name: &'static str, source: &'static str, space: &UserSpace, entry: u64) {
    let kernel_root = paging::current_root();
    let prev_rsp0 = gdt::kernel_stack();
    let space_ptr: *const UserSpace = space;

    serial::print("user: enter name=");
    serial::print(name);
    serial::print(" pid=");
    serial::print_dec(current_pid());
    serial::print(" entry=");
    serial::print_hex(entry);
    serial::print(" source=");
    serial::print(source);
    serial::print(" rsp0=");
    serial::print_hex(trampoline::trap_stack_top());
    serial::println("");

    // SAFETY: 步骤顺序是关键 —— 先换 TSS.RSP0 与用户地址空间指针，再切 CR3，
    // 之后才能 iretq 到 ring3；返回时按相反顺序恢复内核状态。
    unsafe {
        gdt::set_kernel_stack(trampoline::trap_stack_top());
        trampoline::set_user_space(space_ptr as u64);
        cpu::write_cr3(space.root());
        trampoline::enter(entry, space.stack_top());
        cpu::write_cr3(kernel_root);
        gdt::set_kernel_stack(prev_rsp0);
        trampoline::set_user_space(0);
    }

    serial::print("user: back in kernel (name=");
    serial::print(name);
    serial::println(")");
}

/// 从 VFS 读 `/init` 并装载运行（v0.7 的常规路径）。
fn run_from_vfs(name: &'static str, path: &'static str, pid: u64) -> Result<(), &'static str> {
    let elf = vfs::read_all(path).map_err(|_| "user: cannot read program from VFS")?;
    let mut space = UserSpace::new().map_err(|_| "user: address space allocation failed")?;
    let stats = space
        .load_elf(&elf)
        .map_err(|_| "user: ELF load failed")?;

    serial::print("user: program=");
    serial::print(path);
    serial::print(" source=elf bytes=");
    serial::print_dec(stats.bytes);
    serial::print(" segments=");
    serial::print_dec(stats.segments as u64);
    serial::print(" image_pages=");
    serial::print_dec(stats.pages as u64);
    serial::print(" writable_pages=");
    serial::print_dec(stats.writable_pages as u64);
    serial::print(" entry=");
    serial::print_hex(stats.entry);
    serial::println("");

    fs::file::reset();
    unsafe {
        USER_PID = pid;
        EXIT_CODE = -1;
        enter_ring3(name, "elf", &space, stats.entry);
    }
    Ok(())
}

/// 运行内嵌的手写机器码（隔离探针）。
fn run_raw(name: &'static str, code: &'static [u8], pid: u64) -> Result<(), &'static str> {
    let mut space = UserSpace::new().map_err(|_| "user: address space allocation failed")?;
    let entry = space.map_raw(code).map_err(|_| "user: raw program mapping failed")?;

    serial::print("user: program=");
    serial::print(name);
    serial::print(" source=asm bytes=");
    serial::print_dec(code.len() as u64);
    serial::print(" image_pages=");
    serial::print_dec(space.image_pages() as u64);
    serial::print(" entry=");
    serial::print_hex(entry);
    serial::println("");

    fs::file::reset();
    unsafe {
        USER_PID = pid;
        EXIT_CODE = -1;
        enter_ring3(name, "asm", &space, entry);
    }
    Ok(())
}

/// 读回用户程序写的文件（`/tmp/hello.txt`），证明用户态 `write` 真的落到了 RAMFS。
fn verify_user_file() -> Result<(), &'static str> {
    let data = vfs::read_all(USER_FILE)
        .map_err(|_| "user: /tmp/hello.txt missing (user write did not reach RAMFS)")?;
    if data.is_empty() {
        return Err("user: /tmp/hello.txt is empty");
    }
    serial::print("fs: readback ");
    serial::print(USER_FILE);
    serial::print(" len=");
    serial::print_dec(data.len() as u64);
    serial::println("");
    serial::print("fs: user data: ");
    serial::put_bytes(&data, data.len());
    serial::println("");
    Ok(())
}

/// 验收自测：ELF 从 VFS 装载、ring3 syscall 往返（含用户堆）、用户写入落盘、地址空间隔离。
///
/// 最后一项是**页帧泄漏检查**（v0.9 用户堆）：两次运行的 `UserSpace` 都被 `Drop`，
/// 包括 `brk`/`mmap` 分配出去又没有 `munmap` 归还的页 —— 帧分配器的 `free_frames`
/// 必须回到运行前的值，否则说明「用户堆的页帧在地址空间销毁时没还」。
pub fn selftest() -> Result<(), &'static str> {
    crate::syscall::init();
    serial::println("user: selftest begin");
    let free_before = frame::stats().free_frames;

    // 1) 正常路径：/init（ELF 来自 RAMFS）完成一轮完整 syscall 往返并以 exit(42) 结束
    run_from_vfs("init", INIT_PATH, 1)?;
    let code = last_exit_code();
    if code != 42 {
        serial::print("user: init FAIL exit=");
        serial::print_dec(code as u64);
        serial::println("");
        return Err("init did not exit(42)");
    }
    serial::println("user: init PASS (ELF from RAMFS; write/open/read/close/exit code=42)");

    // 2) 用户态写入落到 RAMFS：内核读回同一文件并打印内容
    verify_user_file()?;

    // 3) 隔离路径：用户程序读内核页，必须收到 ring3 #PF 并被终止
    let probe = asm_program(addr_of!(cer_user_probe_start), addr_of!(cer_user_probe_end));
    run_raw("probe", probe, 2)?;
    let code = last_exit_code();
    if code != -1 {
        serial::print("user: probe FAIL exit=");
        serial::print_dec(code as u64);
        serial::println("");
        return Err("probe reached sys_exit: ring3 isolation not enforced");
    }
    serial::println("user: probe PASS (kernel page not reachable from ring3)");

    // 4) 地址空间（含用户堆页）必须整棵归还
    let free_after = frame::stats().free_frames;
    serial::print("user: frames free before=");
    serial::print_dec(free_before as u64);
    serial::print(" after=");
    serial::print_dec(free_after as u64);
    serial::println("");
    if free_before != free_after {
        serial::println("user: heap frame reclamation FAIL (address space leaked frames)");
        return Err("user address space leaked frames");
    }
    serial::println("user: heap frame reclamation PASS");

    serial::println("user: selftest PASS");
    Ok(())
}
