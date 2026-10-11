//! GDT 全局描述符表 + TSS（v0.2 实现；v0.6 调整为 syscall/sysret 布局）
//!
//! 布局（v0.6 起，顺序由 IA32_STAR 决定，不可随意调整）：
//!
//! ```text
//! 0x00 null | 0x08 内核代码 | 0x10 内核数据 | 0x18 用户数据 | 0x20 用户代码
//! 0x28 TSS（16 字节描述符，占 5-6 两项）
//! ```
//!
//! - `syscall`：CS = STAR[47:32] = 0x08，SS = CS + 8 = 0x10；
//! - `sysret` ：STAR[63:48] = 0x10 → SS = base + 8 = 0x18，CS = base + 16 = 0x20。
//!
//! 因此「用户数据紧邻用户代码之前」是硬约束：两者的相对位置错了，
//! `sysret` 会把 SS/CS 加载成错误选择子（表现为返回用户态即 #GP）。
//!
//! TSS 提供 IST1 专用双错栈与 RSP0（ring3 → ring0 的中断/异常切入栈）。

use core::arch::asm;
use core::mem::size_of;
use core::ptr::{addr_of, addr_of_mut};

/// 内核代码段选择子（index 1）
pub const KERNEL_CODE: u16 = 0x08;
/// 内核数据段选择子（index 2）
pub const KERNEL_DATA: u16 = 0x10;
/// 用户数据段选择子（index 3）
pub const USER_DATA: u16 = 0x18;
/// 用户代码段选择子（index 4）
pub const USER_CODE: u16 = 0x20;
/// TSS 选择子（index 5）
pub const TSS_SEL: u16 = 0x28;

/// 带 RPL=3 的选择子（`iretq` 到 ring3 时使用）。
pub const USER_DATA_RPL3: u16 = USER_DATA | 3;
/// 带 RPL=3 的用户代码选择子。
pub const USER_CODE_RPL3: u16 = USER_CODE | 3;

/// `sysret` 的用户基址（IA32_STAR[63:48]）：`sysret` 会取 base+8 作 SS、
/// base+16 作 CS，故 base 取用户数据段前一项（0x10 → SS=0x18, CS=0x20）。
pub const SYSRET_USER_BASE: u64 = 0x10;

// 内核栈顶（crate 根 `main.rs` 的 `_start` 汇编定义，`.global` 导出）
extern "C" {
    #[link_name = "stack_top"]
    static STACK_TOP: u8;
}

/// 64 位任务状态段
#[repr(C, packed)]
struct TaskStateSegment {
    reserved0: u32,
    rsp: [u64; 3],
    reserved1: u64,
    ist: [u64; 7],
    reserved2: u64,
    reserved3: u16,
    iomap_base: u16,
}

/// 双错专用栈（IST1）：内核栈损坏时 #DF 仍能进入 handler
const DF_STACK_SIZE: usize = 16 * 1024;
#[repr(align(16))]
struct Stack([u8; DF_STACK_SIZE]);
static mut DOUBLE_FAULT_STACK: Stack = Stack([0; DF_STACK_SIZE]);

static mut TSS: TaskStateSegment = TaskStateSegment {
    reserved0: 0,
    rsp: [0; 3],
    reserved1: 0,
    ist: [0; 7],
    reserved2: 0,
    reserved3: 0,
    iomap_base: size_of::<TaskStateSegment>() as u16, // 无 I/O 位图
};

#[repr(C, align(16))]
struct Gdt {
    null: u64,
    kcode: u64,
    kdata: u64,
    udata: u64,
    ucode: u64,
    tss_low: u64,
    tss_high: u64,
}

static mut GDT: Gdt = Gdt {
    null: 0,
    kcode: 0,
    kdata: 0,
    udata: 0,
    ucode: 0,
    tss_low: 0,
    tss_high: 0,
};

#[repr(C, packed)]
struct GdtPointer {
    limit: u16,
    base: u64,
}

