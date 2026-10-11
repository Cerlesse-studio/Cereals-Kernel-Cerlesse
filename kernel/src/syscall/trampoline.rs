//! 用户态 ↔ 内核态边界（v0.6 实现）
//!
//! 本模块提供 v0.6 用户态路径的全部汇编入口：
//!
//! 1. [`syscall_entry`]：`IA32_LSTAR` 指向的 `syscall` 指令入口。`syscall` 只保存
//!    RIP→RCX、RFLAGS→R11，**不切换栈**，因此入口第一件事是把用户 RSP 存起来、
//!    换到专用的 syscall 内核栈，然后再调用 Rust 分发函数。
//! 2. [`enter_user`]：内核态 → ring3 的最初进入（构造 `iretq` 帧）。
//! 3. [`leave_user`]：从用户态回到内核调用者（`sys_exit` / ring3 异常终止路径）。
//!
//! 约定（与 `syscall/mod.rs` 一致）：`rax` = syscall 号，`rdi/rsi/rdx/r10/r8` =
//! 第 1..5 参数，返回值在 `rax`。
//!
//! **寄存器保存规则（与 Linux syscall ABI 一致，用户态可以依赖）**：一次 `syscall`
//! 往返后，除 `rax`（返回值）、`rcx`（被指令写成用户 RIP）、`r11`（用户 RFLAGS）
//! 之外，全部通用寄存器保持进入前的值。这条规则不是“顺手为之”：Rust 编译器在用户态
//! 会把活跃值放在任意 caller-saved 寄存器（实测踩中 `r9`），不恢复就会在 `sysretq`
//! 之后写到一个垃圾地址（v0.7 实测 `cr2=0x105` 的 ring3 #PF）。
//!
//! 栈模型：
//! - 用户态执行期间的中断/异常由 CPU 依据 `TSS.RSP0` 切到**陷入栈**（`TRAP_STACK`）；
//! - `syscall` 指令切到**系统调用栈**（`SYSCALL_STACK`）。
//!   两条栈分开，避免中断在系统调用处理期间压入同一条栈造成覆盖。

use core::arch::global_asm;

/// 系统调用专用内核栈大小
pub const SYSCALL_STACK_SIZE: usize = 16 * 1024;
/// ring3 中断/异常陷入栈大小
pub const TRAP_STACK_SIZE: usize = 16 * 1024;

#[repr(align(16))]
struct SyscallStack([u8; SYSCALL_STACK_SIZE]);
static mut SYSCALL_STACK: SyscallStack = SyscallStack([0; SYSCALL_STACK_SIZE]);

#[repr(align(16))]
struct TrapStack([u8; TRAP_STACK_SIZE]);
static mut TRAP_STACK: TrapStack = TrapStack([0; TRAP_STACK_SIZE]);

// 以下静态量供汇编直接以 `[rip + SYMBOL]` 访问，因此必须 `#[no_mangle]`。
/// `syscall` 进入时保存的用户 RSP
#[no_mangle]
pub static mut CER_USER_RSP: u64 = 0;
/// `enter_user` 保存的内核 RSP（`leave_user` 据此恢复内核调用栈）
#[no_mangle]
pub static mut CER_KERNEL_RSP: u64 = 0;
/// `enter_user` 之后内核继续执行的地址（`leave_user` 的跳转目标）
#[no_mangle]
pub static mut CER_KERNEL_RET: u64 = 0;
/// 当前用户地址空间指针（作为 `sys_enter` 的 `space` 参数传入）
#[no_mangle]
pub static mut CER_USER_SPACE: u64 = 0;
/// 系统调用内核栈顶
#[no_mangle]
pub static mut CER_SYSCALL_STACK_TOP: u64 = 0;
/// 中断/异常陷入栈顶（写入 `TSS.RSP0`）
#[no_mangle]
pub static mut CER_TRAP_STACK_TOP: u64 = 0;

