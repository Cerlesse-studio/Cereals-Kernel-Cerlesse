//! 系统调用封装（v0.6 首批：`write` / `getpid` / `nanosleep` / `exit`；
//! v0.7 增加 `open` / `read` / `close`）
//!
//! 约定与内核 `kernel/src/syscall/syscall_table.rs` 完全对齐（Linux x86_64 语义）：
//!
//! | 项 | 取值 |
//! | ---- | ---- |
//! | 指令 | `syscall`（入口 `IA32_LSTAR` → 内核 `syscall_entry`，见 `kernel/src/syscall/trampoline.rs`） |
//! | 号 | `rax` |
//! | 参数 | `rdi, rsi, rdx, r10, r8` |
//! | 返回 | `rax`；负数 = `-errno`（如 `-38` = `ENOSYS`、`-14` = `EFAULT`） |
//!
//! v0.7 的文件 syscall 形状（与 Linux 的差异写在类型里，避免误用）：
//!
//! - `open(path_ptr, path_len, flags)`：路径用**指针 + 长度**而不是 NUL 结尾 ——
//!   内核已经校验过用户缓冲区间，不需要在不可信内存上找终止符；
//! - `read(fd, buf, count)`：`buf` 必须对 ring3 **可写**（内核按页检查）；
//! - `write(fd, buf, count)`：`buf` 必须可读；fd 1/2 写串口，fd ≥ 3 写文件；
//! - `close(fd)`：只接受 fd ≥ 3（0/1/2 是保留位）。
//!
//! 尚未实现的号（`fork/execve/wait4`）在内核侧仍落在公共 ENOSYS 桩上。

/// 系统调用号（与内核表一致；不要用魔法数调用 `raw`）。
pub const SYS_READ: u64 = 0;
pub const SYS_WRITE: u64 = 1;
pub const SYS_OPEN: u64 = 2;
pub const SYS_CLOSE: u64 = 3;
pub const SYS_SLEEP: u64 = 35; // nanosleep
pub const SYS_GETPID: u64 = 39;
pub const SYS_FORK: u64 = 57;
pub const SYS_EXEC: u64 = 59;
pub const SYS_EXIT: u64 = 60;
pub const SYS_WAIT: u64 = 61;

/// 标准输入（v0.7 未接通：`read` 返回 0 = EOF）。
pub const STDIN: i32 = 0;
/// 标准输出（内核把它接到串口）。
pub const STDOUT: i32 = 1;
/// 标准错误（同样接到串口）。
pub const STDERR: i32 = 2;
/// 第一个动态 fd（内核 `fs::file::FD_FIRST_DYNAMIC`）。
pub const FD_FIRST_DYNAMIC: i32 = 3;

/// `open` 标志：只读。
pub const O_RDONLY: i32 = 0;
/// `open` 标志：只写。
pub const O_WRONLY: i32 = 1;
/// `open` 标志：读写。
pub const O_RDWR: i32 = 2;
/// `open` 标志：不存在则创建（父目录必须已存在）。
pub const O_CREAT: i32 = 0o100;

/// `-ENOENT`：路径不存在。
pub const ENOENT: isize = -2;
/// `-EBADF`：fd 无效 / 未打开 / 打开模式与操作不符。
pub const EBADF: isize = -9;
/// `-ENOMEM`：内核内存不足。
pub const ENOMEM: isize = -12;
/// `-EFAULT`：用户传了不属于自己的地址区间。
pub const EFAULT: isize = -14;
/// `-EISDIR`：目标是目录，但调用方要当文件用。
pub const EISDIR: isize = -21;
/// `-EINVAL`：参数非法（含 `flags` 里的未知位、非法路径）。
pub const EINVAL: isize = -22;
/// `-ENOSPC`：空间不足（fd 用尽 / 文件超过 RAMFS 上限）。
pub const ENOSPC: isize = -28;
/// `-ENOSYS`：号合法但内核尚未实现。
pub const ENOSYS: isize = -38;

