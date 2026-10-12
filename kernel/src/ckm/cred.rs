//! 凭证与能力位判定（`docs/ckm.md` 3.1/3.2）
//!
//! 这一层回答「**现在是谁**」，并负责第一版唯一的凭证改变路径：
//! `execve` 一个**由组件表登记**的程序时换 uid/gid/能力。
//!
//! 两条必须成立的规则（`docs/ckm.md` 3.2）：
//!
//! 1. **能力不可继承性提升**：`fork`/`execve` 不增加能力。组件路径给的是
//!    「该组件登记的能力」，**不是**父进程与组件能力的并集（[`exec_cred`]）；
//! 2. **没有提权路径**：第一版没有 `setuid`/`setgid`/`su`。「谁能做什么」由内核的
//!    组件表 + 常量决定，用户态无法自己声明。
//!
//! 内核上下文（没有当前进程，例如启动早期的内核自测）用 [`Cred::kernel`]：
//! 判定点表里有一半操作在内核自测里也会被走到，若内核凭证是空的，
//! 「允许」这条路径就永远没有执行者。**内核内部的 `fs::vfs` 调用不经过 syscall 层**
//! （因此不判定），而不是靠一张假凭证。

use crate::process::process;
use crate::security::Cred;

/// 当前进程的凭证；没有当前进程（内核上下文）时返回 [`Cred::kernel`]。
pub fn current() -> Cred {
    process::current().map(|p| p.cred).unwrap_or_else(Cred::kernel)
}

/// 当前进程 pid（内核上下文 = 0）。
///
/// 审计记录与串口证据都要它：**「谁干的」是拒绝可核查的前提**
/// （`docs/ckm.md` 4.4 的 `pid` 字段）。
pub fn current_pid() -> u64 {
    process::current().map(|p| p.pid).unwrap_or(0)
}

/// `execve` 之后的凭证：**登记过的组件程序**换 uid/gid/能力，其余完全沿袭父进程。
///
/// 命中组件表时同时产生一条 `exec_component` 判定证据
/// （`ckm: policy op=exec_component pid=N uid=<父> result=allow need=0x0 detail=0x<槽位>`），
/// 因此「哪个进程在哪次 exec 上换了身份」在串口上可断言。
/// `detail` 是槽位下标而不是 uid：槽位能定位到具体的状态表项，uid 不能（多个组件可以同 uid）。
pub fn exec_cred(path: &str, parent: Cred) -> Cred {
    match crate::ckm::state::component_by_path(path) {
        Some((slot, s)) => {
            let _ = crate::ckm::policy::check(
                crate::ckm::policy::Op::ExecComponent,
                &parent,
                slot as u64,
            );
            Cred {
                uid: s.uid,
                gid: s.gid,
                caps: s.caps,
            }
        }
        None => parent,
    }
}

/// 内核直接运行的顶层程序（`/init`、`/bin/sh`、`/bin/dynhello`…）的初始凭证。
///
/// - 组件表登记过的（`/init`、`/bin/sh`）→ 该组件的 uid/gid/能力；
/// - 其余（例如 `/bin/dynhello`、隔离探针）→ [`Cred::plain`]：**最小权限**启动。
///
/// 为什么「其余」不是内核凭证：内核直接运行一个用户程序，与「用户自己 execve 它」
/// 是同一件事的两种入口，不该因为入口不同而多给权限。真正需要系统权限的
/// 只有 `/init`（建立系统视图），而它是**显式登记**的。
pub fn program_cred(path: &str) -> Cred {
    match crate::ckm::state::component_by_path(path) {
        Some((_, s)) => Cred {
            uid: s.uid,
            gid: s.gid,
            caps: s.caps,
        },
        None => Cred::plain(),
    }
}
