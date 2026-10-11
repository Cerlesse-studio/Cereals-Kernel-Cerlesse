//! RAMFS 内存文件系统（v0.7 实现，第一个 VFS 后端）
//!
//! 存储模型（刻意做成「一台机器上最直接能对的形状」）：
//!
//! ```text
//! RamFs { root: Node }
//! Node  { name, kind, data: Vec<u8>, children: Vec<Node> }
//! ```
//!
//! 目录就是 `children` 非空的 `Node`，文件就是 `data`。没有 inode 号、没有块
//! 分配、没有日志：数据直接放在内核堆里（`Vec<u8>`），因此 [`RamFs`] 必须在
//! `memory::init`（内核堆就绪）之后才使用。
//!
//! 为什么 v0.7 先做 RAMFS 而不是磁盘 FS：
//!
//! - 它让 VFS / syscall 的**文件语义**可以先立起来并被断言（`open/read/write/close`
//!   与挂载点路由都能在 QEMU 串口上验证），不必先解决驱动与块设备；
//! - 磁盘 FS（FAT32 on virtio-blk）是 v0.8 的事，那时 `FileSystem` trait 已经稳定，
//!   加后端只需再实现一遍它。
//!
//! 上限：单文件 [`MAX_FILE_BYTES`]（防用户态一次 `write` 把内核堆写爆），
//! 目录深度与条目数不设限（受内核堆总量约束）。

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::driver::serial;
use crate::fs::inode::{DirEntry, Metadata, NodeKind};
use crate::fs::vfs::{self, FileSystem};

/// 单文件字节上限（256 KiB）。
pub const MAX_FILE_BYTES: usize = 256 * 1024;

/// 一个节点（文件或目录）。
struct Node {
    name: String,
    kind: NodeKind,
    /// 文件内容（目录恒为空）
    data: Vec<u8>,
    /// 子节点（文件恒为空）
    children: Vec<Node>,
}

impl Node {
    /// 空目录。
    fn dir(name: &str) -> Self {
        Self {
            name: name.to_string(),
            kind: NodeKind::Dir,
            data: Vec::new(),
            children: Vec::new(),
        }
    }

    /// 空文件。
    fn file(name: &str) -> Self {
        Self {
            name: name.to_string(),
            kind: NodeKind::File,
            data: Vec::new(),
            children: Vec::new(),
        }
    }

    /// 元数据。
    fn meta(&self) -> Metadata {
        match self.kind {
            NodeKind::File => Metadata::file(self.data.len() as u64),
            NodeKind::Dir => Metadata::dir(),
        }
    }

    /// 按路径分量找子节点（共享借用）。
    fn walk(&self, comps: &[&str]) -> Result<&Node, i64> {
        match comps.split_first() {
            None => Ok(self),
            Some((head, rest)) => {
                if !self.kind.is_dir() {
                    return Err(vfs::ENOTDIR);
                }
                let child = self
                    .children
                    .iter()
                    .find(|c| c.name == *head)
                    .ok_or(vfs::ENOENT)?;
                child.walk(rest)
            }
        }
    }

    /// 按路径分量找子节点（可变借用）。
    fn walk_mut(&mut self, comps: &[&str]) -> Result<&mut Node, i64> {
        match comps.split_first() {
            None => Ok(self),
            Some((head, rest)) => {
                if !self.kind.is_dir() {
                    return Err(vfs::ENOTDIR);
                }
                let child = self
                    .children
                    .iter_mut()
                    .find(|c| c.name == *head)
                    .ok_or(vfs::ENOENT)?;
                child.walk_mut(rest)
            }
        }
    }
}

/// RAMFS 实例：一个根目录。
pub struct RamFs {
    name: &'static str,
    root: Node,
    /// 已创建的文件数（统计日志用）
    files: usize,
    /// 已占用总字节数
    bytes: usize,
}

impl RamFs {
    /// 新建 RAMFS。
    ///
    /// 实例由 [`crate::fs::mount::MountTable`] 装箱独占持有（不是 `static mut`）：
    /// 挂载表是访问后端的唯一入口，避免两个 `&mut` 同时指向同一个文件系统。
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            root: Node::dir(""),
            files: 0,
            bytes: 0,
        }
    }

    /// 文件数与占用字节数（`fs: ramfs files=.. bytes=..` 日志用）。
    pub fn usage(&self) -> (usize, usize) {
        (self.files, self.bytes)
    }

    /// 在 `parent` 下新建 `name` 节点；已存在则报 [`vfs::EEXIST`]。
    fn insert(parent: &mut Node, name: &str, kind: NodeKind) -> Result<(), i64> {
        if parent.children.iter().any(|c| c.name == name) {
            return Err(vfs::EEXIST);
        }
        let node = match kind {
            NodeKind::File => Node::file(name),
            NodeKind::Dir => Node::dir(name),
        };
        parent.children.push(node);
        Ok(())
    }
}

