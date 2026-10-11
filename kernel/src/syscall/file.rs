//! 文件类系统调用（v0.7 实现：`read` / `write` / `open` / `close`）
//!
//! v0.6 只有「字符设备」意义上的 stdout/stderr（fd=1/2 写串口）。v0.7 把 fd 接到
//! VFS 上：
//!
//! | fd | 含义 |
//! | ---- | ---- |
//! | 0 | stdin —— **键盘行缓冲（v0.9）**：交付整行（行尾统一为 `\n`），见 [`read_stdin`] |
//! | 1 / 2 | stdout / stderr —— 写串口（v0.6 行为不变） |
//! | ≥ 3 | RAMFS 文件（由 [`crate::fs::file`] 的 fd 表管理） |
//!
//! stdin 的语义选择（v0.9）：**行缓冲（cooked）**而不是原始字节流 ——
//! 键盘驱动已经做了行编辑（退格、回显），去程交付「一整行 + `\n`」；
//! 用户程序因此不需要自己拼行。没有整行时会等待，但**不会无限等**：
//! 超过 [`STDIN_IDLE_TICKS`] 返回 `-EAGAIN`，否则「无人输入」会让整机
//! 卡在这里（v0.9 还没有可抢占的调度器，见 `docs/syscall.md` 第 3 节）。
//!
//! 两条贯穿所有 handler 的规则：
//!
//! 1. **用户指针先校验再用**。内核不做「尽力而为」的解引用：区间必须落在当前
//!    用户地址空间的已映射页里（`write`/`open` 要求可读，`read` 要求可写），
//!    否则返回 `-EFAULT` 并在串口留痕 —— 这正是隔离生效的证据；
//! 2. **单次 I/O 有上限**（[`IO_MAX`]），避免用户态用一个巨大的 count 让内核
//!    长时间不返回（也避免内核栈上出现大缓冲）。

use core::slice;

use crate::driver::{keyboard, serial};
use crate::fs::file as fdtable;
use crate::fs::vfs;
use crate::syscall::address_space::UserSpace;
use crate::time::clock;

/// 单次 `read`/`write` 允许的最大字节数。
const IO_MAX: u64 = 4096;
/// 路径长度上限（`open` 的 `len` 参数）。
const PATH_MAX: u64 = 256;

/// `-EFAULT`：用户传了不属于自己的地址区间。
///
/// v0.9 第五步起对 `syscall` 其它模块可见：目录类 syscall（`dir.rs`）与
/// 进程类 syscall（`process.rs` 的 `getprocs`）都要做同样的指针校验。
pub const EFAULT: i64 = -14;

/// 校验用户缓冲区间并要求**可读**，转成切片。
///
/// # Safety
/// `buf` 必须来自 ring3，且 `space` 必须是当前生效的用户地址空间。
pub unsafe fn user_bytes<'a>(
    space: *const UserSpace,
    buf: u64,
    count: u64,
    op: &str,
) -> Result<&'a [u8], i64> {
    if space.is_null() {
        return Err(EFAULT);
    }
    let space = unsafe { &*space };
    if !space.valid_user_range(buf, count) {
        return Err(report_efault(op, buf, count, false));
    }
    Ok(unsafe { slice::from_raw_parts(buf as *const u8, count as usize) })
}

/// 校验用户缓冲区间并要求**可写**，转成可变切片。
///
/// # Safety
/// 同 [`user_bytes`]。
pub unsafe fn user_bytes_mut<'a>(
    space: *const UserSpace,
    buf: u64,
    count: u64,
    op: &str,
) -> Result<&'a mut [u8], i64> {
    if space.is_null() {
        return Err(EFAULT);
    }
    let space = unsafe { &*space };
    if !space.valid_user_write_range(buf, count) {
        return Err(report_efault(op, buf, count, true));
    }
    Ok(unsafe { slice::from_raw_parts_mut(buf as *mut u8, count as usize) })
}