/// 原始系统调用：`rax` = 号，`rdi/rsi/rdx/r10/r8` = 参数，返回 `rax`。
///
/// **寄存器约定**：与 Linux syscall ABI 一致 —— 一次往返后只有 `rax`（返回值）、
/// `rcx`、`r11` 变化，其余寄存器（包括 `rdi/rsi/rdx/r10/r8`）保持进入前的值。
/// 内核入口汇编为此在调用 Rust 分发函数前保存 `rdi/rsi/rdx/r10/r8/r9`、
/// 返回前逐个恢复（见 `kernel/src/syscall/trampoline.rs` 的栈帧注释）。
///
/// 因此这里用 `in(..)` 而不是 `inout(..) => _`：后者会告诉编译器这些寄存器
/// 被破坏，虽然保守但更悲观。**不要再改回 `inout`** —— 真正需要的是内核侧
/// 恢复寄存器，那样用户代码才能把活跃值留在任意 caller-saved 寄存器里。
///
/// # Safety
/// 这是一个裸系统调用：参数与返回值的语义由内核决定（见模块文档）。
#[inline(always)]
pub unsafe fn raw(nr: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> isize {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") nr => ret,
            in("rdi") a1,
            in("rsi") a2,
            in("rdx") a3,
            in("r10") a4,
            in("r8") a5,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret as isize
}

/// `write(fd, buf, count)`：fd 1/2 写串口，fd ≥ 3 写文件。
///
/// 返回实际写入字节数，负数表示错误（`EBADF` / `EFAULT` / `EINVAL`）。
/// 内核要求缓冲区落在**当前用户地址空间**内（代码页/数据段/用户栈），
/// 否则返回 `EFAULT` 并在串口留下 `user: EFAULT write ...` 记录。
pub fn write(fd: i32, buf: &[u8]) -> isize {
    unsafe { raw(SYS_WRITE, fd as u64, buf.as_ptr() as u64, buf.len() as u64, 0, 0) }
}

/// `read(fd, buf, count)`：从 `fd` 读入 `buf`，返回读到的字节数（0 = EOF）。
///
/// 内核要求 `buf` 对 ring3 **可写**（栈或 `RW` 段），否则 `EFAULT`。
pub fn read(fd: i32, buf: &mut [u8]) -> isize {
    unsafe { raw(SYS_READ, fd as u64, buf.as_ptr() as u64, buf.len() as u64, 0, 0) }
}

/// `open(path, flags)`：打开（必要时创建）文件，返回 fd（≥ 3）或负 errno。
///
/// 路径用「指针 + 长度」传递；`flags` 只允许 [`O_RDONLY`]/[`O_WRONLY`]/[`O_RDWR`]
/// 与 [`O_CREAT`] 的组合，其余位内核返回 [`EINVAL`]。
pub fn open(path: &str, flags: i32) -> isize {
    unsafe {
        raw(
            SYS_OPEN,
            path.as_ptr() as u64,
            path.len() as u64,
            flags as u64,
            0,
            0,
        )
    }
}

/// `close(fd)`：释放 fd（0/1/2 是保留位，关闭它们返回 [`EBADF`]）。
pub fn close(fd: i32) -> isize {
    unsafe { raw(SYS_CLOSE, fd as u64, 0, 0, 0, 0) }
}

/// `getpid()`：当前用户程序的 PID（内核分配的顺序号）。
pub fn getpid() -> u64 {
    unsafe { raw(SYS_GETPID, 0, 0, 0, 0, 0) as u64 }
}

/// `nanosleep(ticks)`：睡眠 `ticks` 个 PIT tick（100Hz → 1 tick = 10ms）。
///
/// 等待期间内核 `sti; hlt`，PIT 继续推进 tick，因此返回值是真实流逝的 tick 数。
pub fn nanosleep(ticks: u64) -> isize {
    unsafe { raw(SYS_SLEEP, ticks, 0, 0, 0, 0) }
}

/// `exit(code)`：结束当前用户态执行并把退出码交回内核。
///
/// v0.7 的语义仍是「结束这次用户程序运行」（内核不销毁进程对象），
/// 调用后控制权回到 `kernel/src/syscall/user.rs::selftest` 的调用者。
pub fn exit(code: i32) -> ! {
    unsafe {
        raw(SYS_EXIT, code as u64, 0, 0, 0, 0);
    }
    // 内核的 exit 路径不会回到用户态（它走 `trampoline::leave`）；
    // 真回到这里说明 ABI 被破坏了，用 `ud2` 立刻显形而不是继续跑。
    unsafe { core::arch::asm!("ud2", options(noreturn)) }
}
