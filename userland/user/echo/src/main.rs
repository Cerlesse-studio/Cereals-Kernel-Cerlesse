//! `echo` —— 打印参数（v0.9 第五步的基础用户程序，RAMFS 上落在 `/bin/echo`）
//!
//! 这是最小的「有参数的程序」：除了 `write` 之外不需要任何内核能力，
//! 因此它把 **`argc`/`argv` 传递**单独隔离出来验收 —— 参数是内核在 `execve`
//! 时写进新进程用户栈的（`syscall::process::build_user_frame`），
//! 用户态通过 `libc::args` 取回，中间没有堆、没有格式化缓冲。
//!
//! 与内建 `echo` 的关系：Shell 的 `echo` 是内建命令，会**遮蔽**裸命令名。
//! 要跑这个程序得用绝对路径 —— `/bin/echo`（`make test-programs` 就是这么做的）。
//!
//! 输出：把全部参数用单个空格拼成一行并**一次** `write` 送出（与内建 `echo`
//! 每参数一次 `write` 不同 —— 这里演示的是另一种合理取舍：一次系统调用）。
//! 参数总长超过 [`OUT`] 时截断，不分配内存。

#![no_std]
#![no_main]

use libc::{arg, argc, write, STDOUT};

/// 单行输出缓冲（固定大小：用户态这一版没必要为格式化分配堆）。
const OUT: usize = 256;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let mut out = [0u8; OUT];
    let mut len = 0usize;
    let mut truncated = false;

    // `argv[0]` 是命令名（内核原样透传），所以从下标 1 开始才是参数。
    for i in 1..argc() {
        let Some(arg_bytes) = arg(i) else {
            continue;
        };
        if i > 1 {
            if len == OUT {
                truncated = true;
                break;
            }
            out[len] = b' ';
            len += 1;
        }
        for &byte in arg_bytes {
            if len == OUT {
                truncated = true;
                break;
            }
            out[len] = byte;
            len += 1;
        }
        if truncated {
            break;
        }
    }

    if len < OUT {
        out[len] = b'\n';
        len += 1;
    }
    let _ = write(STDOUT, &out[..len]);
    let _ = truncated; // 缓冲固定：截断了也只是少几个字符，退出码仍为 0
    0
}