/// 越界指针的串口留痕（测试断言这条日志，等于断言「内核真的校验了指针」）。
fn report_efault(op: &str, addr: u64, len: u64, need_write: bool) -> i64 {
    serial::print("user: EFAULT ");
    serial::print(op);
    serial::print(" ptr=");
    serial::print_hex(addr);
    serial::print(" len=");
    serial::print_dec(len);
    serial::println(if need_write {
        " (buffer not writable in user space)"
    } else {
        " (buffer not readable in user space)"
    });
    EFAULT
}

/// `user: write fd=<n> len=<n>`。
fn log_write(fd: i32, n: usize) {
    serial::print("user: write fd=");
    serial::print_dec(fd as u64);
    serial::print(" len=");
    serial::print_dec(n as u64);
    serial::println("");
}

/// `user: read fd=<n> want=<n> got=<n>`。
fn log_read(fd: i32, want: u64, got: usize) {
    serial::print("user: read fd=");
    serial::print_dec(fd as u64);
    serial::print(" want=");
    serial::print_dec(want);
    serial::print(" got=");
    serial::print_dec(got as u64);
    serial::println("");
}

/// `user: <op> fd=<n> errno=<n>`。
fn log_errno(op: &str, fd: i32, err: i64) {
    serial::print("user: ");
    serial::print(op);
    serial::print(" fd=");
    serial::print_dec(fd as u64);
    serial::print(" errno=");
    serial::print_dec(err.unsigned_abs());
    serial::println("");
}

/// stdin（fd=0）等待一个整行的最大 tick 数（PIT 100Hz → 3 秒）。
///
/// 为什么不是阻塞到底：v0.9 仍然是「一次运行一个用户程序、内核在 syscall 里等」
/// 的模型。若 stdin 无限等待，任何没有键盘输入的启动都会永久停在 Shell 的 `read` 上，
/// `user: selftest PASS` 不再出现。返回值 `-EAGAIN` 把「现在没有输入」交给
/// 用户程序决定（Shell 收到两次就自行退出）。进程模型落地后这里会换成真正的阻塞等待。
const STDIN_IDLE_TICKS: u64 = 300;
/// `-EAGAIN`：本次没有整行可用（非阻塞语义的一部分，不是错误）。
const EAGAIN: i64 = -11;

/// stdin（fd=0）：从键盘行缓冲取一整行。
///
/// 与 `sys_sleep` 相同的等待手法：`syscall` 入口用 IA32_FMASK 清了 IF，
/// 等待期间显式 `sti; hlt` 打开中断（IRQ1 才能把字符填进行缓冲）；
/// 取到行后 `cli` 恢复「handler 关中断」的约定再返回。
///
/// 取行本身不需要关中断：`ready` 标志已保证 IRQ1 的写者不会动正在被取走的缓冲
/// （见 `driver::keyboard::LineBuffer`）。
unsafe fn read_stdin(space: *const UserSpace, buf: u64, count: u64) -> i64 {
    let dst = match unsafe { user_bytes_mut(space, buf, count, "read") } {
        Ok(dst) => dst,
        Err(errno) => return errno,
    };

    let deadline = unsafe { clock::tick_count() } + STDIN_IDLE_TICKS;
    loop {
        if let Some(n) = unsafe { keyboard::take_line(dst) } {
            unsafe {
                core::arch::asm!("cli", options(nomem, nostack, preserves_flags));
            }
            log_read(0, count, n);
            return n as i64;
        }
        if unsafe { clock::tick_count() } >= deadline {
            serial::print("user: read fd=0 idle (no line within ");
            serial::print_dec(STDIN_IDLE_TICKS);
            serial::println(" ticks)");
            return EAGAIN;
        }
        unsafe {
            core::arch::asm!("sti", "hlt", options(nomem, nostack, preserves_flags));
        }
    }
}

