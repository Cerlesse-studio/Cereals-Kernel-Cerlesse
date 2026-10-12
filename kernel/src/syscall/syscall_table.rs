//! 系统调用号到内核处理函数的表（v0.6）
//!
//! 编号沿用 **Linux x86_64** 语义，便于以后直接跑现成的用户态程序：
//! `read=0, write=1, open=2, close=3, mmap=9, mprotect=10, munmap=11, brk=12,
//! getpid=39, nanosleep=35, fork=57, execve=59, exit=60, wait4=61`，
//! 目录列举 `getdents=78`（Linux 同名号，记录格式见下）。
//!
//! **Cerlesse 扩展号段（`>= 256`）**：Linux x86_64 没有「列举进程」的系统调用
//! （`ps` 走 `/proc`），而本内核的 VFS 还没有虚拟文件，所以 v0.9 第五步在
//! 0..255 之外开了 `getprocs=256`。号段边界与 [`SYS_MAX`] 一致：
//! `0..=255` 保持 Linux 语义，`>= 256` 明确是 Cerlesse 自己的 ABI。
//! CKM-1 在 257..=259 上实现了策略查询 / 审计读取 / 组件状态（260 留桩）；
//! CSS-1 在 CKM 号段之后开出存储号段 `261..=266`
//! （`docs/storage.md` 6.3）：261/265 已实现，其余落 [`UNIMPLEMENTED`] 桩；
//! **CSS-3 把 262/263/264 全部实现**（挂载 / 卸载 / 盘符），只剩 266（格式化）留桩。
//!
//! 表按 `SYS_MAX` 排满：未单独实现的号落在公共 ENOSYS 桩上，
//! 因此「号合法但未实现」与「号越界」是两种可区分的失败
//! （前者查表命中桩，后者 `parse` 返回 `None`，两者最终都返回 `-ENOSYS`）。

// 编号表本身就是对外契约：尚未实现的号也必须有名字，否则调用方只能靠魔法数。
#![allow(dead_code)]

use crate::syscall::address_space::UserSpace;

