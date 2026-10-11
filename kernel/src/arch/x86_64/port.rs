//! x86_64 端口 I/O（v0.8 实现）
//!
//! 为什么单独一个模块：v0.8 的 PCI 配置空间（`0xCF8`/`0xCFC`）与 legacy virtio
//! 的寄存器窗口都是**端口 I/O**，需要 32 位宽度的 `inl`/`outl`；此前各驱动
//! （serial / keyboard / PIC）各自带一份私有的 `outb`/`inb`，宽度也只到 8 位。
//! 这里提供完整的一组（8/16/32 位），新代码统一用它们；已有模块的私有副本
//! 保持不变（改动它们只会扩大 diff，与本次目标无关）。
//!
//! 语义提醒：端口 I/O 不是内存访问，**不参与缓存一致性**，也不可以被编译器
//! 重排到彼此之间之外的地方。因此这里全部声明 `options(nomem, nostack, preserves_flags)`：
//! - `nomem`：不动内存（除了 `in` 系列的输出寄存器）；
//! - `nostack`：不动栈；
//! - `preserves_flags`：`in`/`out` 不修改 RFLAGS（与 `pushfq` 类指令不同）。
//!
//! 端口 I/O 也**不需要**内存屏障来保证自设备可见：对端口寄存器的写是强序的。

use core::arch::asm;

/// 从 8 位端口读一字节。
///
/// # Safety
/// `port` 必须是真实存在的端口；访问不存在的端口会返回垃圾值（x86 不会报错），
/// 对某些设备端口还可能产生副作用。
#[inline]
pub unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    unsafe {
        asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack, preserves_flags));
    }
    value
}

/// 向 8 位端口写一字节。
///
/// # Safety
/// 同 [`inb`]。
#[inline]
pub unsafe fn outb(port: u16, value: u8) {
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
    }
}

/// 从 16 位端口读一字节。
///
/// # Safety
/// 同 [`inb`]。
#[inline]
pub unsafe fn inw(port: u16) -> u16 {
    let value: u16;
    unsafe {
        asm!("in ax, dx", in("dx") port, out("ax") value, options(nomem, nostack, preserves_flags));
    }
    value
}

/// 向 16 位端口写一字节。
///
/// # Safety
/// 同 [`inb`]。
#[inline]
pub unsafe fn outw(port: u16, value: u16) {
    unsafe {
        asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack, preserves_flags));
    }
}

/// 从 32 位端口读一字节。
///
/// # Safety
/// 同 [`inb`]。
#[inline]
pub unsafe fn inl(port: u16) -> u32 {
    let value: u32;
    unsafe {
        asm!("in eax, dx", in("dx") port, out("eax") value, options(nomem, nostack, preserves_flags));
    }
    value
}

/// 向 32 位端口写一字节。
///
/// # Safety
/// 同 [`inb`]。
#[inline]
pub unsafe fn outl(port: u16, value: u32) {
    unsafe {
        asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack, preserves_flags));
    }
}
