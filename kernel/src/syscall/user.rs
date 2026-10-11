//! 内嵌用户程序与用户态运行驱动（v0.6 建立，v0.7 改为「从 VFS 读 ELF 装载」，
//! v0.9 第三步改为「每个程序是一个进程对象」）
//!
//! 加载链（v0.7 起从未改变，v0.9 只在上面挂进程对象）：
//!
//! ```text
//! include_bytes!(user_*.elf)              内核映像里内嵌的用户程序 ELF
//!        │  fs::init（main.rs 调用）
//!        ▼
//! RAMFS /init、/bin/sh、/bin/hello        ← VFS 上的文件
//!        │  vfs::read_all(path)            ← 走挂载表路由 + 文件读
//!        ▼
//! elf::load → UserSpace（按段权限逐页映射）
//!        │  process::create → 进程表登记（独立地址空间 + 独立内核栈 + 用户帧）
//!        ▼
//! trampoline::run（切 CR3 + 恢复用户帧 → ring3）
//!        │
//!        ├─ 内建命令 / 文件 syscall / 用户堆 / 退出码
//!        └─ v0.9：fork / execve / wait4（`/bin/hello` 是被它跑起来的外部程序）
//! ```
//!
//! 四个被运行的程序覆盖四条验收路径：
//!
//! - `/init`（`user/hello`，Rust + `libc`）：写串口、读 `/etc/motd`、写
//!   `/tmp/hello.txt`（内核随后读回并打印）、用户堆自测、`exit(42)`；
//! - `probe`：**隔离探针** —— 位置无关的手写机器码（`global_asm!`），
//!   直接读内核映像地址（0x200_0000），必须被 ring3 #PF 拦下；
//! - `/bin/sh`（`user/shell`）：**交互式 Shell** —— 命令从键盘行缓冲（`read(0)`）来，
//!   内建命令 + **外部程序**（走 `fork`/`execve`/`wait4`）；
//! - `/bin/hello`（`user/hello-bin`）：**外部程序**（本轮新增）—— 由 Shell `fork` +
//!   `execve` 起来，打印自己的 PID 与 `argv`，证明「新进程 + 参数传递」真的发生了。

use core::arch::global_asm;
use core::ptr::addr_of;

use crate::arch::x86_64::{cpu, gdt, paging};
use crate::driver::serial;
use crate::fs::{self, vfs};
use crate::memory::frame;
use crate::process::pid;
use crate::process::process::{self, NO_PROCESS};
use crate::syscall::address_space::{UserSpace, USER_STACK_TOP};
use crate::syscall::trampoline::{self, RunCtx, UserFrame, NO_PROCESS as NO_PROCESS_U64};

/// 内嵌的 `/init`：`user/hello` 的 ELF，由 `kernel/build.rs` 构建并写入
/// `$OUT_DIR/user_hello.elf`（**不再摊平** —— 解析与映射是 `UserSpace::load_elf` 的事）。
static USER_INIT_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/user_hello.elf"));

/// 内嵌的 `/bin/sh`：`user/shell` 的 ELF（v0.9 交互式 Shell）。
static USER_SHELL_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/user_shell.elf"));

/// 内嵌的 `/bin/hello`：`user/hello-bin` 的 ELF（v0.9 第三步的**外部程序**）。
static USER_HELLO_BIN_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/user_hello_bin.elf"));

/// 用户程序在 RAMFS 上的路径。
pub const INIT_PATH: &str = "/init";
/// 交互式 Shell 在 RAMFS 上的路径（`fs::init` 写入）。
pub const SHELL_PATH: &str = "/bin/sh";
/// 外部程序在 RAMFS 上的路径（`fs::init` 写入）。
pub const HELLO_BIN_PATH: &str = "/bin/hello";
/// 用户程序写文件的路径；内核在用户态结束后读回并打印。
pub const USER_FILE: &str = "/tmp/hello.txt";

/// `fs::init` 写进 RAMFS 的用户程序清单（路径 → ELF 字节）。
static PROGRAM_IMAGES: [(&str, &[u8]); 3] = [
    (INIT_PATH, USER_INIT_ELF),
    (SHELL_PATH, USER_SHELL_ELF),
    (HELLO_BIN_PATH, USER_HELLO_BIN_ELF),
];

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

/// 最近一次用户态运行的退出码；`-1` 表示「没有走到 exit」（异常终止）。
///
/// 这是**验收用的镜像变量**：进程自己的退出码在进程表里（`Process::exit_code`），
/// 这里只是把「刚刚结束的那个进程」的结果留给 `selftest` 的断言读。
static mut EXIT_CODE: i64 = -1;

/// 要写进 RAMFS 的用户程序清单（见 [`PROGRAM_IMAGES`]）。
pub fn program_images() -> &'static [(&'static str, &'static [u8])] {
    &PROGRAM_IMAGES
}

/// 记录退出码（由 `syscall::process::sys_exit` / 异常终止路径调用）。
pub fn set_exit_code(code: i64) {
    unsafe {
        EXIT_CODE = code;
    }
}

/// 最近一次用户程序的退出码（未到达 exit 时为 `-1`）。
pub fn last_exit_code() -> i64 {
    unsafe { EXIT_CODE }
}

/// 取一个内嵌汇编程序的字节区间 `[start, end)`。
fn asm_program(start: *const u8, end: *const u8) -> &'static [u8] {
    let len = end as usize - start as usize;
    unsafe { core::slice::from_raw_parts(start, len) }
}