/// 系统调用号（Linux x86_64 语义）
pub const SYS_READ: u64 = 0;
pub const SYS_WRITE: u64 = 1;
pub const SYS_OPEN: u64 = 2;
pub const SYS_CLOSE: u64 = 3;
pub const SYS_MMAP: u64 = 9;
pub const SYS_MPROTECT: u64 = 10;
pub const SYS_MUNMAP: u64 = 11;
pub const SYS_BRK: u64 = 12;
pub const SYS_SLEEP: u64 = 35; // nanosleep
pub const SYS_GETPID: u64 = 39;
pub const SYS_FORK: u64 = 57;
pub const SYS_EXEC: u64 = 59;
pub const SYS_EXIT: u64 = 60;
pub const SYS_WAIT: u64 = 61;
/// 目录列举（v0.9 第五步）。Linux 也有这个号，但**记录格式不同**（定长 88 字节，
/// 见 `syscall/dir.rs` 模块文档）：本内核的 VFS 没有 inode 号/可变长目录项，
/// 因此不编造 `linux_dirent64` 里那些字段。
pub const SYS_GETDENTS: u64 = 78;
/// 进程表快照（v0.9 第五步）。**Cerlesse 扩展号**：Linux x86_64 不使用 256。
pub const SYS_GETPROCS: u64 = 256;
/// CKM 策略查询（**CKM-1**，`docs/ckm.md` 4.3）：返回某个 op 在当前凭证下的判定
/// + 一条 48 字节记录。**无能力要求**（只能查自己）。
pub const SYS_CKM_POLICY_QUERY: u64 = 257;
/// CKM 审计读取（CKM-1）：把审计环里 `seq > since_seq` 的记录拷进用户缓冲。
/// 需要 `CAP_AUDIT_READ`（判定点 5）。
pub const SYS_CKM_AUDIT_READ: u64 = 258;
/// CKM 组件状态导出（CKM-1）：按确定性顺序导出 64 字节记录。只读。
pub const SYS_CKM_COMPONENT_STATE: u64 = 259;
/// CKM 策略设置（**阶段 4 才定义**，CKM-1 落 ENOSYS 桩）：需要 `CAP_CKM_ADMIN`。
pub const SYS_CKM_POLICY_SET: u64 = 260;
/// 存储卷快照（**CSS-1**，H-08）：按确定性顺序导出 64 字节卷记录。
/// 号段接在 CKM 提案预留的 257–260 之后（见 `docs/storage.md` 6.3）。
pub const SYS_STORAGE_VOLUMES: u64 = 261;
/// 存储重扫（CSS-1）：`disk_index == u32::MAX` 表示全部设备。
pub const SYS_STORAGE_RESCAN: u64 = 265;
/// 按卷身份挂载（**CSS-3 已实现**）：已挂载的卷只加别名挂载点（`docs/storage.md` 6.3）。
pub const SYS_STORAGE_MOUNT: u64 = 262;
/// 卸载挂载点（CSS-3 已实现）：只摘别名，卷仍在卷表里。
pub const SYS_STORAGE_UNMOUNT: u64 = 263;
/// 分配/取消盘符（CSS-3 已实现）：映射写进 CKM 状态区（`storage/letters`）。
pub const SYS_STORAGE_ASSIGN_LETTER: u64 = 264;
/// 创建/格式化文件系统（CSS-4 实现）。
pub const SYS_STORAGE_FORMAT: u64 = 266;
/// 系统调用号上限（不含），用于边界检查。
///
/// `0..=255` 是 Linux x86_64 号段，`256..` 是 Cerlesse 扩展号段（见模块头）：
/// `256` = `getprocs`，`257..=260` 是 CKM 号段（CKM-1 已实现 257/258/259，
/// 260 落 [`UNIMPLEMENTED`] 桩），`261..=266` 由 CSS 使用
/// （CSS-1 实现 261/265，CSS-3 实现 262/263/264，266 待 CSS-4）。
pub const SYS_MAX: u64 = 267;

/// 已定义但尚未实现的系统调用号（逐个补齐）：`(号, 名字)`。
///
/// 显式列出而不是「不写」：查表时它们落在 ENOSYS 桩上，
/// 与「号越界」是两种不同的失败原因，调试时不会被混为一谈。
/// v0.7 已把 `read/open/close` 从这张表里摘掉；用户堆（`mmap/munmap/brk`）
/// 见下方 [`SYS_TABLE`]。
///
/// **CSS-1 起这张表重新有内容**（266 格式化待 CSS-4）；
/// **CKM-1 追加 260**（`ckm_policy_set`）：号在阶段 1–3 已定义但行为未定，
/// 落在同一张桩表上（阶段 4 才给实现）；**CSS-3 把 262/263/264 从表里摘掉**
/// （挂载/卸载/字母已经实现，见 `syscall/storage.rs`）。
/// 串口上的 `syscall: ENOSYS nr=… name=…` 依然能区分「未实现」与「号越界」。
const UNIMPLEMENTED: [(u64, &str); 2] = [
    (SYS_CKM_POLICY_SET, "ckm_policy_set"),
    (SYS_STORAGE_FORMAT, "storage_format"),
];

/// 查「已命名但未实现」的号对应的名字（[`UNIMPLEMENTED`] 的查询接口）。
///
/// 用途：`syscall_dispatch` 在把调用交给 ENOSYS 桩之前打印
/// `syscall: ENOSYS nr=57 name=fork` —— 让「谁在依赖未实现的调用」
/// 在串口上**可断言**，而不是只写在文档里。
pub fn unimplemented_name(nid: u64) -> Option<&'static str> {
    let mut i = 0;
    while i < UNIMPLEMENTED.len() {
        if UNIMPLEMENTED[i].0 == nid {
            return Some(UNIMPLEMENTED[i].1);
        }
        i += 1;
    }
    None
}