/// 系统调用 `read(fd, buf, count)`。
pub unsafe fn sys_read(args: &[u64; 5], space: *const UserSpace) -> i64 {
    let fd = args[0] as i32;
    let buf = args[1];
    let count = args[2];

    if count > IO_MAX {
        return vfs::EINVAL;
    }
    if count == 0 {
        return 0;
    }
    if fd == 0 {
        return unsafe { read_stdin(space, buf, count) };
    }
    if fd < 3 {
        // fd 1/2 是只写串口设备（读它们返回 EBADF，而不是「读到空」）
        log_errno("read", fd, vfs::EBADF);
        return vfs::EBADF;
    }

    let dst = match unsafe { user_bytes_mut(space, buf, count, "read") } {
        Ok(dst) => dst,
        Err(errno) => return errno,
    };
    match fdtable::read(fd, dst) {
        Ok(n) => {
            log_read(fd, count, n);
            n as i64
        }
        Err(errno) => {
            log_errno("read", fd, errno);
            errno
        }
    }
}

/// 系统调用 `write(fd, buf, count)`。
pub unsafe fn sys_write(args: &[u64; 5], space: *const UserSpace) -> i64 {
    let fd = args[0] as i32;
    let buf = args[1];
    let count = args[2];

    if count > IO_MAX {
        return vfs::EINVAL;
    }
    if count == 0 {
        return 0;
    }

    let bytes = match unsafe { user_bytes(space, buf, count, "write") } {
        Ok(bytes) => bytes,
        Err(errno) => return errno,
    };

    if fd == 1 || fd == 2 {
        let written = serial::put_bytes(bytes, bytes.len());
        log_write(fd, written);
        return written as i64;
    }
    if fd < 3 {
        log_errno("write", fd, vfs::EBADF);
        return vfs::EBADF;
    }
    match fdtable::write(fd, bytes) {
        Ok(n) => {
            log_write(fd, n);
            n as i64
        }
        Err(errno) => {
            log_errno("write", fd, errno);
            errno
        }
    }
}

/// 系统调用 `open(path, len, flags)`：按路径打开文件并分配 fd。
///
/// 参数里带长度而不是要求 NUL 结尾：用户缓冲已经被内核校验过区间，
/// 用长度就不需要在用户内存里找终止符（少一次不可信内存上的扫描）。
pub unsafe fn sys_open(args: &[u64; 5], space: *const UserSpace) -> i64 {
    let ptr = args[0];
    let len = args[1];
    let flags = args[2] as i32;

    if len == 0 || len > PATH_MAX {
        return vfs::EINVAL;
    }
    let bytes = match unsafe { user_bytes(space, ptr, len, "open") } {
        Ok(bytes) => bytes,
        Err(errno) => return errno,
    };
    let path = match core::str::from_utf8(bytes) {
        Ok(path) => path,
        Err(_) => {
            serial::println("user: open rejected (path not utf-8)");
            return vfs::EINVAL;
        }
    };

    match fdtable::open(path, flags) {
        Ok(fd) => {
            serial::print("user: open path=");
            serial::print(path);
            serial::print(" flags=");
            serial::print_hex(flags as u64);
            serial::print(" fd=");
            serial::print_dec(fd as u64);
            serial::println("");
            fd as i64
        }
        Err(errno) => {
            serial::print("user: open path=");
            serial::print(path);
            serial::print(" errno=");
            serial::print_dec(errno.unsigned_abs());
            serial::println("");
            errno
        }
    }
}

/// 系统调用 `close(fd)`：释放 fd 槽位。
pub unsafe fn sys_close(args: &[u64; 5], _space: *const UserSpace) -> i64 {
    let fd = args[0] as i32;
    match fdtable::close(fd) {
        Ok(()) => {
            serial::print("user: close fd=");
            serial::print_dec(fd as u64);
            serial::println("");
            0
        }
        Err(errno) => {
            log_errno("close", fd, errno);
            errno
        }
    }
}
