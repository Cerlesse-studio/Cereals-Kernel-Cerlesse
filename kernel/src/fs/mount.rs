//! 挂载管理（v0.7 实现；CSS-2 收口）
//!
//! 挂载表的语义是：**挂载点 → 后端文件系统**，按**最长前缀**路由
//! （`/mnt/x` 命中挂在 `/mnt` 的后端而不是 `/` 上的根后端）。
//!
//! 它存在的意义不止「多一个 FS」：`resolve()` 是**唯一**把「用户看到的路径」
//! 变成「某个后端内的相对路径」的地方，`fs::vfs` 的每个系统级入口都经过它。
//! 因此「路由到哪里」是可以用串口断言验证的行为，而不是隐含在代码里。
//!
//! ## CSS-2 的三处收口（见 `docs/storage.md` 5.2）
//!
//! 1. **后端与挂载点分开存**（`backends` / `mounts`）：一个后端可以被**多个**
//!    挂载点引用（同一个卷挂到 `/volumes/<label>` 与 `/media/<label>` 是**同一份
//!    数据**，不是两次打开）。v0.7 的「挂载点里直接放 `Box<dyn FileSystem>`」
//!    做不到这件事 —— 那个形状里「挂载点数 = 后端数」是隐含假设。
//! 2. **挂载点带卷身份**（`vol_id`）与**选项**（只读挂载）：卷的身份以
//!    `storage::registry` 的卷表为准（CSS-1 的稳定标识），挂载表只是引用它；
//!    挂载/卸载同时更新卷记录里的 `mount_count`（`docs/storage.md` 6.4 偏移 56）。
//! 3. **错误是 errno**（`EEXIST` / `ENOSPC` / `ENOENT` / `EINVAL`），不再是
//!    `&'static str` —— 否则「拒绝理由」不可断言（`docs/storage.md` 4.5）。
//!
//! ## `resolve()` 返回 `&'static mut` 的安全论据
//!
//! 后端由 `backends: Vec<Box<dyn FileSystem>>` **独占持有**，`resolve()` 是唯一
//! 取可变引用的地方，且单个调用内只取一个；两个挂载点指向同一个后端时，两次
//! `resolve()` 之间必然隔着一次完整的 read/stat（借用已结束），内核又是单线程的
//! （没有并发访问挂载表）。这与 v0.7 起一直使用的 `static mut TABLE` 是同一套契约。

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::fs::vfs::{self, FileSystem, MountOptions};

/// 挂载点数量上限。
///
/// v0.8 用到 4 个（`/` RAMFS、`/mnt` 自测用 RAMFS、`/disk` 启动盘 FAT、`/mnt/fat` 测试盘 FAT32），
/// CSS-2 用第 5 个做「同一卷的第二个挂载点」，留出 `/dev`、`/proc` 的位置。
pub const MAX_MOUNTS: usize = 8;

/// 一个挂载点：路径 + 它引用的后端 + 卷身份 + 选项。
pub struct MountEntry {
    /// 挂载点路径（已规范化，`/` 表示根）
    pub point: String,
    /// `backends` 里的下标（**不是**「第几个挂载点」：两个挂载点可以指同一个后端）
    pub backend: usize,
    /// 卷身份（`storage::registry` 的 `vol_id`）；非卷挂载（RAMFS 根）为 `None`
    pub vol_id: Option<String>,
    /// 挂载选项（CSS-2 起）
    pub options: MountOptions,
}

/// 路径解析结果（证据行与「路径 → 卷」断言用）。
pub struct Route {
    /// 命中的挂载点（最长前缀）
    pub point: String,
    /// 后端名字
    pub backend: &'static str,
    /// 该挂载点的卷身份（非卷挂载为 `None`）
    pub vol_id: Option<String>,
    /// 挂载选项
    pub options: MountOptions,
}

