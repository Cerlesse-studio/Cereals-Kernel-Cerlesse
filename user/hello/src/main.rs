//! hello —— v0.7 的参考用户程序（也是 `/init`）
//!
//! 它不是「演示」而是**验收基准**：这段 Rust 编译出的 ELF 会由 `kernel/build.rs`
//! 内嵌，启动时写进 RAMFS 的 `/init`，再由内核的 ELF Loader 装载到 ring3 运行。
//! `make test-user` / `make test-fs` 的硬断言就落在这段程序的串口轨迹上：
//!
//! | 步骤 | 验证的能力 | 可断言的日志 |
//! | ---- | ---- | ---- |
//! | `println!` | `write` → 串口 | `user: write fd=1 len=22` |
//! | `getpid` | PID 传递 | `user: getpid -> 1` |
//! | `open/read/close /etc/motd` | VFS 读路径 | `user: open path=/etc/motd flags=0x0 fd=3`、`user: read fd=3 want=64 got=23`、首次读完后第二次读回 0（EOF） |
//! | `open(O_CREAT)/write /tmp/hello.txt` | VFS 写路径 | `user: write fd=3 len=24` + 内核读回 `fs: readback /tmp/hello.txt len=24` |
//! | `static mut` 自增 | `RW` 段对 ring3 可写 | `hello: static counter=1` |
//! | 非法指针 `write` | syscall 参数校验 | `user: EFAULT write ...` / `hello: bad write ret=-14` |
//! | `nanosleep` | tick 真实推进 | `user: sleep done ticks=2` |
//! | `exit(42)` | 退出码回收 | `user: exit code=42` |
//!
//! 编写新的用户程序时把它当作模板复制即可（`Cargo.toml` + `build.rs` +
//! `user.ld` + `src/main.rs`），约定见 `libc/src/lib.rs` 顶部文档。

#![no_std]
#![no_main]

use libc::syscall::{raw, SYS_WRITE};
use libc::{
    close, getpid, nanosleep, open, println, read, write, O_CREAT, O_RDONLY, O_WRONLY,
};

/// 可写的静态变量：v0.7 的 ELF Loader 把 `RW` 段映射成 ring3 可写，
/// 因此这里在运行期被改写（v0.6 会 #PF —— 代码页整体只读）。
static mut COUNTER: u32 = 0;

/// 内核提供的文件（`fs::init` 写入，23 字节）。
const MOTD_PATH: &str = "/etc/motd";
/// 用户程序写文件的落点（内核随后读回并打印内容）。
const OUT_PATH: &str = "/tmp/hello.txt";

/// 内核通过 `_start` 调用该入口；返回值即退出码。
#[no_mangle]
pub extern "C" fn main() -> i32 {
    // 22 字节，一次 write —— `make test-user` 断言 `user: write fd=1 len=22`。
    println!("hello from user space");

    // 展示 `{}` 格式化（PID 由 syscall 返回，再经 libc 格式化后写出）。
    let pid = getpid();
    println!("hello: pid={}", pid);

    // ---- 1. 从 RAMFS 读文件：open → read → write → read(EOF) → close ----
    let fd = open(MOTD_PATH, O_RDONLY);
    println!("hello: open {} fd={}", MOTD_PATH, fd);
    if fd >= 0 {
        let mut buf = [0u8; 64];
        let n = read(fd as i32, &mut buf);
        println!("hello: read n={}", n);
        if n > 0 {
            // 把读到的内容原样写到串口（内核日志里能看到 23 字节的那一次 write）
            let _ = write(1, &buf[..n as usize]);
        }
        // 第二次读：offset 已经在文件尾 → 0（EOF），证明偏移量真的在推进
        let eof = read(fd as i32, &mut buf);
        println!("hello: eof read n={}", eof);
        let _ = close(fd as i32);
    }

    // ---- 2. 往 RAMFS 写文件：内核在用户态结束后读回并打印 ----
    let wfd = open(OUT_PATH, O_CREAT | O_WRONLY);
    println!("hello: create {} fd={}", OUT_PATH, wfd);
    if wfd >= 0 {
        let msg = b"written by user program\n";
        let written = write(wfd as i32, msg);
        println!("hello: wrote {} bytes", written);
        let _ = close(wfd as i32);
    }

    // ---- 3. 可写静态数据（RW 段对 ring3 可写） ----
    let counter = unsafe {
        COUNTER += 1;
        COUNTER
    };
    println!("hello: static counter={}", counter);

    // ---- 4. 非法用户指针：内核校验用户缓冲，应返回 -EFAULT（-14） ----
    let bad = unsafe { raw(SYS_WRITE, 1, 0xdead_0000_0000, 1, 0, 0) };
    println!("hello: bad write ret={}", bad);

    // ---- 5. nanosleep：2 个 tick = 20ms，内核在等待期间 `sti; hlt` ----
    let slept = nanosleep(2);
    println!("hello: slept={}", slept);

    42
}
