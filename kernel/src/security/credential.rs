//! 进程凭证（v1.0 落地；CKM 在其上补能力位，见 `docs/ckm.md` 3.1）
//!
//! 每个进程持有一份**不可伪造**的凭证：它由内核在 `fork`/`execve` 时决定，
//! 用户态既不能读改自己的 uid（没有 `setuid`），也不能声明自己想要的能力位。
//! 这是第一版权限模型的地基 —— 没有它，能力检查（`docs/ckm.md` 3.4）没有主体可挂靠。
//!
//! ```text
//! struct Cred { uid: u32, gid: u32, caps: u32 }
//! ```
//!
//! 固定的主体划分（单管理员模型，第一版刻意很小）：
//!
//! | uid | 主体 | 初始能力 | 说明 |
//! | ---- | ---- | ---- | ---- |
//! | 0 | 内核上下文 | 全部 | 内核自己发起的操作不受判定（内核不是「外部主体」） |
//! | 0 | `/init` | [`CAP_FS_SYSTEM_WRITE`] | 建立系统视图；**不给** CKM 管理能力（最小权限） |
//! | 1000 | 管理员（`/bin/sh`） | [`capability::ADMIN`] | 阶段 1 是内核常量，阶段 4 起由签名配置决定 |
//! | 1001 | 管理服务（`ckmd`） | [`capability::SERVICE`] | 能执行事务，但不能改策略（阶段 4 才出现） |
//! | ≥ 2000 | 普通用户 / 普通扩展（`/bin/hello`） | 无 | 默认拒绝 |
//!
//! **凭证只有一条改变路径**：`execve` 一个由 CKM 组件表登记的程序时换 uid/gid/能力，
//! 其余情况（`fork`、普通 `execve`）**完全沿袭父进程**。因此第一版没有提权路径。

use crate::security::capability;

/// 内核上下文的 uid（内核自己发起的操作不受判定）。
pub const KERNEL_UID: u32 = 0;
/// 管理员 uid（阶段 1 = 内核常量 `CKM_ADMIN_UID`；阶段 4 起由签名配置决定）。
pub const ADMIN_UID: u32 = 1000;
/// 管理服务 `ckmd` 的 uid（阶段 4 出现；常量先定义，避免将来改号）。
pub const SERVICE_UID: u32 = 1001;
/// 普通用户 / 普通扩展的 uid（[`Cred::plain`]）。
pub const PLAIN_UID: u32 = 2000;

/// 进程凭证：`uid` + 主组 `gid` + 能力位图 `caps`。
///
/// `caps` 是位图，判定只看「需要的位置是否都在持有位里」（`need & !caps == 0`），
/// 位定义在 [`crate::security::capability`]（唯一定义点，位号是 ABI）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cred {
    /// 用户
    pub uid: u32,
    /// 主组（第一版不支持附加组）
    pub gid: u32,
    /// 能力位图
    pub caps: u32,
}

impl Cred {
    /// 内核上下文的凭证：uid 0 + 全部能力位。
    ///
    /// 为什么内核持「全部」而不是「不参与判定」：判定点表里有一半操作
    /// （写 `/ckm/**`、重扫设备）在内核自测里也会被走到，若内核凭证是空的，
    /// 「允许」这条路径就永远没有执行者。源码级例外是**内核内部的 `fs::vfs` 调用
    /// 不经过 syscall 层**（不判定），而不是靠一张假凭证。
    pub const fn kernel() -> Self {
        Self { uid: KERNEL_UID, gid: KERNEL_UID, caps: capability::ALL }
    }

    /// `/init` 的凭证：uid 0 + [`CAP_FS_SYSTEM_WRITE`]（建立系统视图，**不给**管理能力）。
    pub const fn init() -> Self {
        Self { uid: KERNEL_UID, gid: KERNEL_UID, caps: capability::INIT }
    }

    /// 管理员（uid 1000）的凭证：CKM 管理 + 系统写入 + 审计读取 + 存储管理/格式化。
    pub const fn admin() -> Self {
        Self { uid: ADMIN_UID, gid: ADMIN_UID, caps: capability::ADMIN }
    }

    /// 管理服务（uid 1001）的凭证：事务能力 + 系统写入 + 审计读取（**不能改策略**）。
    pub const fn service() -> Self {
        Self { uid: SERVICE_UID, gid: SERVICE_UID, caps: capability::SERVICE }
    }

    /// 普通扩展（uid 2000）的凭证：**没有任何能力位**（默认拒绝）。
    pub const fn plain() -> Self {
        Self { uid: PLAIN_UID, gid: PLAIN_UID, caps: 0 }
    }

    /// 是否 root（uid 0）。POSIX 语义下 root 绕过文件 mode 检查
    /// （能力检查**不**绕过，见 `docs/ckm.md` 3.3）。
    pub const fn is_root(&self) -> bool {
        self.uid == KERNEL_UID
    }

    /// 能力位判定：`need & !caps == 0`。
    pub const fn caps_ok(&self, need: u32) -> bool {
        need & !self.caps == 0
    }
}