/// 运行一个已装载的进程：登记进程对象、切 CR3/TSS/栈、恢复用户帧。
///
/// 返回时用户程序已经结束（`sys_exit` 或 ring3 异常），进程的地址空间已归还。
///
/// # Safety
/// `space` 必须已经装载好映像；`frame` 必须是同一个 `space` 里的合法入口。
unsafe fn run_process(
    name: &'static str,
    pid: u64,
    source: &'static str,
    space: UserSpace,
    frame: UserFrame,
) -> Result<(), &'static str> {
    let Some(slot) = process::free_slot() else {
        return Err("user: process table full");
    };
    let root = space.root();
    let p = process::create(slot, pid, name, 0, NO_PROCESS).ok_or("user: process slot busy")?;
    p.frame = frame;
    p.space = Some(space);
    // 「谁在等我」= 内核启动上下文（顶层程序退出后回到 `run_process` 的调用者）
    p.return_to = RunCtx {
        krsp: 0, // 由 run_user_frame 的汇编写入
        cr3: paging::current_root(),
        rsp0: gdt::kernel_stack(),
        syscall_top: trampoline::syscall_stack_top(),
        space: 0,
        current: NO_PROCESS_U64,
    };

    let space_ptr = p.space.as_ref().expect("刚写入的地址空间") as *const UserSpace as u64;
    serial::print("user: enter name=");
    serial::print(name);
    serial::print(" pid=");
    serial::print_dec(pid);
    serial::print(" entry=");
    serial::print_hex(frame.rip);
    serial::print(" source=");
    serial::print(source);
    serial::print(" rsp0=");
    serial::print_hex(p.kstack_top());
    serial::println("");

    fs::file::reset();
    unsafe {
        EXIT_CODE = -1;
        process::set_current_index(slot);
        cpu::write_cr3(root);
        gdt::set_kernel_stack(p.kstack_top());
        trampoline::set_syscall_stack_top(p.kstack_top());
        trampoline::set_user_space(space_ptr);
        trampoline::run(
            &p.frame as *const UserFrame,
            &mut p.return_to as *mut RunCtx,
        );
    }

    // 用户态结束：内核侧已由 exit/异常路径恢复成 `return_to` 的快照
    let code = process::process_mut(slot).map(|p| p.exit_code).unwrap_or(-1);
    set_exit_code(code);
    process::release(slot); // 归还地址空间（含堆/mmap 页）
    Ok(())
}

/// 从 VFS 读一个程序、装载、运行（v0.7 起的常规路径）。
fn run_from_vfs(name: &'static str, path: &'static str, pid: u64) -> Result<(), &'static str> {
    let elf = vfs::read_all(path).map_err(|_| "user: cannot read program from VFS")?;
    let mut space = UserSpace::new().map_err(|_| "user: address space allocation failed")?;
    let stats = space.load_elf(&elf).map_err(|_| "user: ELF load failed")?;

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

    // 顶层程序的 argv 就是它自己（`argv[0] = path`）—— 与外部程序走的是同一条路
    let frame = crate::syscall::process::user_frame_for(&space, stats.entry, &[path.as_bytes()])
        .map_err(|_| "user: argv stack build failed")?;
    unsafe { run_process(name, pid, "elf", space, frame) }
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

    let frame = UserFrame::new_user(entry, USER_STACK_TOP, 0, 0);
    unsafe { run_process(name, pid, "asm", space, frame) }
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

/// 验收自测：ELF 从 VFS 装载、ring3 syscall 往返（含用户堆）、用户写入落盘、地址空间隔离、
/// 交互式 Shell 与**外部程序**（v0.9 第三步的 fork/execve/wait4）。
///
/// 最后一项是**页帧泄漏检查**：四次运行的 `UserSpace` 都被释放，`fork` 出来的子进程
/// 在 `wait4` 里回收 —— 帧分配器的 `free_frames` 必须回到运行前的值，否则说明
/// 「某个进程的地址空间没有被归还」。
pub fn selftest() -> Result<(), &'static str> {
    crate::syscall::init();
    // 0 号保留给 `kernel_main`（启动上下文），用户程序从 1 开始编号 ——
    // `fork` 继续往下取号，因此 PID 不会与顶层程序撞车。
    let _kernel_main = pid::alloc();
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

    // 4) 交互式 Shell（v0.9）：`/bin/sh` 从 VFS 装载，命令从键盘行缓冲（stdin）来；
    //    它的外部命令路径会真正跑起 `/bin/hello`（见其源码的 `cmd_external`）。
    run_from_vfs("sh", SHELL_PATH, 3)?;
    let code = last_exit_code();
    if code != 0 {
        serial::print("user: shell FAIL exit=");
        serial::print_dec(code as u64);
        serial::println("");
        return Err("shell did not exit(0)");
    }
    serial::println("user: shell PASS (keyboard line buffer -> stdin -> builtins + external program)");

    // 5) 每个进程的地址空间（含用户堆与 fork 出的子进程）都必须整棵归还
    let free_after = frame::stats().free_frames;
    serial::print("user: frames free before=");
    serial::print_dec(free_before as u64);
    serial::print(" after=");
    serial::print_dec(free_after as u64);
    serial::println("");
    if free_before != free_after {
        serial::println("user: process frame reclamation FAIL (an address space leaked frames)");
        return Err("user address space leaked frames");
    }
    serial::println("user: heap frame reclamation PASS");

    serial::println("user: selftest PASS");
    Ok(())
}
