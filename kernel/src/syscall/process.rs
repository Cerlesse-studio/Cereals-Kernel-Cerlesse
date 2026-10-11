//! 进程类系统调用（v0.6 实现）
//!
//! v0.6 的用户态仍是「单用户程序、运行完即回内核」的模型：`sys_exit` 不销毁
//! 进程而是**结束当前用户态执行**（`trampoline::leave`），把控制权交回内核
//! 调用者（`syscall::user::run`）。真正的进程表/地址空间回收留给 v0.7。
//!
//! `space` 是当前用户地址空间（由入口汇编从 `CER_USER_SPACE` 传入）。

use crate::driver::serial;
use crate::syscall::address_space::UserSpace;
use crate::syscall::trampoline;
use crate::time::clock;

/// `nanosleep(secs, rem)` 的最大等待 tick 数；仅作为「PIT 停摆」时的兜底，
/// 避免内核在用户态请求下永久挂住。
const SLEEP_MAX_SPINS: u64 = 10_000;

/// 系统调用 `exit(code)`：记录退出码并结束用户态执行。
///
/// 本函数**不返回**（尾表达式 `leave()` 类型为 `!`），声明 `-> i64` 只是
/// 为了与 [`crate::syscall::syscall_table::SyscallHandler`] 的类型一致。
pub unsafe fn sys_exit(args: &[u64; 5], _space: *const UserSpace) -> i64 {
    let code = args[0] as i64;
    serial::print("user: exit code=");
    serial::print_dec(code as u64);
    serial::println("");
    crate::syscall::user::set_exit_code(code);
    unsafe { trampoline::leave() }
}

/// 系统调用 `getpid`：返回当前用户程序的 PID。
pub unsafe fn sys_getpid(_args: &[u64; 5], _space: *const UserSpace) -> i64 {
    let pid = crate::syscall::user::current_pid();
    serial::print("user: getpid -> ");
    serial::print_dec(pid);
    serial::println("");
    pid as i64
}

/// 系统调用 `nanosleep`：以 PIT tick（100Hz → 10ms）为单位等待 `args[0]` 个 tick。
///
/// 实现要点：`syscall` 入口用 IA32_FMASK 清了 IF，所以内核默认在关中断下运行；
/// 等待期间显式 `sti; hlt` 打开中断并空转，让 PIT 能继续推进 tick。
pub unsafe fn sys_sleep(args: &[u64; 5], _space: *const UserSpace) -> i64 {
    let ticks = args[0].max(1);
    let start = unsafe { clock::tick_count() };
    let deadline = start + ticks;

    serial::print("user: sleep ticks=");
    serial::print_dec(ticks);
    serial::println("");

    let mut spins = 0u64;
    while unsafe { clock::tick_count() } < deadline {
        unsafe {
            core::arch::asm!("sti", "hlt", options(nomem, nostack, preserves_flags));
        }
        spins += 1;
        if spins > SLEEP_MAX_SPINS {
            serial::println("user: sleep aborted (timer not advancing)");
            return -4; // -EINTR
        }
    }

    let elapsed = unsafe { clock::tick_count() } - start;
    serial::print("user: sleep done ticks=");
    serial::print_dec(elapsed);
    serial::println("");
    0
}
