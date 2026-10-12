//! CKM 覆盖层（CKM-2：**机制**，不含 CKM-5 的 kind=3 全量策略）
//!
//! 权威文档：[`docs/ckm.md`](../../../docs/ckm.md) 第 6 节（模型 / 冲突检测 / 停用即撤销）。
//!
//! CKM-2 只交付**机制**，不交付「谁能注册」的完整策略：
//!
//! - [`register`] / [`unregister`]：覆盖表的增删（能力位 `CAP_CKM_SERVICE`，判定点 7/8）；
//! - [`lookup`]：**最长前缀 → 优先级最高**的查表（`docs/ckm.md` 6.3 的第 1 条）；
//! - [`rewrite`]：把「用户看到的路径」改写成覆盖区里的真实路径 —— 这是 `fs::mount`
//!   在路径解析时调用的钩子，因此**新 `open` 一定拿到覆盖后的视图**（6.4 第 2 条）；
//! - 冲突**拒绝后注册者**：同优先级且前缀重叠 → `op=overlay_conflict result=deny`（6.3 第 2 条）。
//!
//! 模型（6.1）：覆盖层的物理落点是 `/ckm/overlay/<id>/**`，注册表项只记
//! `(id, kind, priority, prefix)`：
//!
//! ```text
//! 基线路径                覆盖层（kind=3，按 priority）
//! /etc/motd    ← 最长前缀命中 →  /ckm/overlay/<id>/motd
//! ```
//!
//! **不做的事**（明确留给后续期，避免「机制期悄悄承担策略」）：
//!
//! - 不实现 `.mask` 显式隐藏（CKM-5）：CKM-2 的语义是「前缀命中即改写」，
//!   覆盖区里没有对应文件就是 `ENOENT` —— 这就是最朴素的「隐藏」，而 `mask`
//!   要做的是「有条目、但显式声明隐藏」，属于覆盖表全量策略；
//! - 不从磁盘/回执重建覆盖表（CKM-3/4）：第一版由内核内置注册（[`docs/ckm.md`](../../../docs/ckm.md) 6.2）；
//! - 不校验 `kind` 之外的扩展元数据（`.ckc` 清单属于 CKM-3）。
//!
//! 依赖方向：[`docs/ckm.md`](../../../docs/ckm.md) 4.1 的图里 `ckm/` 只依赖 `security/`；
//! 本模块额外只依赖 `ckm::{cred, policy, audit}` 与 `driver::serial`，**不**依赖 `fs/`
//! —— 需要文件的那些动作（建覆盖区、读写文件）都由 `fs/` 自己发起（`fs::overlay_selftest`），
//! 因为 `fs/ ──▶ ckm/` 才是允许的方向。

use alloc::format;
use alloc::string::String;

use crate::ckm::policy::{self, Op};
use crate::driver::serial;
use crate::security::Cred;

/// 覆盖表容量（固定容量 + 明确拒绝，与状态表/挂载表同风格）。
pub const ENTRY_MAX: usize = 16;

/// 覆盖层 id 长度上限（`/ckm/overlay/<id>` 的一级目录名）。
pub const ID_MAX: usize = 32;

/// 前缀长度上限。
pub const PREFIX_MAX: usize = 64;

/// 覆盖区的固定落点（`docs/ckm.md` 6.1）。
pub const OVERLAY_ROOT: &str = "/ckm/overlay";

/// 第一版只对 `kind=3`（系统修改模块）开放（6.1 最后一条）。
pub const OVERLAY_KIND: u8 = 3;

/// 非法参数（id/前缀不合法、kind 不是 3）。
pub const EINVAL: i64 = -22;
/// 前缀重叠且优先级相同 → 拒绝后注册者。
pub const EEXIST: i64 = -17;
/// 撤销一个不存在的 id。
pub const ENOENT: i64 = -2;
/// 覆盖表已满。
pub const ENOSPC: i64 = -28;

/// 一条覆盖表项。
#[derive(Clone)]
struct Entry {
    id: String,
    kind: u8,
    priority: u16,
    prefix: String,
}

// SAFETY: 内核单线程；`entries()` 是唯一的访问点（与状态表/卷表同一套契约）。
static mut ENTRIES: [Option<Entry>; ENTRY_MAX] = [const { None }; ENTRY_MAX];

fn entries() -> &'static mut [Option<Entry>; ENTRY_MAX] {
    unsafe { &mut *(&raw mut ENTRIES) }
}

/// 已注册的覆盖条目数。
pub fn count() -> usize {
    entries().iter().filter(|slot| slot.is_some()).count()
}

/// 表是否为空（`fs::mount` 的快路径：空表时**不**分配、不查表）。
pub fn is_empty() -> bool {
    entries().iter().all(|slot| slot.is_none())
}

