//! VFS 虚拟文件系统抽象（v0.7 实现）
//!
//! 这一层定义「文件系统长什么样」，具体存储交给后端实现（当前只有
//! [`crate::fs::ramfs::RamFs`]）：
//!
//! - [`FileSystem`]：后端要实现的 trait（stat / read / write / create / mkdir / list）；
//! - 路径工具：[`normalize`] / [`components`] / [`within`] / [`split_parent`]；
//! - 系统级入口：[`stat`] / [`read_all`] / [`write`] / [`create`] / [`list`] ……
//!   它们先经 [`crate::fs::mount::table`] 做**挂载点路由**，再把挂载点内的相对
//!   路径交给对应后端。
//!
//! 错误约定：与系统调用一致，返回**负数 errno**（`-ENOENT` 等），
//! 这样 `fs` 的错误可以直接从 syscall handler 返回给用户态，中间不需要翻译。
//!
//! v0.7 的路径语义（刻意收窄，避免「看起来支持其实没实现」）：
//! - 只支持绝对路径与相对根路径（`/a/b`、`a/b` 等价），统一规范化为 `/a/b`；
//! - `.` 与 `..` 一律 [`EINVAL`]（没有 cwd、没有父目录链接）；
//! - 重复 `/` 与结尾 `/` 被折叠（`//a//b/` → `/a/b`）。

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::inode::{DirEntry, Metadata};

// ---- errno（与 syscall 约定一致：负数） ----

/// 文件/目录不存在。
pub const ENOENT: i64 = -2;
/// 可执行文件格式错误（ELF 装载器拒绝时使用，见 `syscall::elf`）。
pub const ENOEXEC: i64 = -8;
/// 内存不足（帧/堆分配失败）。
pub const ENOMEM: i64 = -12;
/// 文件描述符无效 / 未打开 / 打开模式与操作不符。
pub const EBADF: i64 = -9;
/// 系统调用参数非法（这里是：路径非法）。
pub const EINVAL: i64 = -22;
/// 目标已存在。
pub const EEXIST: i64 = -17;
/// 目标是目录，但调用方要当文件用。
pub const EISDIR: i64 = -21;
/// 路径中间分量不是目录。
pub const ENOTDIR: i64 = -20;
/// 空间不足（用在了「文件超过 RAMFS 单文件上限」）。
pub const ENOSPC: i64 = -28;
/// 只读文件系统（写操作落到 [`FileSystem::write`] 等写接口上时返回它）。
pub const EROFS: i64 = -30;
/// `-EIO`：块设备读写失败。
pub const EIO: i64 = -5;
/// `-EUCLEAN`：磁盘上的结构损坏（簇链断/越界、目录项非法）。
pub const EUCLEAN: i64 = -117;
/// `-ENODEV`：设备不存在 / 已消失（CSS 追加；让「拔盘」与「坏盘」可区分）。
pub const ENODEV: i64 = -19;

/// 后端文件系统必须实现的操作。
///
/// 方法都收**挂载点内的相对路径**（`/a/b`；挂载点本身是 `/`），
/// 挂载点前缀的剥离由 [`crate::fs::mount::MountTable::resolve`] 完成。
///
/// v0.8 起所有路径操作收 `&mut self`：挂载表本来就用 `&'static mut` **独占**持有后端
/// （[`crate::fs::mount::MountTable::resolve`]），而磁盘后端需要修改自己的扇区缓冲、
/// 甚至直接访问块设备。用 `&self` + 内部可变性只是把这件事实藏起来。
pub trait FileSystem {
    /// 后端名字（出现在 `fs: mount point=... fs=<name>` 与路由日志里）。
    fn name(&self) -> &'static str;

    /// 取元数据。
    fn stat(&mut self, path: &str) -> Result<Metadata, i64>;

