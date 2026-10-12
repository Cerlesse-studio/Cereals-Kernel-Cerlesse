//! 安全子系统（v1.0 落地：凭证 + 文件 mode + 能力位）
//!
//! 这一层是**叶子被依赖者**：它只回答「谁（凭证）能否对某个对象（mode）做某事」，
//! 不依赖 `fs/`、`syscall/`、`ckm/`（`ckm/` 反过来依赖它，见 `docs/ckm.md` 4.1）。
//!
//! | 文件 | 职责 | 权威文档 |
//! | ---- | ---- | ---- |
//! | [`credential`] | `Cred { uid, gid, caps }`、固定主体划分（内核 / `/init` / 管理员 / 普通） | `docs/ckm.md` 3.1 |
//! | [`capability`] | 6 个能力位的**位定义**（位号是 ABI：审计记录的 `need` 字段就是它） | `docs/ckm.md` 3.2 |
//! | [`permission`] | 文件 mode 的读/写/执行判定，失败 `EACCES(-13)` | `docs/ckm.md` 3.3 |
//!
//! 为什么「凭证」与「能力位」分在两个文件、而判定在第三个：
//!
//! - `credential.rs` 是 **v1.0** 的产物（uid/gid + mode），CKM 只是给同一个结构
//!   补上 `caps` 字段并把它装进进程控制块；
//! - `capability.rs` 只放**位常量**：`0x4` 这类数字会出现在串口证据与审计记录里，
//!   因此必须只有一个定义点，`ckm::policy` 引用它而不是自己再写一遍；
//! - `permission.rs` 是 POSIX 语义的检查（「不属于你」），与能力检查
//!   （「你没有这个能力」→ `EPERM(-1)`）**刻意分开**，两者的 errno 在测试里必须可区分。

pub mod capability;
pub mod credential;
pub mod permission;

/// 进程凭证类型：内核其它模块（进程表字段、`ckm::policy` 的参数）只需要这一个名字，
/// 因此在这里转出一份，其余常量按模块路径引用，避免第二个定义点。
pub use credential::Cred;