global_asm!(
    // ---------------------------------------------------------------------
    // syscall 入口（IA32_LSTAR）
    // ---------------------------------------------------------------------
    // 进入时：rcx = 用户 RIP，r11 = 用户 RFLAGS，rsp 仍是用户栈。
    //
    // 栈帧（自上而下 push，栈顶 = CER_SYSCALL_STACK_TOP）：
    //
    //   top-8   rcx（用户 RIP）      ┐ 最后 sysret 用
    //   top-16  r11（用户 RFLAGS）   ┘
    //   top-24  rax（syscall 号）    ← 返回值最后写在 rax，这个副本只用于取号
    //   top-32  r9                   ┐
    //   top-40  r8                   │
    //   top-48  r10                  │ 进入前保存、返回前恢复
    //   top-56  rdx                  │（SysV 里它们是 caller-saved，
    //   top-64  rsi                  │  Rust 分发函数会把它们改掉）
    //   top-72  rdi                  ┘  ← rsp（= &args[0]）
    //
    // **为什么连 r9 也要存**：SysV 约定下 caller-saved 的寄存器（rax/rcx/rdx/rsi/rdi/
    // r8/r9/r10/r11）在 `call syscall_dispatch` 之后值不再被保证。用户态的编译器
    // 却把它们当作**跨 syscall 存活**（它只知道 `rcx`/`r11`/`rax` 会被 `syscall`
    // 指令破坏）。v0.7 实测踩中过一次：`Writer::flush` 把自身指针留在 `r9` 上，
    // syscall 返回后 `movq $0, 0x100(%r9)` 写到了 `cr2=0x105` → ring3 #PF。
    // 因此这里按 Linux syscall ABI 的语义收口：**除 rax/rcx/r11 外全部寄存器原样返回**。
    ".global syscall_entry",
    "syscall_entry:",
    "mov [rip + CER_USER_RSP], rsp",
    "mov rsp, [rip + CER_SYSCALL_STACK_TOP]",
    "push rcx",
    "push r11",
    "push rax",
    "push r9",
    "push r8",
    "push r10",
    "push rdx",
    "push rsi",
    "push rdi",
    "mov rsi, rsp",              // rsi = &args[0]（rdi, rsi, rdx, r10, r8 连续）
    "mov rdi, [rsp + 48]",       // rdi = syscall 号（rax 在 top-24 = rsp+48）
    "mov rdx, [rip + CER_USER_SPACE]",
    "and rsp, -16",              // SysV：call 前 16 字节对齐
    "call syscall_dispatch",
    "mov rsp, [rip + CER_SYSCALL_STACK_TOP]", // 丢弃对齐偏移，回到栈顶
    "sub rsp, 72",               // 站到 rdi 槽位（top-72）
    // 依次弹回进入前的 rdi/rsi/rdx/r10/r8/r9；rax 不动（返回值已在其中）
    "pop rdi",
    "pop rsi",
    "pop rdx",
    "pop r10",
    "pop r8",
    "pop r9",
    "add rsp, 8",                // 跳过保存的号（top-24）
    "pop r11",                   // 用户 RFLAGS
    "pop rcx",                   // 用户 RIP
    "mov rsp, [rip + CER_USER_RSP]",
    "sysretq",
    "\n",

    // ---------------------------------------------------------------------
    // 进入用户态（内核 → ring3）
    // ---------------------------------------------------------------------
    // rdi = 用户入口 rip，rsi = 用户栈顶
    ".global enter_user",
    "enter_user:",
    "mov [rip + CER_KERNEL_RSP], rsp",
    "lea rax, [rip + cer_user_resume]",
    "mov [rip + CER_KERNEL_RET], rax",
    "push 0x1b",                 // SS = 用户数据段 | RPL3
    "push rsi",                  // RSP = 用户栈顶
    "push 0x202",                // RFLAGS：IF=1
    "push 0x23",                 // CS = 用户代码段 | RPL3
    "push rdi",                  // RIP = 用户入口
    "iretq",
    "cer_user_resume:",
    "ret",                       // 用户退出后回到 Rust 调用者
    "\n",

    // ---------------------------------------------------------------------
    // 从用户态返回内核（不返回用户态）
    // ---------------------------------------------------------------------
    ".global leave_user",
    "leave_user:",
    "mov rsp, [rip + CER_KERNEL_RSP]",
    "push qword ptr [rip + CER_KERNEL_RET]",
    "sti",                       // 恢复内核中断使能（FMASK 已清 IF）
    "ret",                       // → cer_user_resume → ret 回到 Rust 调用者
    "\n",
);

// 汇编里 `enter_user` 用的是立即数 0x1b / 0x23（用户数据段/代码段 | RPL3）。
// 这里把两个字面量与 GDT 常量绑定，任何一侧改动都会在编译期被发现，
// 而不是等到 ring3 返回时以 #GP 的形式炸在运行期。
const _: () = assert!(crate::arch::x86_64::gdt::USER_DATA_RPL3 == 0x1b);
const _: () = assert!(crate::arch::x86_64::gdt::USER_CODE_RPL3 == 0x23);

extern "C" {
    /// `syscall` 指令入口（地址写入 IA32_LSTAR）。
    fn syscall_entry();
    /// 进入 ring3；在被 [`leave_user`] 拉回内核前不返回。
    fn enter_user(entry: u64, stack_top: u64);
    /// 从用户态返回内核调用者。
    fn leave_user() -> !;
}

/// 初始化两条内核栈并记录其栈顶（不可在用户态运行期间再次调用）。
pub fn init_stacks() {
    unsafe {
        CER_SYSCALL_STACK_TOP =
            core::ptr::addr_of!(SYSCALL_STACK.0) as u64 + SYSCALL_STACK_SIZE as u64;
        CER_TRAP_STACK_TOP = core::ptr::addr_of!(TRAP_STACK.0) as u64 + TRAP_STACK_SIZE as u64;
    }
}

/// `syscall` 指令入口地址（写入 IA32_LSTAR）。
pub fn syscall_entry_addr() -> u64 {
    syscall_entry as *const () as u64
}

/// 中断/异常陷入栈顶（写入 `TSS.RSP0`）。
pub fn trap_stack_top() -> u64 {
    unsafe { CER_TRAP_STACK_TOP }
}

/// 记录当前用户地址空间指针，供 `syscall` 分发时作为 `space` 参数。
///
/// # Safety
/// `space` 必须指向在用户态运行期间持续有效的 `UserSpace`。
pub unsafe fn set_user_space(space: u64) {
    unsafe {
        CER_USER_SPACE = space;
    }
}

/// 进入 ring3 执行用户程序；用户调用 `sys_exit` 或发生 ring3 异常时返回。
///
/// # Safety
/// 调用前必须已完成：`write_cr3(用户页表根)`、`TSS.RSP0 = trap_stack_top()`、
/// `set_user_space(...)`，且 `entry`/`stack_top` 在用户地址空间中已映射。
pub unsafe fn enter(entry: u64, stack_top: u64) {
    unsafe { enter_user(entry, stack_top) }
}

/// 从用户态返回内核调用者（`sys_exit` / ring3 异常终止路径使用）。
///
/// # Safety
/// 只能在 [`enter`] 已经进入用户态之后调用；调用前须已恢复内核 CR3。
pub unsafe fn leave() -> ! {
    unsafe { leave_user() }
}
