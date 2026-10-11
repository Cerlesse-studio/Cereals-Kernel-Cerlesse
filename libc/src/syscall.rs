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
//! v0.9 第一步加入用户堆（`mmap` / `munmap` / `brk`，内核侧 `kernel/src/syscall/memory.rs`）：
//!
//! - `mmap(len, prot, flags)` 只支持**私有匿名**映射（`MAP_PRIVATE|MAP_ANONYMOUS`），
//!   且**没有 offset 参数** —— 内核的 syscall ABI 传 5 个参数（`rdi, rsi, rdx, r10, r8`），
//!   匿名映射不需要偏移；文件映射要先扩 ABI（见内核模块文档）；
//! - `PROT_WRITE|PROT_EXEC` 被内核拒绝（W^X），`PROT_NONE` 与 `MAP_FIXED` 同样拒绝
//!   （需要 `mprotect` / 固定地址放置，两者都还没实现）；
//! - `brk` 沿用 Linux 语义：**不返回错误**，失败时返回原值（用 `ret < addr` 判断）。
//!
//! v0.9 第三步加入了进程管理（内核侧 `kernel/src/syscall/process.rs`）：
//!
//! - `fork()`：父进程返回子进程 PID、子进程返回 0；子进程从**同一条用户指令**
//!   继续执行（内核克隆了 `fork` 那一刻的完整寄存器帧）；
//! - `execve(path, argv)`：换映像，成功后**不返回**；`argv` 用「指针 + 长度」对数组传
//!   （内核逐段校验后写进新进程的用户栈，见 `libc::args`）；
//! - `wait4(pid)`：运行并回收直接子进程，返回退出码。
//!
//! 仍未实现的号只剩 `mprotect`（W^X 转换用），它在内核侧落在公共 ENOSYS 桩上。
//!
//! 与 Linux 的调度差异：本内核单 CPU、用户态无抢占，子进程不会与父进程并发运行，
//! 它要等父进程 `wait4` 才被恢复 —— 标准写法 `if pid == 0 { execve } else { wait4 }`
//! 因此原样可用（详见 `docs/process.md`）。

/// 系统调用号（与内核表一致；不要用魔法数调用 `raw`）。
pub const SYS_READ: u64 = 0;
pub const SYS_WRITE: u64 = 1;
pub const SYS_OPEN: u64 = 2;
pub const SYS_CLOSE: u64 = 3;
pub const SYS_MMAP: u64 = 9;
pub const SYS_MPROTECT: u64 = 10;
pub const SYS_MUNMAP: u64 = 11;
pub const SYS_BRK: u64 = 12;
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
/// `-EAGAIN`：本次没有数据可用（非阻塞语义，不是错误）。
///
/// v0.9 只有 `read(0)` 会返回它：1.5 秒内键盘没有交付一整行时
/// （内核在无输入时不会无限等待，理由见 `kernel/src/syscall/file.rs::read_stdin`）。
pub const EAGAIN: isize = -11;
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
/// `-ENOSYS`：号合法但内核尚未实现（v0.9 起只剩 `mprotect`）。
pub const ENOSYS: isize = -38;
/// `-ECHILD`：`wait4` 找不到该 PID 的直接子进程。
pub const ECHILD: isize = -10;

/// `mmap` 保护位：可读。
pub const PROT_READ: i32 = 0x1;
/// `mmap` 保护位：可写。
pub const PROT_WRITE: i32 = 0x2;
/// `mmap` 保护位：可执行（**不能**与 [`PROT_WRITE`] 同时使用：内核强制 W^X）。
pub const PROT_EXEC: i32 = 0x4;

/// `mmap` 标志：私有映射（与 [`MAP_ANONYMOUS`] 一起是最小可用组合）。
pub const MAP_PRIVATE: i32 = 0x2;
/// `mmap` 标志：匿名映射（本内核**只**支持它，且要求 `fd = -1`）。
pub const MAP_ANONYMOUS: i32 = 0x20;

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

/// `brk(addr)`：调整用户堆顶（内核的 brk 堆窗口 256 KiB）。
///
/// `addr = 0` 只查询当前堆顶。返回当前 break —— **失败时返回原值**（Linux 语义），
/// 调用方用 `ret >= addr` 判断成功；`addr` 会被内核向上取整到页。
pub fn brk(addr: u64) -> u64 {
    let ret = unsafe { raw(SYS_BRK, addr, 0, 0, 0, 0) };
    ret as u64
}