/// 系统调用处理函数类型。`args[0..5]` = `rdi, rsi, rdx, r10, r8`。
pub type SyscallHandler = unsafe fn(args: &[u64; 5], space: *const UserSpace) -> i64;

/// 按系统调用号查表：号越界返回 `None`，否则返回槽位中的 handler
/// （未实现的号命中 [`syscall_enosys`]）。
pub fn parse(nid: u64) -> Option<SyscallHandler> {
    if nid >= SYS_MAX {
        return None;
    }
    Some(SYS_TABLE[nid as usize])
}

/// 公共 ENOSYS 桩（`-ENOSYS` = -38）。
pub(crate) fn syscall_enosys(_args: &[u64; 5], _space: *const UserSpace) -> i64 {
    -38
}

/// 系统调用表。已注册的号在 `const` 求值期替换为具体 handler。
pub const SYS_TABLE: [SyscallHandler; SYS_MAX as usize] = {
    let mut table: [SyscallHandler; SYS_MAX as usize] = [syscall_enosys; SYS_MAX as usize];
    table[SYS_READ as usize] = crate::syscall::file::sys_read;
    table[SYS_WRITE as usize] = crate::syscall::file::sys_write;
    table[SYS_OPEN as usize] = crate::syscall::file::sys_open;
    table[SYS_CLOSE as usize] = crate::syscall::file::sys_close;
    table[SYS_MMAP as usize] = crate::syscall::memory::sys_mmap;
    // v1.0 第一步：mprotect 不再是 ENOSYS 桩（W^X 的「先写后换只执行」）
    table[SYS_MPROTECT as usize] = crate::syscall::memory::sys_mprotect;
    table[SYS_MUNMAP as usize] = crate::syscall::memory::sys_munmap;
    table[SYS_BRK as usize] = crate::syscall::memory::sys_brk;
    table[SYS_GETPID as usize] = crate::syscall::process::sys_getpid;
    table[SYS_SLEEP as usize] = crate::syscall::process::sys_sleep;
    table[SYS_EXIT as usize] = crate::syscall::process::sys_exit;
    // v0.9 第三步：进程对象落地后 fork/execve/wait4 不再是 ENOSYS 桩
    table[SYS_FORK as usize] = crate::syscall::process::sys_fork;
    table[SYS_EXEC as usize] = crate::syscall::process::sys_exec;
    table[SYS_WAIT as usize] = crate::syscall::process::sys_wait;
    // v0.9 第五步：用户态可枚举目录（`ls`）与进程表（`ps`）
    table[SYS_GETDENTS as usize] = crate::syscall::dir::sys_getdents;
    table[SYS_GETPROCS as usize] = crate::syscall::process::sys_getprocs;
    // CKM-1：策略查询 / 审计读取 / 组件状态（260 落 UNIMPLEMENTED 桩）
    table[SYS_CKM_POLICY_QUERY as usize] = crate::syscall::ckm::sys_ckm_policy_query;
    table[SYS_CKM_AUDIT_READ as usize] = crate::syscall::ckm::sys_ckm_audit_read;
    table[SYS_CKM_COMPONENT_STATE as usize] = crate::syscall::ckm::sys_ckm_component_state;
    // CSS-1（H-08）：卷表快照与重扫
    table[SYS_STORAGE_VOLUMES as usize] = crate::syscall::storage::sys_storage_volumes;
    table[SYS_STORAGE_RESCAN as usize] = crate::syscall::storage::sys_storage_rescan;
    // CSS-3：按卷身份挂载 / 卸载 / 盘符（266 格式化仍落 UNIMPLEMENTED 桩）
    table[SYS_STORAGE_MOUNT as usize] = crate::syscall::storage::sys_storage_mount;
    table[SYS_STORAGE_UNMOUNT as usize] = crate::syscall::storage::sys_storage_unmount;
    table[SYS_STORAGE_ASSIGN_LETTER as usize] = crate::syscall::storage::sys_storage_assign_letter;
    table
};
