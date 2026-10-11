//! `cat` —— 打印文件内容（v0.9 第五步的基础用户程序，RAMFS 上落在 `/bin/cat`）
//!
//! 走的是 v0.7 就有的文件 syscall（`open` → `read` → `write` → `close`），
//! 因此它同时是「Shell 内建命令能满足的链路，用户程序自己也能实现」的证据：
//! 命令解析、参数、fd 管理与错误处理全部在用户态，内核只提供 syscall。
//!
//! 与内建 `cat` 的关系：Shell 的 `cat` 是内建命令，会**遮蔽**裸命令名
//! （内建优先，与真实 shell 一致）。要跑这个程序得用绝对路径 —— `/bin/cat`。
//! `make test-programs` 就是这么做的，所以断言里能同时看到内建与程序两条路径。
//!
//! 输出形状：
//!
//! ```text
//! motd: hello from ramfs          ← 文件内容（原样字节，可能没有换行）
//! cat: /etc/motd bytes=23         ← 每个文件一行小结（一次 write）
//! ```
//!
//! 失败时退出码为 1（Shell 会打印 `sh: cat: exit code=1`），错误本身打到 stdout
//! 以便与串口日志对齐。

#![no_std]
#![no_main]

use libc::{arg_str, argc, close, open, println, read, write, O_RDONLY, STDOUT};

/// 单次 `read` 的字节数（内核的 `IO_MAX` 是 4096，这里小一点即可）。
const IO: usize = 256;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    if argc() <= 1 {
        println!("cat: missing file operand (usage: cat <path> [path...])");
        return 1;
    }

    let mut buf = [0u8; IO];
    let mut status = 0;
    for i in 1..argc() {
        let Some(path) = arg_str(i) else {
            println!("cat: argument {} is not valid UTF-8", i);
            status = 1;
            continue;
        };
        let fd = open(path, O_RDONLY);
        if fd < 0 {
            println!("cat: {}: cannot open (errno {})", path, fd);
            status = 1;
            continue;
        }
        let fd = fd as i32;
        let mut total = 0usize;
        loop {
            let n = read(fd, &mut buf);
            if n < 0 {
                println!("cat: {}: read failed (errno {})", path, n);
                status = 1;
                break;
            }
            if n == 0 {
                break; // EOF：fd 偏移走到了文件末尾
            }
            let n = n as usize;
            let written = write(STDOUT, &buf[..n]);
            if written != n as isize {
                println!("cat: {}: short write ({} of {})", path, written, n);
                status = 1;
                break;
            }
            total += n;
        }
        let _ = close(fd);
        println!("cat: {} bytes={}", path, total);
    }
    status
}
