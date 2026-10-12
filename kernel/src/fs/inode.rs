//! VFS 节点元数据（v0.7 实现）
//!
//! v0.7 的 VFS 只有两种节点：**文件**（一段字节）与**目录**（名字 → 子节点映射）。
//! 没有 inode 号、没有硬链接、没有设备节点 —— 这些都建立在 v0.8 的磁盘 FS 之上。
//!
//! `inode.rs` 只放「与后端无关」的形状：节点类型、元数据、目录项。
//! 具体存储（`Vec<u8>` + 子节点表）在 [`crate::fs::ramfs`]。

use alloc::string::String;

use crate::security::permission::mode;

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
///
/// v1.0 起带 **mode + 属主 uid/gid**（`security/permission.rs` 的判定输入）。
/// 两个后端目前都用默认值（文件 `0644` root、目录 `0755` root），因为两者都还没有
/// 「谁创建了它」的概念：RAMFS 由内核建立、FAT 是只读探测（写拒绝由后端的 `EROFS` 给出）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Metadata {
    /// 节点类型
    pub kind: NodeKind,
    /// 文件字节数（目录恒为 0）
    pub size: u64,
    /// mode 位（`0o644` 之类，见 `security::permission::mode`）
    pub mode: u16,
    /// 属主 uid
    pub uid: u32,
    /// 属主主组 gid
    pub gid: u32,
}

impl Metadata {
    /// 普通文件的元数据（`-rw-r--r--`、属主 root）。
    pub const fn file(size: u64) -> Self {
        Self {
            kind: NodeKind::File,
            size,
            mode: crate::security::permission::mode::FILE_DEFAULT,
            uid: 0,
            gid: 0,
        }
    }

    /// 目录的元数据（`drwxr-xr-x`、属主 root）。
    pub const fn dir() -> Self {
        Self {
            kind: NodeKind::Dir,
            size: 0,
            mode: crate::security::permission::mode::DIR_DEFAULT,
            uid: 0,
            gid: 0,
        }
    }

    /// `ls -l` 风格的权限串（`-rw-r--r--` / `drwxr-xr-x`）。
    ///
    /// v1.0 起由 mode 位真正算出来（不再是写死的两个字符串），因此支持
    /// 任意 mode；默认值算出的串与 v0.7 的硬编码串**逐字节相同**，
    /// 既有 `fs: ls` 断言不受影响。调用方提供一个 10 字节缓冲，避免 `alloc`。
    pub fn perms_str<'a>(&self, buf: &'a mut [u8; 10]) -> &'a str {
        buf[0] = match self.kind {
            NodeKind::Dir => b'd',
            NodeKind::File => b'-',
        };
        const TRIPLETS: [[(u16, u8); 3]; 3] = [
            [
                (mode::OWNER_READ, b'r'),
                (mode::OWNER_WRITE, b'w'),
                (mode::OWNER_EXEC, b'x'),
            ],
            [
                (mode::GROUP_READ, b'r'),
                (mode::GROUP_WRITE, b'w'),
                (mode::GROUP_EXEC, b'x'),
            ],
            [
                (mode::OTHER_READ, b'r'),
                (mode::OTHER_WRITE, b'w'),
                (mode::OTHER_EXEC, b'x'),
            ],
        ];
        for (triplet, entry) in TRIPLETS.iter().enumerate() {
            for (i, (bit, ch)) in entry.iter().enumerate() {
                buf[1 + triplet * 3 + i] = if self.mode & bit != 0 { *ch } else { b'-' };
            }
        }
        core::str::from_utf8(&buf[..]).unwrap_or("----------")
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
