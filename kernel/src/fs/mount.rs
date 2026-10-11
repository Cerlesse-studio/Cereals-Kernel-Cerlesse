//! 挂载管理（v0.7 实现）
//!
//! v0.7 的挂载表很小但语义完整：**挂载点 → 后端文件系统**，按**最长前缀**
//! 路由（`/mnt/x` 命中挂在 `/mnt` 的后端而不是 `/` 上的根后端）。
//!
//! 它存在的意义不止「多一个 FS」：`resolve()` 是**唯一**把「用户看到的路径」
//! 变成「某个后端内的相对路径」的地方，`fs::vfs` 的每个系统级入口都经过它。
//! 因此「路由到哪里」是可以用串口断言验证的行为，而不是隐含在代码里。
//!
//! v0.7 不支持 `umount`：挂载表只增不减（内核里的 RAMFS 生命周期 = 内核生命周期）。

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::fs::vfs::{self, FileSystem};

/// 挂载点数量上限（远超 v0.7 的需要，留出 `/dev`、`/proc` 的位置）。
pub const MAX_MOUNTS: usize = 4;

/// 一个挂载点：路径 + 后端文件系统。
///
/// 后端用 `Box<dyn FileSystem>` **独占持有**：挂载表是访问后端的唯一入口
/// （`resolve()` 是唯一的取引用点），因此不存在两个 `&mut` 同时指向同一个后端
/// 的情形 —— 这也是为什么 RAMFS 实例不是 `static mut` 而是装箱后交给挂载表。
pub struct MountPoint {
    /// 挂载点路径（已规范化，`/` 表示根）
    pub point: String,
    /// 后端（生命周期 = 内核生命周期，由挂载表持有）
    pub fs: Box<dyn FileSystem>,
}

/// 挂载表。
pub struct MountTable {
    mounts: Vec<MountPoint>,
}

impl MountTable {
    /// 空表（`const`：可直接用于 `static mut`）。
    pub const fn new() -> Self {
        Self { mounts: Vec::new() }
    }

    /// 在 `point` 挂载 `fs`。
    ///
    /// 拒绝：非绝对路径（[`vfs::EINVAL`]）、重复挂载点（[`vfs::EEXIST`]）、
    /// 超过 [`MAX_MOUNTS`]（[`vfs::ENOSPC`]）。
    pub fn mount(&mut self, point: &str, fs: Box<dyn FileSystem>) -> Result<(), i64> {
        let point = vfs::normalize(point)?;
        if self.mounts.iter().any(|m| m.point == point) {
            return Err(vfs::EEXIST);
        }
        if self.mounts.len() >= MAX_MOUNTS {
            return Err(vfs::ENOSPC);
        }
        self.mounts.push(MountPoint { point, fs });
        Ok(())
    }

    /// 最长前缀匹配：返回（后端, 挂载点内的相对路径）。
    ///
    /// 收 `&'static mut self`：挂载表与后端都是静态对象，返回的引用可以活到
    /// 调用结束之后（`vfs::read_all` 需要读整个 `/init`）。
    pub fn resolve(
        &'static mut self,
        path: &str,
    ) -> Result<(&'static mut dyn FileSystem, String), i64> {
        let path = vfs::normalize(path)?;

        let mut best: Option<usize> = None;
        for (i, m) in self.mounts.iter().enumerate() {
            if !vfs::within(&path, &m.point) {
                continue;
            }
            let better = match best {
                None => true,
                // 最长前缀优先：挂在 /mnt 上的后端比 / 更具体
                Some(j) => m.point.len() > self.mounts[j].point.len(),
            };
            if better {
                best = Some(i);
            }
        }
        let idx = best.ok_or(vfs::ENOENT)?;

        // 先把相对路径算出来，再取后端引用（避免同时借用两个字段）
        let rel = {
            let point = &self.mounts[idx].point;
            if point == "/" {
                path
            } else {
                let rest = &path[point.len()..];
                if rest.is_empty() {
                    "/".to_string()
                } else {
                    rest.to_string()
                }
            }
        };
        Ok((&mut *self.mounts[idx].fs, rel))
    }
}

// 全局挂载表。内核里没有并发访问（`fs::init` 之后是只读路由），
// 与 `frame`/`ipc` 等模块一致使用 `static mut` + 访问函数。
static mut TABLE: MountTable = MountTable::new();

/// 全局挂载表。
pub fn table() -> &'static mut MountTable {
    // `&raw mut`：避免 `static_mut_refs` 警告（rust 2024 兼容性 lint），
    // 语义同 `&mut TABLE`（访问契约见上方注释）。
    unsafe { &mut *(&raw mut TABLE) }
}
