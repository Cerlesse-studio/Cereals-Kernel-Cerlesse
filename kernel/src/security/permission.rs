//! 文件权限判定（v1.0 落地：mode 位 + 属主 uid/gid + `EACCES`）
//!
//! 这一层回答 **POSIX 意义上的**「这个主体能不能读/写/执行这个文件」，
//! 与能力检查（`ckm::policy`，失败 `EPERM(-1)`）刻意分开（`docs/ckm.md` 3.3）：
//!
//! | 失败 | errno | 含义 | 谁判定 |
//! | ---- | ---- | ---- | ---- |
//! | mode 不允许 | `-EACCES` = -13 | 「这个文件不属于你」 | 本模块 |
//! | 能力不足 | `-EPERM` = -1 | 「你没有这个能力」（受保护前缀） | `ckm::policy` |
//!
//! 第一版的简化（明确写出来，避免当成 bug）：
//!
//! 1. **只有主组**：不做附加组（`supplementary groups`）；
//! 2. **uid 0 绕过 mode**：POSIX root 语义（但**不**绕过能力检查）；
//! 3. **目录只判写位**：`O_CREAT` 要求父目录可写；不判搜索位（`x`）与 `sticky`；
//! 4. **不做 ACL / 标签**：见 `docs/ckm.md` 第 12 节开放问题 1。

use crate::security::credential::Cred;

/// `-EACCES`：mode 不允许（POSIX）。
pub const EACCES: i64 = -13;
/// `-EPERM`：能力不足（受保护前缀）。数值来源与 `ckm::policy` 共用。
pub const EPERM: i64 = -1;

/// mode 位（`-rwxr-xr-x` 的数字表示）与两个默认值。
pub mod mode {
    /// 文件默认 mode（`Metadata::file`）：`-rw-r--r--`。
    pub const FILE_DEFAULT: u16 = 0o644;
    /// 目录默认 mode（`Metadata::dir`）：`drwxr-xr-x`。
    pub const DIR_DEFAULT: u16 = 0o755;

    /// 属主读。
    pub const OWNER_READ: u16 = 0o400;
    /// 属主写。
    pub const OWNER_WRITE: u16 = 0o200;
    /// 属主执行。
    pub const OWNER_EXEC: u16 = 0o100;
    /// 同组读。
    pub const GROUP_READ: u16 = 0o040;
    /// 同组写。
    pub const GROUP_WRITE: u16 = 0o020;
    /// 同组执行。
    pub const GROUP_EXEC: u16 = 0o010;
    /// 其他人读。
    pub const OTHER_READ: u16 = 0o004;
    /// 其他人写。
    pub const OTHER_WRITE: u16 = 0o002;
    /// 其他人执行。
    pub const OTHER_EXEC: u16 = 0o001;
}

/// 想要的访问类型。
///
/// 第一版只有**读**与**写**两个判定点（`docs/ckm.md` 3.4 #1/#7）：
/// `execve` 要求程序文件**可读**（规格原文），因此没有独立的「执行位」判定 ——
/// `x` 位的检查等真正的「可执行文件」语义（脚本解释器、`/bin` 白名单之外的路径）
/// 成为需求时再加，而不是先写一个没人调用的分支。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Want {
    /// 读（`open(O_RDONLY)`、`execve` 装载映像）
    Read,
    /// 写（`open(O_WRONLY|O_RDWR)`、`O_CREAT` 对父目录）
    Write,
}

impl Want {
    /// 串口证据里的短名（`fs: EACCES … want=write`）。
    pub const fn name(self) -> &'static str {
        match self {
            Want::Read => "read",
            Want::Write => "write",
        }
    }

    /// 三类主体各自需要的 mode 位：`(属主, 同组, 其他人)`。
    const fn bits(self) -> (u16, u16, u16) {
        match self {
            Want::Read => (mode::OWNER_READ, mode::GROUP_READ, mode::OTHER_READ),
            Want::Write => (mode::OWNER_WRITE, mode::GROUP_WRITE, mode::OTHER_WRITE),
        }
    }
}

/// 判定 `cred` 对 `(mode, owner_uid, owner_gid)` 的文件是否有 `want` 权限。
///
/// 通过返回 `Ok(())`；不允许返回 `Err(`[`EACCES`]`)`。选哪一组位只看主体：
/// uid 与属主相同 → 属主位；否则 gid 与属主组相同 → 同组位；否则其他人位。
pub fn check(
    mode_bits: u16,
    owner_uid: u32,
    owner_gid: u32,
    cred: &Cred,
    want: Want,
) -> Result<(), i64> {
    // POSIX root 语义：uid 0 绕过 mode 检查（能力检查仍然生效）。
    if cred.is_root() {
        return Ok(());
    }
    let (owner, group, other) = want.bits();
    let allowed = if cred.uid == owner_uid {
        mode_bits & owner != 0
    } else if cred.gid == owner_gid {
        mode_bits & group != 0
    } else {
        mode_bits & other != 0
    };
    if allowed {
        Ok(())
    } else {
        Err(EACCES)
    }
}
