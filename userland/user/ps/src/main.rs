//! `ps` —— 打印进程表（v0.9 第五步的基础用户程序，RAMFS 上落在 `/bin/ps`）
//!
//! 它只需要一条内核能力：**`getprocs`**（v0.9 第五步新增的 Cerlesse 扩展系统调用，
//! 编号 256）。Linux 的 `ps` 读 `/proc`，而本内核的 VFS 还没有虚拟文件，所以这里
//! 直接把进程表快照交出来 —— 记录是定长的（40 字节/条），布局只有一个权威定义
//! （`kernel/src/syscall/process.rs::sys_getprocs`），libc 提供 `procinfo_*` 解析器。
//!
//! 为什么值得单独做一个程序：这是**第一条用户态读取内核进程表**的路径，也就把
//! 「`fork`/`execve` 建出来的进程对象」变成用户可见的事实 —— 运行 `ps` 时表里
//! 应当恰好有 Shell（父）与 `ps` 自己两条，且 `ppid` 指向 Shell。
//!
//! 输出形状：
//!
//! ```text
//! ps: pid ppid state comm
//! ps: pid=3 ppid=0 state=RUNNING comm=sh
//! ps: pid=4 ppid=3 state=RUNNING comm=ps
//! ps: 2 process(es)
//! ```

#![no_std]
#![no_main]

use libc::{
    getprocs, println, procinfo_name, procinfo_pid, procinfo_ppid, procinfo_state, PROCINFO_SIZE,
};

/// 进程表容量（内核 `process::MAX_PROC` = 8）—— 一次调用就能装下整张表，
/// 因此不需要分页/续读循环。
const BUF: usize = 8 * PROCINFO_SIZE;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let mut buf = [0u8; BUF];
    let n = getprocs(&mut buf);
    if n < 0 {
        println!("ps: getprocs failed (errno {})", n);
        return 1;
    }

    println!("ps: pid ppid state comm");
    for i in 0..n as usize {
        println!(
            "ps: pid={} ppid={} state={} comm={}",
            procinfo_pid(&buf, i),
            procinfo_ppid(&buf, i),
            procinfo_state(&buf, i),
            procinfo_name(&buf, i)
        );
    }
    println!("ps: {} process(es)", n);
    0
}
