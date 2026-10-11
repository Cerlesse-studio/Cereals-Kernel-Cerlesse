//! 用户态堆：`#[global_allocator]`（v0.9 运行时前置条件第一步）
//!
//! 内核在 v0.9 补上了用户堆窗口（`brk` / `mmap` / `munmap`）。这一层把内核的匿名
//! 映射变成 Rust 用户程序能用的堆：注册一个全局分配器之后，`user/*` 里的
//! `Vec`/`String`/`Box` 才真正可分配（在这一版之前，用户程序**只能**把数据放在
//! 栈或静态数组里 —— 见 `libc/README.md` 第 4 节）。
//!
//! ## 实现：只前进的 bump 分配器
//!
//! ```text
//! 一个 64 KiB 的块（mmap 匿名页，内核保证全零）
//!   ├── 已切出去的分配（cursor 之前的字节，不回收）
//!   └── 剩余空间（cursor..end）
//! 空间不够 → 再 mmap 一块（旧块的剩余尾巴不再使用）
//! ```
//!
//! `dealloc` 是**空操作**：这一版只保证「可以分配」，不保证「可以回收」。
//!
//! ## 为什么不回收也能接受（以及限制在哪）
//!
//! - 当前用户态模型是「一次运行一个程序，`exit` 后整棵地址空间由内核
//!   `UserSpace::Drop` 释放」——所以进程内的「泄漏」在一次运行结束时会被内核回收，
//!   不会留在系统里；
//! - 代价是**长驻程序**的内存占用会随分配次数单调上升（尤其 `Vec` 增长会反复申请新块）。
//!   真正的回收（空闲链表 / `malloc` 级碎片整理）与 `mprotect`（W^X 转换）
//!   一起留到后续版本，那时这份实现会被替换而不是打补丁；
//! - 单线程：用户态目前没有线程，因此不需要锁（`RawHeap` 用 `UnsafeCell` 表达
//!   「有内部可变性，但调用方保证不并发」）。
//!
//! ## 对齐
//!
//! 每个分配都按 `Layout::align()` 向上对齐（下限 16 字节，覆盖 `u128`/SSE 之类）。
//! 对齐是**分配器自己的责任**：内核只保证 mmap 返回页对齐（4096）的起点。

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::ptr;

use crate::syscall::{mmap, MAP_ANONYMOUS, MAP_PRIVATE, PROT_READ, PROT_WRITE};

/// 一次向内核申请的块大小（64 KiB）。`Vec` 之类的常见负载几次分配就能跑起来。
const CHUNK: usize = 64 * 1024;
/// 页面大小（与内核 `memory::page::PAGE_SIZE` 一致）。
const PAGE: usize = 4096;
/// 对齐下限：16 字节足够覆盖 `u128`、8 字节整数与常见结构体。
const MIN_ALIGN: usize = 16;

/// bump 分配器的状态：`[cursor, end)` 是当前块里还没切出去的部分。
struct Bump {
    cursor: usize,
    end: usize,
}

/// 带内部可变性的全局分配器（单线程，因此不需要锁）。
struct RawHeap(UnsafeCell<Bump>);

// SAFETY: 用户态目前单线程，分配路径不会被并发调用（见模块文档「实现」一节）。
unsafe impl Sync for RawHeap {}

/// 全局分配器实例（`#[global_allocator]` 注册的就是它）。
#[global_allocator]
static HEAP: RawHeap = RawHeap(UnsafeCell::new(Bump { cursor: 0, end: 0 }));

impl RawHeap {
    /// 从内核新申请一块内存，返回 `[base, base + len)`。
    ///
    /// 申请量至少 [`CHUNK`]，按页向上取整（内核按页分配，非整页的请求也会占满页）。
    fn map_chunk(&self, need: usize) -> Option<(usize, usize)> {
        let want = need.max(CHUNK);
        let len = want.checked_add(PAGE - 1).map(|v| v & !(PAGE - 1))?;
        let addr = mmap(len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS);
        if addr <= 0 {
            return None; // 负数是 errno（窗口用完 → -ENOMEM）
        }
        let base = addr as usize;
        Some((base, base + len))
    }
}

unsafe impl GlobalAlloc for RawHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = layout.size().max(1);
        let align = layout.align().max(MIN_ALIGN);
        // SAFETY: 单线程（模块文档），且本函数是唯一的写入点。
        let bump = unsafe { &mut *self.0.get() };

        let mut start = match bump.cursor.checked_add(align - 1) {
            Some(v) => v & !(align - 1),
            None => return ptr::null_mut(),
        };
        if start.checked_add(size).map_or(true, |need| need > bump.end) {
            // 当前块放不下：换一块新的（旧块的剩余尾巴按设计被放弃）
            let need = match size.checked_add(align) {
                Some(need) => need,
                None => return ptr::null_mut(),
            };
            match self.map_chunk(need) {
                Some((base, end)) => {
                    start = (base + align - 1) & !(align - 1);
                    bump.cursor = start;
                    bump.end = end;
                }
                None => return ptr::null_mut(), // OOM：交给调用方的 handle_alloc_error
            }
        }
        bump.cursor = start + size;
        start as *mut u8
    }

    /// 不回收（见模块文档）。内核在用户程序结束时整棵释放地址空间。
    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
}

/// 当前块里还剩多少字节（诊断/自测用，不参与分配决策）。
pub fn remaining() -> usize {
    // SAFETY: 单线程（模块文档）；只读游标，不改变分配状态。
    let bump = unsafe { &*HEAP.0.get() };
    bump.end.saturating_sub(bump.cursor)
}