/// `mmap(len, prot, flags)`：匿名映射 `len` 字节，返回页对齐地址或负 errno。
///
/// 内核只接受 `MAP_PRIVATE|MAP_ANONYMOUS`（`fd` 由本函数固定传 -1）、
/// 拒绝 `PROT_WRITE|PROT_EXEC`（W^X）与未知保护位；新页保证**全零**。
pub fn mmap(len: usize, prot: i32, flags: i32) -> isize {
    unsafe {
        raw(
            SYS_MMAP,
            0, // addr 只是提示：内核忽略它（不支持 MAP_FIXED）
            len as u64,
            prot as u64,
            flags as u64,
            u64::MAX, // fd = -1
        )
    }
}

/// `munmap(addr, len)`：整段归还匿名映射（要求页对齐且全段已映射）。
pub fn munmap(addr: u64, len: usize) -> isize {
    unsafe { raw(SYS_MUNMAP, addr, len as u64, 0, 0, 0) }
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

/// `execve` 的 `argv` 项数上限（与内核 `syscall::process::ARG_MAX` 一致）。
pub const ARG_MAX: usize = 8;

/// `fork()`：复制当前进程。
///
/// - **父进程**：返回子进程 PID（> 0）；
/// - **子进程**：返回 0，并从**同一条用户指令**继续执行（内核恢复了
///   `fork` 那一刻的完整寄存器现场，只把 `rax` 改成 0）；
/// - 失败：返回负 errno（进程表满 → [`EAGAIN`]，地址空间复制失败 → [`ENOMEM`]）。
///
/// 本内核单 CPU、用户态无抢占：子进程**不会与父进程并发运行**，它要等父进程
/// `wait4` 才被恢复。对这段调用代码来说两者没有区别 —— 标准的
/// `if pid == 0 { execve(..) } else { wait4(pid) }` 原样可用。
pub fn fork() -> isize {
    unsafe { raw(SYS_FORK, 0, 0, 0, 0, 0) }
}

/// `execve(path, argv)`：用 `path` 的映像替换当前进程。
///
/// **成功后不返回**（内核换 CR3 + 恢复新入口的帧）；失败返回负 errno，调用方
/// 通常立刻 [`exit`]（Shell 的 `cmd_external` 就是这么写的）。
///
/// `argv` 用「指针 + 长度」对数组传递（与 `open`/`write` 的路径同一种风格）：
/// 内核逐段校验后把字符串与指针数组写进新进程的用户栈。用户态没有堆也能用 ——
/// 这里的临时数组在栈上。
pub fn execve(path: &str, argv: &[&str]) -> isize {
    let n = argv.len().min(ARG_MAX);
    let mut pairs = [0u64; ARG_MAX * 2];
    for (i, arg) in argv.iter().take(n).enumerate() {
        pairs[i * 2] = arg.as_ptr() as u64;
        pairs[i * 2 + 1] = arg.len() as u64;
    }
    unsafe {
        raw(
            SYS_EXEC,
            path.as_ptr() as u64,
            path.len() as u64,
            pairs.as_ptr() as u64,
            n as u64,
            0,
        )
    }
}

/// `wait4(pid)`：运行并回收直接子进程，返回它的退出码（负数 = errno）。
///
/// 找不到该 PID 的直接子进程返回 [`ECHILD`]；重复 `wait4` 同一个 PID 返回
/// 同一个退出码（内核已把它回收成僵尸后立刻释放，第二次会得到 [`ECHILD`]）。
pub fn wait4(pid: i64) -> isize {
    unsafe { raw(SYS_WAIT, pid as u64, 0, 0, 0, 0) }
}

/// `exit(code)`：结束当前进程并把退出码交回内核。
///
/// 顶层程序（`/init`、`/bin/sh`）结束后控制权回到内核运行驱动；
/// 子进程结束后回到父进程的 [`wait4`]。
pub fn exit(code: i32) -> ! {
    unsafe {
        raw(SYS_EXIT, code as u64, 0, 0, 0, 0);
    }
    // 内核的 exit 路径不会回到用户态（它恢复父进程/驱动方的内核现场）；
    // 真回到这里说明 ABI 被破坏了，用 `ud2` 立刻显形而不是继续跑。
    unsafe { core::arch::asm!("ud2", options(noreturn)) }
}
