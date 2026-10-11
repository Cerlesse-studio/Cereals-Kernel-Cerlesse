//! CPU 控制寄存器与 MSR 操作（v0.1 起；v0.6 填充 syscall/user space 所需部分）
//!
//! v0.3：读取 CR3（当前页表根）供内存子系统使用。
//! v0.6：`syscall`/`sysret` 入口所需的 EFER（SCE/NXE）、IA32_STAR / IA32_LSTAR /
//! IA32_FMASK，以及 CR3 切换与 CR4 的 SMEP/SMAP 清除（ring0 需要读用户页）。

/// IA32_EFER
pub const MSR_EFER: u32 = 0xC000_0080;
/// IA32_STAR（syscall/sysret 的 CS/SS 选择子）
pub const MSR_STAR: u32 = 0xC000_0081;
/// IA32_LSTAR（`syscall` 指令入口地址）
pub const MSR_LSTAR: u32 = 0xC000_0082;
/// IA32_FMASK（`syscall` 进入时清零的 RFLAGS 位）
pub const MSR_FMASK: u32 = 0xC000_0084;

/// EFER.SCE：使能 `syscall`/`sysret` 指令。
const EFER_SCE: u64 = 1 << 0;
/// EFER.NXE：使能页表 NX 位（v0.3 起堆页已使用 bit63）。
const EFER_NXE: u64 = 1 << 11;

/// CR4.SMEP：ring0 禁止执行用户页。
const CR4_SMEP: u64 = 1 << 20;
/// CR4.SMAP：ring0 禁止访问用户页。
const CR4_SMAP: u64 = 1 << 21;

/// 读取 CR3（当前页表根物理地址）。
pub fn read_cr3() -> u64 {
    let value: u64;
    unsafe {
        core::arch::asm!("mov {}, cr3", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

/// 写入 CR3（切换地址空间）。写 CR3 会同时刷新非全局 TLB 项。
///
/// # Safety
/// `value` 必须是当前恒等映射下可直接访问的合法页表根物理地址。
pub unsafe fn write_cr3(value: u64) {
    unsafe {
        core::arch::asm!("mov cr3, {}", in(reg) value, options(nostack, preserves_flags));
    }
}

/// 读取 CR4。
pub fn read_cr4() -> u64 {
    let value: u64;
    unsafe {
        core::arch::asm!("mov {}, cr4", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

/// 写入 CR4。
///
/// # Safety
/// CR4 位控制处理器级安全策略，错误取值会立即改变异常/隔离行为。
pub unsafe fn write_cr4(value: u64) {
    unsafe {
        core::arch::asm!("mov cr4, {}", in(reg) value, options(nostack, preserves_flags));
    }
}

/// 读取 MSR。
///
/// # Safety
/// 仅在 ring0 调用；`addr` 必须是合法 MSR 编号（非法编号触发 #GP）。
pub unsafe fn rdmsr(addr: u32) -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        core::arch::asm!(
            "rdmsr",
            in("ecx") addr,
            out("eax") lo,
            out("edx") hi,
            options(nomem, nostack, preserves_flags)
        );
    }
    ((hi as u64) << 32) | lo as u64
}

/// 写入 MSR。
///
/// # Safety
/// 仅在 ring0 调用；`addr` 必须是合法且可写的 MSR 编号。
pub unsafe fn wrmsr(addr: u32, value: u64) {
    unsafe {
        core::arch::asm!(
            "wrmsr",
            in("ecx") addr,
            in("eax") value as u32,
            in("edx") (value >> 32) as u32,
            options(nomem, nostack, preserves_flags)
        );
    }
}

/// 读取-修改-写入 EFER 的指定位。
unsafe fn set_efer_bits(bits: u64) {
    unsafe {
        let efer = rdmsr(MSR_EFER);
        wrmsr(MSR_EFER, efer | bits);
    }
}

/// 使能 EFER.SCE（`syscall`/`sysret` 指令）。
pub fn enable_syscall_extensions() {
    unsafe { set_efer_bits(EFER_SCE) }
}

/// 使能 EFER.NXE（页表 NX 位）。v0.3 的堆页映射已使用 bit63，此处显式打开。
pub fn enable_nx() {
    unsafe { set_efer_bits(EFER_NXE) }
}

/// 设置 IA32_STAR：
/// - `kernel_cs`（bits 47:32）：`syscall` 后加载的 CS，SS = CS + 8；
/// - `user_base`（bits 63:48）：`sysret` 时 SS = base + 8、CS = base + 16。
///
/// 因此 GDT 必须按 [kcode, kdata, udata, ucode] 顺序排布（见 `gdt.rs`）。
pub fn set_star(kernel_cs: u64, user_base: u64) {
    unsafe {
        wrmsr(
            MSR_STAR,
            ((user_base & 0xFFFF) << 48) | ((kernel_cs & 0xFFFF) << 32),
        );
    }
}

/// 设置 IA32_LSTAR（`syscall` 指令入口地址）。
pub fn set_lstar(entry: u64) {
    unsafe { wrmsr(MSR_LSTAR, entry) }
}

/// 设置 IA32_FMASK（`syscall` 进入内核时清零的 RFLAGS 位）。
pub fn set_fmask(mask: u64) {
    unsafe { wrmsr(MSR_FMASK, mask) }
}

/// 清除 CR4 的 SMEP/SMAP。
///
/// 理由：v0.6 的 ring0 需要直接读写用户页（`sys_write` 拷贝用户缓冲、进程管理
/// 写用户栈），SMEP/SMAP 会使这些访问触发 #PF。v0.7 起改为 stac/clac 精细控制。
pub fn clear_cr4_smep_smap() {
    unsafe {
        let cr4 = read_cr4();
        write_cr4(cr4 & !CR4_SMEP & !CR4_SMAP);
    }
}