impl FileSystem for RamFs {
    fn name(&self) -> &'static str {
        self.name
    }

    fn usage(&self) -> (usize, usize) {
        RamFs::usage(self)
    }

    fn stat(&self, path: &str) -> Result<Metadata, i64> {
        let path = vfs::normalize(path)?;
        Ok(self.root.walk(&vfs::components(&path))?.meta())
    }

    fn read<'a>(&'a self, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, i64> {
        let path = vfs::normalize(path)?;
        let node = self.root.walk(&vfs::components(&path))?;
        if node.kind.is_dir() {
            return Err(vfs::EISDIR);
        }
        let data = &node.data;
        let offset = offset as usize;
        if offset >= data.len() {
            return Ok(0); // EOF（offset == len 也返回 0）
        }
        let n = (data.len() - offset).min(buf.len());
        buf[..n].copy_from_slice(&data[offset..offset + n]);
        Ok(n)
    }

    fn read_all<'a>(&'a self, path: &str) -> Result<&'a [u8], i64> {
        let path = vfs::normalize(path)?;
        let node = self.root.walk(&vfs::components(&path))?;
        if node.kind.is_dir() {
            return Err(vfs::EISDIR);
        }
        Ok(&node.data)
    }

    fn write(&mut self, path: &str, offset: u64, data: &[u8]) -> Result<usize, i64> {
        let path = vfs::normalize(path)?;
        let node = self.root.walk_mut(&vfs::components(&path))?;
        if node.kind.is_dir() {
            return Err(vfs::EISDIR);
        }
        let offset = offset as usize;
        if offset > node.data.len() {
            // 不允许打洞（稀疏文件是 v0.8 磁盘 FS 的话题）
            return Err(vfs::EINVAL);
        }
        if offset + data.len() > MAX_FILE_BYTES {
            return Err(vfs::ENOSPC);
        }
        let before = node.data.len();
        if offset == before {
            node.data.extend_from_slice(data);
        } else {
            let end = offset + data.len();
            if end > before {
                node.data.resize(end, 0);
            }
            node.data[offset..end].copy_from_slice(data);
        }
        // 统计：只记增量（覆盖写不增加）
        self.bytes += node.data.len() - before;
        Ok(data.len())
    }

    fn create(&mut self, path: &str) -> Result<(), i64> {
        let path = vfs::normalize(path)?;
        let comps = vfs::components(&path);
        let name = match comps.last() {
            Some(name) => *name,
            None => return Err(vfs::EINVAL), // 不允许创建根
        };
        let parent = self.root.walk_mut(&comps[..comps.len() - 1])?;
        if !parent.kind.is_dir() {
            return Err(vfs::ENOTDIR);
        }
        Self::insert(parent, name, NodeKind::File)?;
        self.files += 1;
        serial::print("fs: create ");
        serial::println(&path);
        Ok(())
    }

    fn mkdir(&mut self, path: &str) -> Result<(), i64> {
        let path = vfs::normalize(path)?;
        let comps = vfs::components(&path);
        let name = match comps.last() {
            Some(name) => *name,
            None => return Err(vfs::EINVAL),
        };
        let parent = self.root.walk_mut(&comps[..comps.len() - 1])?;
        if !parent.kind.is_dir() {
            return Err(vfs::ENOTDIR);
        }
        Self::insert(parent, name, NodeKind::Dir)?;
        serial::print("fs: mkdir ");
        serial::println(&path);
        Ok(())
    }

    fn list(&self, path: &str) -> Result<Vec<DirEntry>, i64> {
        let path = vfs::normalize(path)?;
        let node = self.root.walk(&vfs::components(&path))?;
        if !node.kind.is_dir() {
            return Err(vfs::ENOTDIR);
        }
        Ok(node
            .children
            .iter()
            .map(|child| DirEntry {
                name: child.name.clone(),
                meta: child.meta(),
            })
            .collect())
    }
}
