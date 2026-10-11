//! 系统调用号到内核处理函数的表（v0.6）
//!
//! 编号沿用 **Linux x86_64** 语义，便于以后直接跑现成的用户态程序：
//! `read=0, write=1, open=2, close=3, mmap=9, mprotect=10, munmap=11, brk=12,
//! getpid=39, nanosleep=35, fork=57, execve=59, exit=60, wait4=61`。
//!
//! 表按 `SYS_MAX` 排满：未单独实现的号落在公共 ENOSYS 桩上，
//! 因此「号合法但未实现」与「号越界」是两种可区分的失败
//! （前者查表命中桩，后者 `parse` 返回 `None`，两者最终都返回 `-ENOSYS`）。

// 编号表本身就是对外契约：尚未实现的号也必须有名字，否则调用方只能靠魔法数。
#![allow(dead_code)]

use crate::syscall::address_space::UserSpace;

/// 系统调用号（Linux x86_64 语义）
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
/// 系统调用号上限（不含），用于边界检查。
pub const SYS_MAX: u64 = 256;

/// 已定义但尚未实现的系统调用号（逐个补齐）。
///
/// 显式列出而不是「不写」：查表时它们落在 ENOSYS 桩上，
/// 与「号越界」是两种不同的失败原因，调试时不会被混为一谈。
/// v0.7 已把 `read/open/close` 从这张表里摘掉；用户堆（`mmap/munmap/brk`）
/// 见下方 [`SYS_TABLE`]。`mprotect` 是下一个相关的号 —— `PROT_NONE` 的映射、
/// 以及「先写后换成只执行」的 W^X 转换都需要它。
const UNIMPLEMENTED: [u64; 4] = [SYS_MPROTECT, SYS_FORK, SYS_EXEC, SYS_WAIT];

/// 系统调用处理函数类型。`args[0..5]` = `rdi, rsi, rdx, r10, r8`。
pub type SyscallHandler = unsafe fn(args: &[u64; 5], space: *const UserSpace) -> i64;

/// 按系统调用号查表：号越界返回 `None`，否则返回槽位中的 handler
/// （未实现的号命中 [`syscall_enosys`]）。
pub fn parse(nid: u64) -> Option<SyscallHandler> {
    if nid >= SYS_MAX {
        return None;
    }
    Some(SYS_TABLE[nid as usize])
}

/// 公共 ENOSYS 桩（`-ENOSYS` = -38）。
pub(crate) fn syscall_enosys(_args: &[u64; 5], _space: *const UserSpace) -> i64 {
    -38
}

/// 系统调用表。已注册的号在 `const` 求值期替换为具体 handler。
pub const SYS_TABLE: [SyscallHandler; SYS_MAX as usize] = {
    let mut table: [SyscallHandler; SYS_MAX as usize] = [syscall_enosys; SYS_MAX as usize];
    table[SYS_READ as usize] = crate::syscall::file::sys_read;
    table[SYS_WRITE as usize] = crate::syscall::file::sys_write;
    table[SYS_OPEN as usize] = crate::syscall::file::sys_open;
    table[SYS_CLOSE as usize] = crate::syscall::file::sys_close;
    table[SYS_MMAP as usize] = crate::syscall::memory::sys_mmap;
    table[SYS_MUNMAP as usize] = crate::syscall::memory::sys_munmap;
    table[SYS_BRK as usize] = crate::syscall::memory::sys_brk;
    table[SYS_GETPID as usize] = crate::syscall::process::sys_getpid;
    table[SYS_SLEEP as usize] = crate::syscall::process::sys_sleep;
    table[SYS_EXIT as usize] = crate::syscall::process::sys_exit;
    table
};
