//! hello-bin —— **外部程序**（RAMFS 上的 `/bin/hello`，v0.9 第三步）
//!
//! 它不是内核直接跑的（`/init`、`/bin/sh` 那种），而是**被 Shell 用
//! `fork` + `execve` 起来**的：这份代码证明了「进程模型可用」这件事，
//! 每一条输出都对应一个可断言的串口轨迹：
//!
//! | 输出 | 证明了什么 |
//! | ---- | ---- |
//! | `hello-bin: Hello from an external program!` | `execve("/bin/hello")` 真的把映像换掉了 |
//! | `hello-bin: pid=N` | 新进程有**自己的** PID（不是 Shell 的） |
//! | `hello-bin: argc=N` | `argc` 由内核经入口寄存器送达 `_start`（见 `libc::args`） |
//! | `hello-bin: argv[i]=...` | `argv` 字符串真的写进了这个进程的用户栈 |
//! | 返回值 0 / 非 0 | 退出码由 `wait4` 交给父进程（Shell 打印 `exit code=`） |
//!
//! 与 `user/hello`（`/init`）的差异：不用堆、不碰文件系统 —— 外部程序应当尽量薄，
//! 让它验证的就是「进程」这件事本身，而不是别的子系统。

#![no_std]
#![no_main]

use libc::{arg, argc, getpid, println};

/// 内核通过 `_start` 调用该入口；返回值即退出码。
///
/// 退出码有意依赖参数个数：Shell 里敲 `hello` 得到 0，敲 `hello a b` 得到 2 ——
/// 于是「参数真的传到了子进程」在**退出码**上也能观察到，而不只是靠日志。
#[no_mangle]
pub extern "C" fn main() -> i32 {
    let pid = getpid();
    let n = argc();

    println!("hello-bin: Hello from an external program!");
    println!("hello-bin: pid={}", pid);
    println!("hello-bin: argc={}", n);
    for i in 0..n {
        match arg(i) {
            Some(bytes) => match core::str::from_utf8(bytes) {
                Ok(text) => println!("hello-bin: argv[{}]={}", i, text),
                Err(_) => println!("hello-bin: argv[{}]=<not utf-8>", i),
            },
            None => println!("hello-bin: argv[{}]=<missing>", i),
        }
    }
    println!("hello-bin: done, exiting with {}", (n - 1) as i32);

    // 退出码 = 参数个数 - 1（`hello` → 0，`hello a b` → 2）
    (n - 1) as i32
}