    /// 从 `offset` 起读最多 `buf.len()` 字节，返回实际读到字节数（EOF 为 0）。
    fn read(&mut self, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, i64>;

    /// 读整个文件（**所有权版本**）。
    ///
    /// v0.7 曾是「零拷贝借用后端内部缓冲」（`&'a [u8]`），v0.8 改成返回 `Vec<u8>`：
    /// 磁盘后端的文件内容根本不在内存里，要「借用」就得先把整个文件读进一个缓存 ——
    /// 那还不如直接把这次读的结果交出去。代价是比 RAMFS 多一次拷贝（小文件可忽略）。
    fn read_all(&mut self, path: &str) -> Result<Vec<u8>, i64>;

    /// 从 `offset` 起写 `data`；`offset == len` 表示追加。
    fn write(&mut self, path: &str, offset: u64, data: &[u8]) -> Result<usize, i64>;

    /// 创建空文件（父目录必须存在）。
    fn create(&mut self, path: &str) -> Result<(), i64>;

    /// 创建目录（父目录必须存在）。
    fn mkdir(&mut self, path: &str) -> Result<(), i64>;

    /// 列举目录（后端自己的顺序；v0.7 的 RAMFS 是插入顺序，FAT 是磁盘上的顺序）。
    fn list(&mut self, path: &str) -> Result<Vec<DirEntry>, i64>;

    /// 后端统计 `(文件数, 占用字节数)`。
    ///
    /// 只有内存后端有「占用字节」的概念（磁盘后端要等 v0.8 的块分配），
    /// 因此默认实现是 `(0, 0)`；RAMFS 覆盖它，让启动日志能报出内核有多少
    /// 文件与字节进了内存。
    fn usage(&self) -> (usize, usize) {
        (0, 0)
    }
}

// ---- 路径工具 ----

/// 规范化路径为 `/a/b` 形式。
///
/// 拒绝空路径、`.`、`..`（[`EINVAL`]）；折叠重复 `/`、结尾 `/`，并允许省略前导 `/`。
pub fn normalize(path: &str) -> Result<String, i64> {
    if path.is_empty() {
        return Err(EINVAL);
    }
    let mut out = String::from("/");
    let mut first = true;
    for comp in path.split('/') {
        if comp.is_empty() {
            continue; // 折叠重复斜杠与结尾斜杠
        }
        if comp == "." || comp == ".." {
            // v0.7 没有 cwd，也没有 `..` 链接：明确拒绝而不是悄悄猜一个含义
            return Err(EINVAL);
        }
        if !first {
            out.push('/');
        }
        out.push_str(comp);
        first = false;
    }
    Ok(out)
}

/// 规范化之后的路径分量（`/a/b` → `["a", "b"]`；`/` → `[]`）。
pub fn components(path: &str) -> Vec<&str> {
    path.split('/').filter(|c| !c.is_empty()).collect()
}

/// `path` 是否等于 `point` 或位于其下（挂载点路由用）。
pub fn within(path: &str, point: &str) -> bool {
    if point == "/" {
        return true;
    }
    path == point || path.starts_with(&format!("{point}/"))
}

// ---- 系统级入口（经挂载表路由） ----

/// 取元数据。
pub fn stat(path: &str) -> Result<Metadata, i64> {
    let (fs, rel) = crate::fs::mount::table().resolve(path)?;
    fs.stat(&rel)
}

/// 读整个文件（所有权版本：调用方拿到 `Vec`）。
pub fn read_all(path: &str) -> Result<Vec<u8>, i64> {
    let (fs, rel) = crate::fs::mount::table().resolve(path)?;
    fs.read_all(&rel)
}

/// 从 `offset` 起读入 `buf`。
pub fn read(path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, i64> {
    let (fs, rel) = crate::fs::mount::table().resolve(path)?;
    fs.read(&rel, offset, buf)
}

/// 从 `offset` 起写 `data`。
pub fn write(path: &str, offset: u64, data: &[u8]) -> Result<usize, i64> {
    let (fs, rel) = crate::fs::mount::table().resolve(path)?;
    fs.write(&rel, offset, data)
}

/// 创建空文件。
pub fn create(path: &str) -> Result<(), i64> {
    let (fs, rel) = crate::fs::mount::table().resolve(path)?;
    fs.create(&rel)
}

/// 创建目录。
pub fn mkdir(path: &str) -> Result<(), i64> {
    let (fs, rel) = crate::fs::mount::table().resolve(path)?;
    fs.mkdir(&rel)
}

/// 列举目录。
pub fn list(path: &str) -> Result<Vec<DirEntry>, i64> {
    let (fs, rel) = crate::fs::mount::table().resolve(path)?;
    fs.list(&rel)
}

/// 后端统计 `(文件数, 占用字节数)`。
pub fn usage(path: &str) -> Result<(usize, usize), i64> {
    let (fs, _) = crate::fs::mount::table().resolve(path)?;
    Ok(fs.usage())
}

/// 某个路径是否是**挂载点本身**。
///
/// 与 [`backend_name`] 的区别很关键：`backend_name("/mnt/fat")` 在没有第二块盘时
/// 也会返回 `Some("ramfs-scratch")`（最长前缀落到挂在 `/mnt` 的后端），
/// 因此不能用它当作「FAT 卷已挂载」的证据。
pub fn is_mount_point(path: &str) -> bool {
    crate::fs::mount::table().is_mount_point(path)
}

/// 后端名字（日志用；路由到哪个文件系统是挂载表的可观测证据）。
pub fn backend_name(path: &str) -> Option<&'static str> {
    let (fs, _) = crate::fs::mount::table().resolve(path).ok()?;
    Some(fs.name())
}