/// 清空覆盖表（自测收尾 / 与 `ckm::init` 的「空表」约定一致）。
pub fn reset() {
    for slot in entries() {
        *slot = None;
    }
}

/// 第 `index` 条的 `(id, kind, priority, prefix)`（证据行与自测用；只读）。
pub fn entry_at(index: usize) -> Option<(&'static str, u8, u16, &'static str)> {
    let slot = entries().get(index)?.as_ref()?;
    Some((slot.id.as_str(), slot.kind, slot.priority, slot.prefix.as_str()))
}

/// 注册一条覆盖：`id` 是扩展身份，`kind` 必须是 [`OVERLAY_KIND`]，`priority` 越大越优先。
///
/// 冲突规则（`docs/ckm.md` 6.3）：
///
/// 1. 前缀**重叠**且优先级**不同** → 注册成功，优先级高的生效，低的**保留但不参与查表**
///    （证据行给出被遮蔽者的槽位）；
/// 2. 前缀重叠且优先级**相同** → 拒绝后注册者（`op=overlay_conflict result=deny`）；
/// 3. 前缀**不重叠** → 互不影响。
///
/// 非法输入（空 id、id 含 `/`、前缀不是绝对路径、前缀覆盖 `/ckm`、kind ≠ 3）一律拒绝，
/// 并且**同样留审计与证据** —— 「请求本身非法」与「能力不足」是两种可区分的失败。
pub fn register(id: &str, kind: u8, priority: u16, prefix: &str, cred: &Cred) -> Result<(), i64> {
    if !valid_id(id) {
        policy::reject(Op::OverlayRegister, cred, 0);
        serial::print("ckm: overlay register id=");
        serial::print(id);
        serial::println(" result=deny reason=id");
        return Err(EINVAL);
    }
    if kind != OVERLAY_KIND {
        policy::reject(Op::OverlayRegister, cred, 0);
        serial::print("ckm: overlay register id=");
        serial::print(id);
        serial::println(" result=deny reason=kind");
        return Err(EINVAL);
    }
    if !valid_prefix(prefix) {
        policy::reject(Op::OverlayRegister, cred, 0);
        serial::print("ckm: overlay register id=");
        serial::print(id);
        serial::println(" result=deny reason=prefix");
        return Err(EINVAL);
    }
    if entries().iter().flatten().any(|e| e.id == id) {
        policy::reject(Op::OverlayRegister, cred, 0);
        serial::print("ckm: overlay register id=");
        serial::print(id);
        serial::println(" result=deny reason=duplicate");
        return Err(EEXIST);
    }

    // 冲突扫描：先算出「谁是赢家/被遮蔽者」，再决定放行还是拒绝。
    let mut shadowed: Option<usize> = None;
    for (index, existing) in entries().iter().enumerate() {
        let Some(existing) = existing else { continue };
        if !overlaps(prefix, &existing.prefix) {
            continue;
        }
        if existing.priority == priority {
            // 6.3 第 2 条：同优先级冲突 —— 拒绝后注册者。
            policy::reject(Op::OverlayConflict, cred, index as u64);
            serial::print("ckm: overlay conflict id=");
            serial::print(id);
            serial::print(" vs=");
            serial::print(&existing.id);
            serial::print(" prefix=");
            serial::print(prefix);
            serial::println(" result=deny reason=priority");
            return Err(EEXIST);
        }
        // 优先级不同：记录被遮蔽者（优先级低的那一方不参与查表）。
        let loser = if existing.priority < priority {
            Some(index)
        } else {
            shadowed.or(Some(index))
        };
        if let Some(loser) = loser {
            shadowed = Some(loser);
        }
    }

    let Some(free) = entries().iter().position(|slot| slot.is_none()) else {
        policy::reject(Op::OverlayRegister, cred, 0);
        serial::println("ckm: overlay register result=deny reason=full");
        return Err(ENOSPC);
    };

    // 判定点 7：注册覆盖层需要 `CAP_CKM_SERVICE`（内核上下文持有全部能力）。
    let detail = shadowed.map_or(0, |index| index as u64);
    policy::check(Op::OverlayRegister, cred, detail)?;

    entries()[free] = Some(Entry {
        id: String::from(id),
        kind,
        priority,
        prefix: String::from(prefix),
    });
    serial::print("ckm: overlay register id=");
    serial::print(id);
    serial::print(" kind=");
    serial::print_dec(kind as u64);
    serial::print(" priority=");
    serial::print_dec(priority as u64);
    serial::print(" prefix=");
    serial::print(prefix);
    serial::print(" slot=");
    serial::print_dec(free as u64);
    if let Some(index) = shadowed {
        serial::print(" shadowed=");
        if let Some(shadowed_entry) = entries()[index].as_ref() {
            serial::print(&shadowed_entry.id);
        }
    }
    serial::println("");
    Ok(())
}

