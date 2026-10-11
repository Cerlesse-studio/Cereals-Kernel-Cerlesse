//! `ls` —— 目录列举（v0.9 第五步的基础用户程序，RAMFS 上落在 `/bin/ls`）
//!
//! 它只需要一条内核能力：**`getdents`**（v0.9 第五步新增）。内核把 VFS 的目录项
//! 写成定长记录（88 字节/条，见 [`libc::DIRENT_SIZE`]），本程序用 libc 的解析器
//! 逐条读出 —— 用户程序不自己算偏移，记录布局只有一个权威定义
//! （`kernel/src/syscall/dir.rs`）。
//!
//! ## 为什么值得单独做一个程序
//!
//! 这是**第一条用户态读取 VFS 目录**的路径：v0.7 起 `list()` 就存在，但只有内核
//! 自测用过（`fs: ls / ...`）。`ls` 没有内建命令版本，所以它一定走
//! `fork` + `execve` + `wait4`（`make test-programs` 断言这条路径上的每一环）。
//!
//! 输出形状（一次 `println!` = 一次 `write`，便于与内核的 `user: write fd=1 len=N`
//! 逐条对齐）：
//!
//! ```text
//! ls: / entries=4
//! ls: entry name=init kind=file size=12345
//! ls: entry name=bin kind=dir size=0
//! ```

#![no_std]
#![no_main]

use libc::{
    arg_str, argc, dirent_is_dir, dirent_name, dirent_size, getdents, println, DIRENT_SIZE,
};

/// `getdents` 的输出缓冲：`1024 / 88 = 11` 条记录，足够当前 RAMFS
/// （`/` 4 条、`/bin` 6 条）。装不下时内核**截断**而不是报错，下面会明说。
const BUF: usize = 1024;
/// 最多接受多少个路径参数（`libc::syscall::ARG_MAX` 也是 8）。
const MAX_ARGS: usize = 8;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    // 参数：`argv[0]` 是命令名本身（Shell 就是这么传的），路径从 1 开始。
    let mut paths: [&str; MAX_ARGS] = [""; MAX_ARGS];
    let mut count = 0usize;
    for i in 1..argc().min(MAX_ARGS) {
        if let Some(path) = arg_str(i) {
            paths[count] = path;
            count += 1;
        }
    }
    if count == 0 {
        paths[0] = "/";
        count = 1;
    }

    let mut buf = [0u8; BUF];
    let capacity = BUF / DIRENT_SIZE;
    let mut status = 0;
    for path in &paths[..count] {
        let n = getdents(path, &mut buf);
        if n < 0 {
            println!("ls: {}: errno {}", path, n);
            status = 1;
            continue;
        }
        println!("ls: {} entries={}", path, n);
        for i in 0..n as usize {
            let kind = if dirent_is_dir(&buf, i) { "dir" } else { "file" };
            println!(
                "ls: entry name={} kind={} size={}",
                dirent_name(&buf, i),
                kind,
                dirent_size(&buf, i)
            );
        }
        if n as usize == capacity {
            println!("ls: {}: listing may be truncated (buffer holds {})", path, capacity);
        }
    }
    status
}