/// TSS 描述符本体（低 8 字节）
fn tss_desc_low(base: u64, limit: u32) -> u64 {
    (limit as u64 & 0xFFFF)
        | ((base & 0xFFFF) << 16)
        | (((base >> 16) & 0xFF) << 32)
        | (0x89 << 40)
        | (((limit as u64 >> 16) & 0xF) << 48)
        | (((base >> 24) & 0xFF) << 56)
}

/// 加载自有 GDT，重载 CS/DS/SS/ES，加载 TSS。
pub fn init() {
    unsafe {
        // 1. 填充 TSS：IST1 = 双错栈顶；RSP0 = 内核陷入栈（默认先用启动栈，
        //    v0.6 进入用户态前会由 `set_kernel_stack` 换成专用陷入栈）。
        let tss = &mut *addr_of_mut!(TSS);
        tss.ist[0] = addr_of!(DOUBLE_FAULT_STACK.0) as u64 + DF_STACK_SIZE as u64;
        tss.rsp[0] = addr_of!(STACK_TOP) as u64;

        // 2. 填充 GDT（顺序见模块头注释：udata 必须紧跟 ucode 之前）
        let tss_low = tss_desc_low(addr_of!(TSS) as u64, (size_of::<TaskStateSegment>() - 1) as u32);
        let tss_high = (addr_of!(TSS) as u64 >> 32) & 0xFFFF_FFFF;
        let gdt = &mut *addr_of_mut!(GDT);
        gdt.null = 0;
        gdt.kcode = 0x00AF_9A00_0000_FFFF; // 64 位代码段：P, DPL0, 可执行/可读, L=1
        gdt.kdata = 0x00CF_9200_0000_FFFF; // 数据段：P, DPL0, 可写, G=1
        gdt.udata = 0x00CF_F200_0000_FFFF; // 用户数据：P, DPL3, 可写, G=1
        gdt.ucode = 0x00AF_FA00_0000_FFFF; // 用户代码：P, DPL3, 可执行/可读, L=1
        gdt.tss_low = tss_low;
        gdt.tss_high = tss_high;

        // 3. lgdt
        let ptr = GdtPointer {
            limit: (size_of::<Gdt>() - 1) as u16,
            base: addr_of!(GDT) as u64,
        };
        asm!("lgdt [{}]", in(reg) &ptr, options(nostack, preserves_flags));

        // 4. 远返回重载 CS = 0x08
        asm!(
            "push {sel}",
            "lea {tmp}, [rip + 2f]",
            "push {tmp}",
            "retfq",
            "2:",
            sel = in(reg) KERNEL_CODE as u64,
            tmp = lateout(reg) _,
            options(preserves_flags),
        );

        // 5. 重载数据段
        asm!(
            "mov ds, {0:x}",
            "mov es, {0:x}",
            "mov ss, {0:x}",
            in(reg) KERNEL_DATA,
            options(nostack, preserves_flags),
        );

        // 6. ltr 加载 TSS（CPU 会置位忙标志）
        asm!("ltr {0:x}", in(reg) TSS_SEL, options(nostack, preserves_flags));
    }
}

/// 设置 TSS.RSP0：ring3 → ring0 的中断/异常切入时使用的内核栈顶。
///
/// 为什么必须是**另一条**栈：进入用户态时内核调用栈仍然存活（`enter_user`
/// 的调用者还压着返回地址），若沿用同一条栈，用户态期间的中断会把帧压到
/// 活动的内核栈上，覆盖内核自身的栈帧。
///
/// # Safety
/// `top` 必须是一条该期间独占、且在当前页表中已映射的内核栈顶端。
pub unsafe fn set_kernel_stack(top: u64) {
    unsafe {
        (*addr_of_mut!(TSS)).rsp[0] = top;
    }
}

/// 读取当前 TSS.RSP0（自检/日志用）。
pub fn kernel_stack() -> u64 {
    unsafe { (*addr_of!(TSS)).rsp[0] }
}
