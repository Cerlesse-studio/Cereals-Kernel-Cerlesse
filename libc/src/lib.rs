//! libc —— Cerlesse 最小用户态运行时（v0.6 脚手架，v0.7 文件 syscall，v0.9 用户堆）
//!
//! 这一层把「内核 ABI」封装成用户程序可以直接调用的形状，让新用户程序
//! **不需要**再手写机器码：
//!
//! - [`syscall`]：`syscall` 指令封装 + 调用（`write` / `read` / `open` / `close` /
//!   `getpid` / `nanosleep` / `exit` / `brk` / `mmap` / `munmap`）；
//! - [`heap`]：`#[global_allocator]`（bump over `mmap`）—— 用户程序因此可以用
//!   `Vec`/`String`/`Box`（在此之前只能把数据放在栈或静态数组里）；
//! - [`console`]：`print!` / `println!`（固定缓冲，一次 `write` 送出一整行）；
//! - `_start`（本文件底部 `global_asm!`）：`_start` → 用户 `main` → `exit(main())`；
//! - `#[panic_handler]`：用户态 panic 打到 stderr 后 `exit(101)`，不会卡死在内核里。
//!
//! 用户程序的约定（`user/*`）：
//!
//! ```ignore
//! #![no_std]
//! #![no_main]
//!
//! use libc::{getpid, println};
//!
//! #[no_mangle]
//! pub extern "C" fn main() -> i32 {
//!     println!("pid={}", getpid());
//!     0 // 退出码
//! }
//! ```
//!
//! 三条硬约束（都有实测理由，改错会在链接期或 ring3 里立刻炸）：
//!
//! 1. **每个段必须页对齐**（`user.ld` 里的 `ALIGN(0x1000)`）。v0.7 的 ELF Loader
//!    按页设置权限（`R` → `NX`、`W` → 可写、`X` → 可执行），同页既 `W` 又 `X`
//!    会被装载器直接拒绝（`user: ELF rejected: W and X segments share a page`）；
//! 2. **必须固定链接到 `USER_VIRT_BASE`（0x0000_0080_0000_0000）**：链接地址 = 运行地址，
//!    程序里的绝对地址因此不需要重定位（内核不做加载时重定位，只接受 `ET_EXEC`）；
//! 3. **程序有可写数据段**（`.data`/`.bss` 映射为 ring3 可写、不可执行）
//!    与**用户堆**（`brk` 窗口 256 KiB + 匿名 `mmap` 窗口 256 KiB，见 [`heap`]）；
//!    `mmap` 只支持私有匿名页、拒绝 `PROT_WRITE|PROT_EXEC`（W^X）、且没有 `offset` 参数。
//!
//! 系统调用号与错误约定见 `kernel/src/syscall/syscall_table.rs`（Linux x86_64 语义）。

#![no_std]

pub mod console;
pub mod heap;
pub mod syscall;

pub use crate::console::Writer;
pub use crate::syscall::{
    brk, close, exit, getpid, mmap, munmap, nanosleep, open, read, write, EBADF, EFAULT, EISDIR,
    EINVAL, ENOENT, ENOMEM, ENOSPC, ENOSYS, MAP_ANONYMOUS, MAP_PRIVATE, O_CREAT, O_RDONLY, O_RDWR,
    O_WRONLY, PROT_EXEC, PROT_READ, PROT_WRITE, STDERR, STDIN, STDOUT,
};

use core::arch::global_asm;

// 用户程序入口。放在独立段 `.text.start` 并由 `user.ld` 排在该段最前面，
// 使入口落在映像第 0 页（ELF 头里的 `e_entry` 由装载器直接使用）。
//
// `main` 由用户程序提供（`#[no_mangle] pub extern "C" fn main() -> i32`），
// 返回值即退出码，交给 `exit` 系统调用。
//
// 注意：`global_asm!` 是宏调用，不能挂文档注释（会被判为 unused doc comment）。
global_asm!(
    ".section .text.start,\"ax\"",
    ".global _start",
    ".type _start, @function",
    "_start:",
    "xor ebp, ebp",              // 栈回溯终点
    "and rsp, -16",              // SysV：call 前保持 16 字节对齐（栈顶本就对齐，这里是防御）
    "call main",
    "mov edi, eax",              // 退出码 = main 的返回值
    "mov eax, 60",               // SYS_exit
    "syscall",
    "ud2",                       // exit 不返回；到达这里说明内核违反了约定
    ".size _start, . - _start",
);

// 只被 `_start` 的汇编 `call main` 引用，Rust 侧看不到使用点，因此对死代码
// 检查显式豁免；真正的检查发生在链接期（用户程序没提供 `main` 就会报未定义符号）。
#[allow(dead_code)]
extern "C" {
    /// 由用户程序提供的 C ABI 入口，返回值作为退出码。
    fn main() -> i32;
}

/// 用户态 panic：打到 stderr 后以 101 退出。
///
/// 放在 libc 里是为了让每个用户程序不必各自实现一遍；
/// 因此用户程序**不要**再定义 `#[panic_handler]`（会产生重复符号）。
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    use core::fmt::Write as _;

    struct Stderr;

    impl core::fmt::Write for Stderr {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            let _ = write(STDERR, s.as_bytes());
            Ok(())
        }
    }

    let _ = core::write!(Stderr, "user panic: {}\n", info);
    exit(101)
}