/// 挂载表：后端存储 + 挂载点条目。
pub struct MountTable {
    backends: Vec<Box<dyn FileSystem>>,
    mounts: Vec<MountEntry>,
}

impl MountTable {
    /// 空表（`const`：可直接用于 `static mut`）。
    pub const fn new() -> Self {
        Self { backends: Vec::new(), mounts: Vec::new() }
    }

    /// 在 `point` 挂载 `fs`（**非卷**挂载：RAMFS 等；等价于「卷身份未知」）。
    ///
    /// 拒绝：非绝对路径（[`vfs::EINVAL`]）、重复挂载点（[`vfs::EEXIST`]）、
    /// 超过 [`MAX_MOUNTS`]（[`vfs::ENOSPC`]）。
    pub fn mount(&mut self, point: &str, fs: Box<dyn FileSystem>) -> Result<(), i64> {
        if self.backends.len() >= MAX_MOUNTS {
            return Err(vfs::ENOSPC);
        }
        self.backends.push(fs);
        let backend = self.backends.len() - 1;
        self.push(point, backend, None, MountOptions::RW)
    }

    /// 在 `point` 挂载**卷**（CSS-2）：带 `vol_id` 与挂载选项，并同步卷记录的挂载计数。
    pub fn mount_volume(
        &mut self,
        point: &str,
        fs: Box<dyn FileSystem>,
        vol_id: &str,
        options: MountOptions,
    ) -> Result<(), i64> {
        if self.backends.len() >= MAX_MOUNTS {
            return Err(vfs::ENOSPC);
        }
        self.backends.push(fs);
        let backend = self.backends.len() - 1;
        self.push(point, backend, Some(vol_id.to_string()), options)
    }

    /// 给**已挂载的卷**再加一个挂载点（CSS-2：同一卷多挂载点）。
    ///
    /// 不走 `backends`（**不重新打开设备**），因此两个路径下看到的是同一个后端实例、
    /// 同一份数据；`vol_id` 未挂载时返回 [`vfs::ENOENT`]。
    pub fn alias_volume(
        &mut self,
        point: &str,
        vol_id: &str,
        options: MountOptions,
    ) -> Result<(), i64> {
        let backend = self
            .mounts
            .iter()
            .find(|m| m.vol_id.as_deref() == Some(vol_id))
            .map(|m| m.backend)
            .ok_or(vfs::ENOENT)?;
        self.push(point, backend, Some(vol_id.to_string()), options)
    }

    /// 卸载一个挂载点（CSS-2）。返回被卸载条目的卷身份（没有卷身份则为 `None`）。
    ///
    /// - 卸载的是**别名**：卷本身留在卷表里（`mounts` 计数减一），再次挂载不需要重新探测设备；
    /// - 根挂载点 `/` 不允许卸载（[`vfs::EINVAL`]）—— 那是这个系统唯一的根文件系统；
    /// - 后端存储在 `backends` 里**不释放**（可能还有别的挂载点引用它，而且释放会让
    ///   已交付的 `&'static mut` 悬空）。
    pub fn umount(&mut self, point: &str) -> Result<Option<String>, i64> {
        let point = vfs::normalize(point)?;
        if point == "/" {
            return Err(vfs::EINVAL);
        }
        let index = self
            .mounts
            .iter()
            .position(|m| m.point == point)
            .ok_or(vfs::ENOENT)?;
        let removed = self.mounts.remove(index);
        self.sync_counts();
        Ok(removed.vol_id)
    }

    /// 某个（已规范化的）路径是否是**挂载点本身**。
    ///
    /// 注意它与 `resolve()` 的区别：`resolve()` 回答「谁会处理这个路径」
    /// （`/mnt/fat/x` 会被挂在 `/mnt` 的后端处理），而这里回答「这个路径上是否真的
    /// 有东西被挂上来」。自测需要后者：没有第二块盘时 `/mnt/fat` 依然**能被解析**
    /// （最长前缀落到 `/mnt`），但不该被当成「FAT 卷已挂载」。
    pub fn is_mount_point(&self, path: &str) -> bool {
        matches!(vfs::normalize(path), Ok(point) if self.mounts.iter().any(|m| m.point == point))
    }

