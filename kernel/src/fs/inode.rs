//! VFS 节点元数据（v0.7 实现）
//!
//! v0.7 的 VFS 只有两种节点：**文件**（一段字节）与**目录**（名字 → 子节点映射）。
//! 没有 inode 号、没有硬链接、没有设备节点 —— 这些都建立在 v0.8 的磁盘 FS 之上。
//!
//! `inode.rs` 只放「与后端无关」的形状：节点类型、元数据、目录项。
//! 具体存储（`Vec<u8>` + 子节点表）在 [`crate::fs::ramfs`]。

use alloc::string::String;

/// 节点类型。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NodeKind {
    /// 普通文件：一段字节 + 读/写偏移语义
    File,
    /// 目录：子节点集合（v0.7 不支持 `..`，见 `fs::vfs::normalize`）
    Dir,
}

impl NodeKind {
    /// 是否为目录。
    pub const fn is_dir(self) -> bool {
        matches!(self, NodeKind::Dir)
    }

    /// 串口日志里使用的稳定名字（断言依赖它，不要随手改）。
    pub const fn name(self) -> &'static str {
        match self {
            NodeKind::File => "file",
            NodeKind::Dir => "dir",
        }
    }
}

/// 节点元数据。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Metadata {
    /// 节点类型
    pub kind: NodeKind,
    /// 文件字节数（目录恒为 0）
    pub size: u64,
}

impl Metadata {
    /// 普通文件的元数据。
    pub const fn file(size: u64) -> Self {
        Self { kind: NodeKind::File, size }
    }

    /// 目录的元数据。
    pub const fn dir() -> Self {
        Self { kind: NodeKind::Dir, size: 0 }
    }

    /// `ls -l` 风格的权限串。
    ///
    /// v0.7 没有用户/组/权限位（v1.0 才做 `security/`），因此权限是**固定**的：
    /// 文件 `-rw-r--r--`、目录 `drwxr-xr-x`。这个串会出现在 `fs: ls` 日志里，
    /// 让 RAMFS 的目录列举有可断言的形状。
    pub const fn perms(self) -> &'static str {
        match self.kind {
            NodeKind::File => "-rw-r--r--",
            NodeKind::Dir => "drwxr-xr-x",
        }
    }
}

/// 目录项：`list()` 的返回元素。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DirEntry {
    /// 条目名（不包含 `/`）
    pub name: String,
    /// 条目元数据
    pub meta: Metadata,
}
