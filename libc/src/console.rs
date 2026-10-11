//! 控制台输出：`print!` / `println!`
//!
//! 设计取舍：一次宏调用 = **一次** `write` 系统调用。
//! 理由是内核会为每次 `write` 打印一行 `user: write fd=1 len=N`，
//! 「一次调用一行日志」让串口轨迹可以逐条对齐（`make test-user` 的
//! `user: write fd=1 len=22` 断言就依赖这一点）。
//!
//! 用户态没有堆，所以格式化结果写进固定大小的栈缓冲（[`LINE_MAX`]），
//! 超出的部分被丢弃而不是分配内存。

use core::fmt;

use crate::syscall::{write, STDERR, STDOUT};

/// 单次输出缓冲的字节上限。足够放日志行，又不会挤爆 4 页用户栈。
pub const LINE_MAX: usize = 256;

/// 固定缓冲的控制台写句柄：`write_str` 只入缓冲，[`Writer::flush`] 才发系统调用。
pub struct Writer {
    fd: i32,
    buf: [u8; LINE_MAX],
    len: usize,
}

impl Writer {
    /// 面向 stdout（fd=1）的写句柄。
    pub fn stdout() -> Self {
        Self::new(STDOUT)
    }

    /// 面向 stderr（fd=2）的写句柄。
    pub fn stderr() -> Self {
        Self::new(STDERR)
    }

    fn new(fd: i32) -> Self {
        Self { fd, buf: [0; LINE_MAX], len: 0 }
    }

    /// 当前缓冲中待写出的字节数。
    pub fn len(&self) -> usize {
        self.len
    }

    /// 缓冲是否为空。
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// 把缓冲区内容作为**一次** `write` 送出，并清空缓冲。
    pub fn flush(&mut self) -> isize {
        let n = write(self.fd, &self.buf[..self.len]);
        self.len = 0;
        n
    }
}

impl fmt::Write for Writer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &byte in s.as_bytes() {
            if self.len == LINE_MAX {
                break; // 截断而不是失败：用户态没有堆可分配
            }
            self.buf[self.len] = byte;
            self.len += 1;
        }
        Ok(())
    }
}

/// 格式化并输出到 stdout（不换行），一次 `write` 系统调用。
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{
        use ::core::fmt::Write as _;
        let mut writer = $crate::console::Writer::stdout();
        let _ = ::core::write!(writer, $($arg)*);
        writer.flush();
    }};
}

/// 格式化并输出到 stdout 并换行，一次 `write` 系统调用。
#[macro_export]
macro_rules! println {
    () => {
        $crate::print!("\n")
    };
    ($($arg:tt)*) => {
        $crate::print!("{}\n", ::core::format_args!($($arg)*))
    };
}