    /// 最长前缀匹配：返回（后端, 挂载点内的相对路径）。
    ///
    /// 收 `&'static mut self`：挂载表与后端都是静态对象，返回的引用可以活到
    /// 调用结束之后（`vfs::read_all` 需要读整个 `/init`）。
    pub fn resolve(
        &'static mut self,
        path: &str,
    ) -> Result<(&'static mut dyn FileSystem, String), i64> {
        let (index, rel) = self.best(path)?;
        let backend = self.mounts[index].backend;
        // SAFETY 论据见模块头「resolve() 返回 &'static mut 的安全论据」：
        // `self` 本身是 `&'static mut`，后端由 `backends` 独占持有。
        Ok((&mut *self.backends[backend], rel))
    }

    /// 路径解析到的挂载点信息（`fs: route …` 证据行与「路径 → 卷」断言用）。
    pub fn route(&self, path: &str) -> Option<Route> {
        let normalized = vfs::normalize(path).ok()?;
        let mut best: Option<usize> = None;
        for (i, m) in self.mounts.iter().enumerate() {
            if !vfs::within(&normalized, &m.point) {
                continue;
            }
            let better = match best {
                None => true,
                Some(j) => m.point.len() > self.mounts[j].point.len(),
            };
            if better {
                best = Some(i);
            }
        }
        let entry = &self.mounts[best?];
        Some(Route {
            point: entry.point.clone(),
            backend: self.backends[entry.backend].name(),
            vol_id: entry.vol_id.clone(),
            options: entry.options,
        })
    }

    /// `vol_id` 当前的挂载点数（卷记录 `mounts` 字段的写入值）。
    pub fn count_mounts(&self, vol_id: &str) -> u32 {
        self.mounts
            .iter()
            .filter(|m| m.vol_id.as_deref() == Some(vol_id))
            .count() as u32
    }

    /// 把「哪个卷挂了几个点」重新写回卷表。
    ///
    /// 调用时机：挂载/卸载之后（`push`/`umount` 内部），以及**重扫之后**
    /// （`storage::registry` 会重建卷记录，把 `mounts` 归零 —— 那时必须按挂载表
    /// 重放一次，否则 `/bin/volumes` 会看到 `mounts=0` 而实际挂着）。
    pub fn sync_counts(&mut self) {
        let mut ids: Vec<String> = Vec::new();
        for entry in &self.mounts {
            if let Some(id) = &entry.vol_id {
                if !ids.iter().any(|seen| seen == id) {
                    ids.push(id.clone());
                }
            }
        }
        for id in ids {
            let count = self.count_mounts(&id);
            crate::storage::registry::set_mount_count(&id, count);
        }
    }

    /// 校验挂载点、查重、压入条目并同步计数。
    fn push(
        &mut self,
        point: &str,
        backend: usize,
        vol_id: Option<String>,
        options: MountOptions,
    ) -> Result<(), i64> {
        let point = vfs::normalize(point)?;
        if self.mounts.iter().any(|m| m.point == point) {
            return Err(vfs::EEXIST);
        }
        if self.mounts.len() >= MAX_MOUNTS {
            return Err(vfs::ENOSPC);
        }
        self.mounts.push(MountEntry { point, backend, vol_id, options });
        self.sync_counts();
        Ok(())
    }

    /// 最长前缀匹配的内部实现：返回（条目下标, 挂载点内相对路径）。
    fn best(&self, path: &str) -> Result<(usize, String), i64> {
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
        let index = best.ok_or(vfs::ENOENT)?;

        // 先把相对路径算出来，再让调用方取后端引用（避免同时借用两个字段）
        let rel = {
            let point = &self.mounts[index].point;
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
        Ok((index, rel))
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