/// 撤销一个 id 的**全部**覆盖条目（`docs/ckm.md` 6.4 第 1 条）。
///
/// 「停用即撤销」在机制层就是这一条：移除后**新 `open` 一定回到基线视图** ——
/// 不需要「刷新已打开的文件」，因为 `fs::mount` 的钩子每次都重新查表。
pub fn unregister(id: &str, cred: &Cred) -> Result<usize, i64> {
    let Some(slot) = entries().iter().position(|slot| {
        slot.as_ref().map(|entry| entry.id.as_str()) == Some(id)
    }) else {
        serial::print("ckm: overlay unregister id=");
        serial::print(id);
        serial::println(" result=deny reason=missing");
        return Err(ENOENT);
    };
    // 与注册同一判定点（覆盖表变动 = 判定点 7）。
    policy::check(Op::OverlayRegister, cred, slot as u64)?;

    let mut removed = 0usize;
    for entry_slot in entries() {
        if entry_slot.as_ref().map(|entry| entry.id.as_str()) == Some(id) {
            *entry_slot = None;
            removed += 1;
        }
    }
    serial::print("ckm: overlay unregister id=");
    serial::print(id);
    serial::print(" removed=");
    serial::print_dec(removed as u64);
    serial::print(" remaining=");
    serial::print_dec(count() as u64);
    serial::println(" result=allow");
    Ok(removed)
}

/// 查表：返回命中的条目下标（前缀最长 → 优先级最高）。
///
/// `path` 必须是**已规范化**的路径（`fs::mount` 在 `best()` 之前调用 [`rewrite`]）。
pub fn lookup(path: &str) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (index, entry) in entries().iter().enumerate() {
        let Some(entry) = entry else { continue };
        if !covers(&entry.prefix, path) {
            continue;
        }
        let better = match best {
            None => true,
            Some(current) => {
                let current = entries()[current].as_ref()?;
                // 最长前缀优先；同前缀（不可能同优先级并存，见 register）比优先级。
                (entry.prefix.len(), entry.priority) > (current.prefix.len(), current.priority)
            }
        };
        if better {
            best = Some(index);
        }
    }
    best
}

/// 覆盖层重写：`Some(新路径)` 表示这条路径被覆盖层接管。
///
/// 新路径 = `/ckm/overlay/<id>` + 前缀之后的剩余部分（命中前缀本身时剩余为空）。
/// 每次改写都打一行证据：**「谁让这条路径换了视图」是可断言的**。
pub fn rewrite(path: &str) -> Option<String> {
    let index = lookup(path)?;
    let entry = entries().get(index)?.as_ref()?;
    let rest = &path[entry.prefix.len()..];
    let target = format!("{}/{}{}", OVERLAY_ROOT, entry.id, rest);
    serial::print("ckm: overlay path=");
    serial::print(path);
    serial::print(" -> ");
    serial::print(&target);
    serial::print(" id=");
    serial::print(&entry.id);
    serial::print(" priority=");
    serial::print_dec(entry.priority as u64);
    serial::println("");
    Some(target)
}

/// id 规则：非空、不超过 [`ID_MAX`]、只含 `[A-Za-z0-9._-]` 且不是 `.`/`..`
/// （它是一级目录名，必须能安全地拼进路径）。
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= ID_MAX
        && id != "."
        && id != ".."
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

/// 前缀规则：绝对路径、不是根、不以 `/` 结尾、不超过 [`PREFIX_MAX`]，
/// 且**不覆盖 `/ckm`** —— 覆盖区自己在 `/ckm/overlay` 下，若允许覆盖 `/ckm`
/// 就会出现「重写后的路径又被重写」的自指（无限递归）。
fn valid_prefix(prefix: &str) -> bool {
    prefix.starts_with('/')
        && prefix != "/"
        && !prefix.ends_with('/')
        && prefix.len() <= PREFIX_MAX
        && prefix != "/ckm"
        && !prefix.starts_with("/ckm/")
}

/// `prefix` 是否覆盖 `path`（等于它或位于它下面）。
fn covers(prefix: &str, path: &str) -> bool {
    path == prefix
        || (path.len() > prefix.len()
            && path.starts_with(prefix)
            && path.as_bytes()[prefix.len()] == b'/')
}

/// 两个前缀是否重叠（一个是另一个的前缀，或相等）。
///
/// 重叠才可能「同一基线路径被两个 kind=3 扩展覆盖」（`docs/ckm.md` 6.3）：
/// `/etc` 与 `/etc/motd` 是重叠的，`/etc` 与 `/usr` 不是。
fn overlaps(a: &str, b: &str) -> bool {
    covers(a, b) || covers(b, a)
}
