//! 盘符表（CSS-3，`docs/storage.md` 3.1/4.1/4.3/6.6）
//!
//! 盘符是卷的**别名**，不是身份：`vol_id` 才是身份（3.4），所以这张表存的是
//! 「字母 → `vol_id`」的映射。这样一来的三个结论都能被断言：
//!
//! 1. **确定性**：同一组设备两次启动的字母完全相同 —— 缺省规则是纯计算
//!    （系统卷 = `C`，其余按 6.5 的枚举顺序 `D`/`E`/…），不读时钟、不用随机数；
//! 2. **不漂移**：管理员改过的字母写进 CKM 状态区（CKM-2 的双槽 A/B 区，
//!    键 [`STORE_KEY`]），重启后**先装盘上的映射再枚举**，因此插拔/重启都不改字母；
//! 3. **可拒绝**：字母被别的卷占着返回 [`EEXIST`]，非法字母返回 [`EINVAL`]，
//!    两种失败都不改变表（「固定容量 + 明确拒绝」与进程表/挂载表同一风格）。
//!
//! 为什么落在 `storage/` 而不是 `fs/`：盘符属于**卷管理器**（CSS），
//! 挂载点是 VFS 的事（4.2 的三条理由之一就是「字母表是内核维护、库只查询」）。
//! 用户态不硬编码字母：它读 `storage_volumes` 记录里的 `letter` 字段（偏移 24）。
//!
//! 持久化只写**自定义**字母（`custom = 1`）：缺省规则每次启动都能重算，
//! 把算出来的结果也写盘只会让「盘上的内容」与「规则」这两份真相不同步 ——
//! 一旦规则改了，盘上就会留下一条谁也说不清的旧映射。
//!
//! 键值编码（`storage/letters`，值 ≤ 255 字节，值是**人可读**的文本）：
//!
//! ```text
//! C=fs:1a2b3c4d;Z=raw:4
//! ```
//!
//! 容量：最多 [`PERSIST_MAX`] 条自定义映射（`12 × ~19 ≈ 228` 字节，留出余量）；
//! 超过就 `ENOSPC` 并且**不写入**。`;` / `=` / `:` 都是合法分隔符，卷标识的字符集
//! （`[A-Za-z0-9._:-]`，3.4）里不含它们，因此解析不需要转义。

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::driver::serial;

/// 盘符槽位数（与卷表容量一致：每个卷最多一个盘符）。
pub const LETTER_SLOTS: usize = crate::storage::MAX_VOLUMES;
/// 可持久化的自定义映射条数上限（值长 255 字节内，见模块头）。
pub const PERSIST_MAX: usize = 12;
/// 状态区里的键名（`docs/storage.md` 6.6 的命名空间 `storage/*`）。
pub const STORE_KEY: &str = "storage/letters";
/// 「没有盘符」的取值（记录里的 0）。
pub const NONE: u8 = 0;
/// 缺省分配的第一个字母：`A`/`B` 留给软驱的历史含义，不从它们开始。
pub const FIRST_DEFAULT: u8 = b'C';

/// `-EINVAL`：字母非法（不是 `A`..=`Z`）。
pub const EINVAL: i64 = -22;
/// `-EEXIST`：该字母已属于另一个卷。
pub const EEXIST: i64 = -17;
/// `-ENOENT`：卷不在卷表里。
pub const ENOENT: i64 = -2;
/// `-ENOSPC`：自定义映射超出可持久化容量。
pub const ENOSPC: i64 = -28;

/// 一条映射（字母 → 卷）。
struct Binding {
    /// 盘符（`A`..=`Z`）
    letter: u8,
    /// 卷标识（`vol_id`）
    vol: String,
    /// 本次启动真的在卷表里见到了这个卷（插拔的可观测面）
    present: bool,
    /// 管理员分配过（需要写盘）
    custom: bool,
}

/// 表（固定容量；`static mut` + 访问函数，与卷表/挂载表同一契约）。
static mut TABLE: [Option<Binding>; LETTER_SLOTS] = [const { None }; LETTER_SLOTS];
/// 本次启动从状态区装载的映射条数（`storage: letters load n=…` 的证据来源）。
static mut LOADED: usize = 0;

fn slots() -> &'static mut [Option<Binding>; LETTER_SLOTS] {
    // SAFETY: 静态表；内核里对它的访问都是单线程的启动/系统调用路径。
    unsafe { &mut *(&raw mut TABLE) }
}

fn table() -> &'static [Option<Binding>; LETTER_SLOTS] {
    // SAFETY: 只读访问静态表。
    unsafe { &*(&raw const TABLE) }
}

/// 清空（幂等；`storage::init` 开头调用 —— 重启/重扫都不该累积旧映射）。
pub fn reset() {
    for slot in slots() {
        *slot = None;
    }
    unsafe { LOADED = 0 };
}

/// 本次启动从状态区装载的映射条数。
pub fn loaded_from_store() -> usize {
    unsafe { LOADED }
}

/// 已登记的卷数（含**不在场**但被自定义映射保留着字母的卷）。
pub fn entry_count() -> usize {
    table().iter().filter(|slot| slot.is_some()).count()
}

/// 在场卷数（`storage: letters` 证据行用）。
pub fn present_count() -> usize {
    table()
        .iter()
        .flatten()
        .filter(|binding| binding.present)
        .count()
}

/// 查某个卷的盘符（没登记返回 `None`）。
pub fn letter_of(vol_id: &str) -> Option<u8> {
    table()
        .iter()
        .flatten()
        .find(|binding| binding.vol == vol_id)
        .map(|binding| binding.letter)
}

/// 某个字母当前的归属卷（空闲返回 `None`）。
pub fn owner_of(letter: u8) -> Option<&'static str> {
    table()
        .iter()
        .flatten()
        .find(|binding| binding.letter == letter)
        .map(|binding| binding.vol.as_str())
}

/// 字母是否已被占用（**包括当前不在场的卷** —— 这正是「拔插不漂移」的实现方式：
/// 字母留给那个卷，不会被别人拿走）。
fn taken(letter: u8) -> bool {
    owner_of(letter).is_some()
}

/// 从状态区装载自定义映射（`storage::init` 里、卷扫描**之前**调用）。
///
/// 返回装载条数。状态区缺席（没有块设备/没有 `ckm::store`）时返回 0 并留一行证据 ——
/// 「没有持久化」与「持久化里没有映射」因此可区分（与 `ckm::store::init` 三态一致）。
pub fn load_from_store() -> usize {
    let Some(raw) = crate::ckm::store::get(STORE_KEY) else {
        serial::println("storage: letters load n=0 source=none");
        unsafe { LOADED = 0 };
        return 0;
    };
    let text = core::str::from_utf8(raw).unwrap_or("");
    let mut count = 0usize;
    for part in text.split(';') {
        if part.is_empty() {
            continue;
        }
        let Some((letter, vol)) = part.split_once('=') else {
            serial::print("storage: letters warning malformed entry len=");
            serial::print_dec(part.len() as u64);
            serial::println("");
            continue;
        };
        let bytes = letter.as_bytes();
        if bytes.len() != 1 || !bytes[0].is_ascii_uppercase() || vol.is_empty() {
            serial::println("storage: letters warning malformed entry (letter/vol)");
            continue;
        }
        if count >= LETTER_SLOTS || insert_binding(bytes[0], vol, false, true).is_err() {
            serial::println("storage: letters warning table full while loading");
            break;
        }
        count += 1;
    }
    unsafe { LOADED = count };
    serial::print("storage: letters load n=");
    serial::print_dec(count as u64);
    serial::print(" source=");
    serial::print(if count > 0 { "store" } else { "none" });
    if count > 0 {
        serial::print(" keys=");
        serial::print(STORE_KEY);
    }
    serial::println("");
    count
}

/// 插一条映射（覆盖同卷的旧映射；字母冲突时返回 [`EEXIST`]）。
fn insert_binding(letter: u8, vol_id: &str, present: bool, custom: bool) -> Result<(), i64> {
    if let Some(owner) = owner_of(letter) {
        if owner != vol_id {
            return Err(EEXIST);
        }
    }
    let slots = slots();
    // 同一个卷只留一条：先摘掉本卷的旧映射
    for slot in slots.iter_mut() {
        if slot.as_ref().is_some_and(|b| b.vol == vol_id) {
            *slot = None;
        }
    }
    for slot in slots.iter_mut() {
        if slot.is_none() {
            *slot = Some(Binding {
                letter,
                vol: String::from(vol_id),
                present,
                custom,
            });
            return Ok(());
        }
    }
    Err(ENOSPC)
}

/// 标记「这个卷这次真的在场」（`registry` 扫描之后调用）。
pub fn mark_present(vol_id: &str) -> bool {
    for slot in slots().iter_mut().flatten() {
        if slot.vol == vol_id {
            slot.present = true;
            return true;
        }
    }
    false
}

/// 盘符的文本形式（`0` 或其他非法值 → `-`）。
///
/// 不分配、不用 `format!`：证据行与记录打印都走这一条，热路径上不该有一次堆分配。
pub fn ascii(letter: &u8) -> &'static str {
    const NAMES: [&str; 26] = [
        "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q",
        "R", "S", "T", "U", "V", "W", "X", "Y", "Z",
    ];
    match *letter {
        b'A'..=b'Z' => NAMES[(*letter - b'A') as usize],
        _ => "-",
    }
}

/// 给一个在场卷分配盘符（缺省规则）：已绑定 → 原字母；否则取最小的空闲字母。
///
/// `prefer_c` 只有系统卷（启动盘）为真 —— `docs/storage.md` 6.6 的「缺省 C 给系统卷」。
/// 字母用尽（26 个都在场或都被保留）时返回 [`NONE`]（记录里就是「无盘符」，
/// 并且写一行证据说明为什么 —— 不静默）。
pub fn default_for(vol_id: &str, prefer_c: bool) -> u8 {
    if let Some(letter) = letter_of(vol_id) {
        mark_present(vol_id);
        return letter;
    }
    if prefer_c && !taken(FIRST_DEFAULT) {
        let _ = insert_binding(FIRST_DEFAULT, vol_id, true, false);
        return FIRST_DEFAULT;
    }
    let mut letter = FIRST_DEFAULT;
    while letter <= b'Z' {
        if !taken(letter) {
            let _ = insert_binding(letter, vol_id, true, false);
            return letter;
        }
        letter += 1;
    }
    serial::print("storage: letters exhausted; volume without a drive letter id=");
    serial::println(vol_id);
    NONE
}

/// 管理员分配/取消盘符：`letter = 0` 表示取消（回到缺省规则）。
///
/// 校验顺序（**先校验后改表**：失败必须不改变任何状态）：
/// ① 字母合法（`A`..=`Z` 或 0）→ 否则 [`EINVAL`]；② 字母空闲或属于同一个卷 → 否则 [`EEXIST`]；
/// ③ 自定义映射的持久化编码装得下 → 否则 [`ENOSPC`]。
/// 通过之后才改内存表并写盘（写盘失败保留内存里的分配，但留警告行 ——
/// 「这次生效、下次启动不生效」必须说出来，不能假装写成功了）。
pub fn assign(vol_id: &str, letter: u8) -> Result<(), i64> {
    if letter == NONE {
        return clear(vol_id);
    }
    if !letter.is_ascii_uppercase() {
        return Err(EINVAL);
    }
    if let Some(owner) = owner_of(letter) {
        if owner != vol_id {
            return Err(EEXIST);
        }
    }
    // 候选集合（去掉本卷的旧映射 + 本条）必须先能编码，否则不改任何状态
    let mut candidate: Vec<(u8, &str)> = Vec::new();
    for binding in table().iter().flatten() {
        if binding.custom && binding.vol != vol_id {
            candidate.push((binding.letter, binding.vol.as_str()));
        }
    }
    candidate.push((letter, vol_id));
    if candidate.len() > PERSIST_MAX {
        return Err(ENOSPC);
    }
    let encoded = encode(&candidate);
    if encoded.len() > crate::ckm::store::VALUE_MAX {
        return Err(ENOSPC);
    }

    insert_binding(letter, vol_id, true, true)?;
    persist();
    Ok(())
}

/// 取消自定义映射（回到缺省规则；缺省字母由 [`registry::reassign`] 立刻重算）。
pub fn clear(vol_id: &str) -> Result<(), i64> {
    let mut found = false;
    let slots = slots();
    for (i, slot) in slots.iter_mut().enumerate() {
        if slot.as_ref().is_some_and(|b| b.vol == vol_id) {
            if slot.as_ref().is_some_and(|b| b.custom) {
                found = true;
            }
            let _ = i;
            *slot = None;
        }
    }
    if found {
        persist();
    }
    Ok(())
}

/// 把**自定义**映射写进状态区（缺省映射不写，理由见模块头）。
///
/// 状态区缺席时留一行警告并返回（不当作失败：没有持久化不等于字母不能用）。
fn persist() {
    let mut custom: Vec<(u8, &str)> = table()
        .iter()
        .flatten()
        .filter(|binding| binding.custom)
        .map(|binding| (binding.letter, binding.vol.as_str()))
        .collect();
    custom.sort_by_key(|(letter, _)| *letter);
    let encoded = encode(&custom);
    if !crate::ckm::store::enabled() {
        serial::print("storage: letters persist skipped (no state area) custom=");
        serial::print_dec(custom.len() as u64);
        serial::println("");
        return;
    }
    match crate::ckm::store::put(STORE_KEY, encoded.as_bytes()) {
        Ok(()) => {
            serial::print("storage: letters saved custom=");
            serial::print_dec(custom.len() as u64);
            serial::println("");
        }
        Err(errno) => {
            serial::print("storage: letters persist FAILED errno=");
            serial::print_dec(errno.unsigned_abs());
            serial::println(" (in-memory assignment only)");
        }
    }
}

/// 自定义映射的持久化编码（`C=fs:x;Z=raw:4`；条数为 0 时是空串）。
fn encode(entries: &[(u8, &str)]) -> String {
    let mut out = String::new();
    for (i, (letter, vol)) in entries.iter().enumerate() {
        if i != 0 {
            out.push(';');
        }
        out.push(*letter as char);
        out.push('=');
        out.push_str(vol);
    }
    out
}

/// 逐卷核对「字母表 ↔ 卷记录」一致（自测的步骤 1/5 共用）。
///
/// 重复身份（克隆卷，`health=degraded`）的**第二个**卷是例外：一个字母不能同时是
/// 两个卷的入口，因此它记录里是 `0` 而表里指向第一个卷 —— 这里把它当断言的一部分
/// （「旁支必须是 0」），而不是放过它。少了这条例外，克隆夹具的盘会让自测直接 FAIL
/// （那说明表与记录真的不一致，只是原因不是代码错）。
fn check_consistency(checks: &mut usize) -> Result<(), &'static str> {
    let mut seen: Vec<String> = Vec::new();
    for i in 0..crate::storage::MAX_VOLUMES {
        let Some(volume) = crate::storage::registry::volume_at(i) else {
            continue;
        };
        let id = String::from(volume.id.as_str());
        if seen.contains(&id) {
            if volume.letter != NONE {
                return Err("storage: a duplicate vol_id got its own drive letter");
            }
            *checks += 1;
            continue;
        }
        seen.push(id.clone());
        let letter = letter_of(&id)
            .ok_or("storage: a present volume has no drive-letter mapping")?;
        if volume.letter != letter {
            return Err("storage: the volume record letter disagrees with the letter table");
        }
        if !letter.is_ascii_uppercase() {
            return Err("storage: a drive letter is outside A..=Z");
        }
        *checks += 1;
    }
    Ok(())
}

/// CSS-3 自测：**默认规则 + 冲突拒绝 + 持久化往返**（`make test-storage` 的断言来源）。
///
/// 为什么自测要真的写一次盘：CKM-2 的状态区是「跨重启」的介质，只证明
/// 「`put` 返回 Ok」等于什么都没证明。这里按 `ckm/store.rs::selftest` 的同一手法做
/// **幂等写**：键不存在 → 给最末一个在场卷分配 `Z`（自定义）→ 断言键里能解析出它；
/// 键已存在（第二次启动）→ 断言它被装载进来了。跨重启的一致性由
/// `tests/test_ckm_store.py` 的两次启动给出（`letter=Z` 只可能来自盘上）。
pub fn selftest() -> Result<(), &'static str> {
    let mut checks = 0usize;
    let count = crate::storage::registry::volume_count();
    if count == 0 {
        serial::println("storage: letters selftest skipped (no volume)");
        return Ok(());
    }

    // 1) 每个在场卷都登记了盘符，且记录字段与表一致（记录是消费者读的东西）
    check_consistency(&mut checks)?;

    // 2) 反例：占用中的字母、非法字母、未知卷 —— 三种失败都不改变表
    let first = crate::storage::registry::volume_at(0).ok_or("storage: no volume at slot 0")?;
    let first_id = String::from(first.id.as_str());
    let first_letter = first.letter;
    let before = entry_count();
    if assign(&first_id, b'z') != Err(EINVAL) {
        return Err("storage: a lowercase drive letter was accepted");
    }
    if crate::storage::registry::assign_letter(&first_id, b'1') != Err(EINVAL) {
        return Err("storage: a non-alphabetic drive letter was accepted");
    }
    if crate::storage::registry::assign_letter("raw:99", b'Y') != Err(ENOENT) {
        return Err("storage: assigning a drive letter to an unknown volume was accepted");
    }
    checks += 3;

    // 2b) 占用中的字母 → `EEXIST`（且**不改**任何状态：另一个卷的字母不能被动）
    let taken_letter = (0..crate::storage::MAX_VOLUMES)
        .filter_map(|i| crate::storage::registry::volume_at(i))
        .map(|v| v.letter)
        .find(|letter| *letter != 0 && *letter != first_letter);
    match taken_letter {
        Some(letter) => {
            if crate::storage::registry::assign_letter(&first_id, letter) != Err(EEXIST) {
                return Err("storage: assigning an occupied drive letter was accepted");
            }
            if crate::storage::registry::find(&first_id).map(|v| v.letter) != Some(first_letter) {
                return Err("storage: a rejected drive-letter assignment changed the volume");
            }
            serial::print("storage: letters conflict rejected letter=");
            serial::println(ascii(&letter));
            checks += 1;
        }
        None => serial::println("storage: letters conflict test skipped (single volume)"),
    }

    // 3) 持久化往返（幂等：第二次启动只断言「装载到了」）
    //
    // 探针用 `Z`：它与缺省序列（C/D/E…）不重叠，因此「这个字母是人改出来的」
    // 在串口上可以直接看出来。取消测试**不能用同一个卷** —— 那会把刚写下的映射擦掉，
    // 第二次启动就无事可断言了（“自测把证据删了”是这类测试最常见的假通过）。
    //
    // 探针**只打非系统卷**：系统卷的 `C` 是缺省规则的一部分，自测不该动它 ——
    // 「自测不留副作用」与覆盖层/审计环的做法一致（那些自测收尾时也会把状态还原）。
    // 单盘启动（只有系统卷）时跳过，并留一行证据：没有可打的探针不是失败。
    let last = (0..crate::storage::MAX_VOLUMES)
        .rev()
        .find_map(|i| crate::storage::registry::volume_at(i))
        .filter(|v| v.disk_id != 0);
    let Some(last) = last else {
        serial::println("storage: letters persistence probe skipped (system volume only)");
        return finish(checks);
    };
    let last_id = String::from(last.id.as_str());
    let fresh = crate::ckm::store::get(STORE_KEY).is_none();
    if fresh {
        // 第一次启动：分配一个自定义字母
        crate::storage::registry::assign_letter(&last_id, b'Z')
            .map_err(|_| "storage: assigning the persistence probe letter failed")?;
        let raw =
            crate::ckm::store::get(STORE_KEY).ok_or("storage: the letter mapping was not persisted")?;
        let text = core::str::from_utf8(raw).unwrap_or("");
        if !text.contains(&format!("Z={last_id}")) {
            return Err("storage: the persisted letter mapping does not contain the probe");
        }
        if letter_of(&last_id) != Some(b'Z') {
            return Err("storage: the in-memory letter table lost the probe letter");
        }
        let volume = crate::storage::registry::find(&last_id)
            .ok_or("storage: the probe volume left the table")?;
        if volume.letter != b'Z' {
            return Err("storage: the volume record did not get the probe letter");
        }
        serial::print("storage: letters persisted probe=Z vol=");
        serial::print(&last_id);
        serial::println("");
        checks += 3;
    } else {
        // 第二次启动：映射是从盘上装载的（`load_from_store` 已经解析过）
        if loaded_from_store() == 0 {
            return Err("storage: the state area has a letters key but nothing was loaded");
        }
        if letter_of(&last_id) != Some(b'Z') {
            return Err("storage: the persisted letter mapping was not applied to the volume");
        }
        serial::print("storage: letters reloaded from store probe=Z vol=");
        serial::print(&last_id);
        serial::println("");
        checks += 2;
    }

    // 4) 取消（`letter = 0`）：自定义映射从表里消失、从持久化值里消失，
    //    并且**缺省规则立刻补一个**（取消不是「没有盘符」，而是「回到规则」）。
    //    只在有**第二个**卷时做 —— 探针那个卷的映射要留到第二次启动。
    // 取消测试只在新钥的启动上跑：第二次启动必须是**只读**的（`state=loaded` 之后
    // 一个字也不写），否则「重启后读回的是盘上的字节」这条验收就无从断言 ——
    // 与 `ckm/store.rs::selftest` 的幂等写同一个理由。
    let second = if fresh {
        (0..crate::storage::MAX_VOLUMES)
            .filter_map(|i| crate::storage::registry::volume_at(i))
            .find(|v| v.id.as_str() != last_id)
            .map(|v| String::from(v.id.as_str()))
    } else {
        None
    };
    if !fresh {
        serial::println("storage: letters clear test skipped (read-only boot)");
    }
    match second {
        Some(second_id) => {
            crate::storage::registry::assign_letter(&second_id, b'Y')
                .map_err(|_| "storage: assigning the clear-probe letter failed")?;
            if letter_of(&second_id) != Some(b'Y') {
                return Err("storage: the clear probe was not applied");
            }
            crate::storage::registry::assign_letter(&second_id, NONE)
                .map_err(|_| "storage: clearing the clear-probe letter failed")?;
            let fallback = letter_of(&second_id)
                .ok_or("storage: clearing a letter did not fall back to the default rule")?;
            if fallback == b'Y' {
                return Err("storage: the cleared letter was re-assigned to the same volume");
            }
            if let Some(raw) = crate::ckm::store::get(STORE_KEY) {
                let text = core::str::from_utf8(raw).unwrap_or("");
                if text.contains(&format!("Y={second_id}")) {
                    return Err("storage: the persisted mapping still holds a cleared letter");
                }
                if !text.contains(&format!("Z={last_id}")) {
                    return Err("storage: clearing one letter dropped another volume's mapping");
                }
            }
            serial::print("storage: letters cleared probe=Y vol=");
            serial::print(&second_id);
            serial::print(" fallback=");
            serial::println(ascii(&fallback));
            checks += 3;
        }
        None => {
            serial::println("storage: letters clear test skipped (single volume)");
        }
    }

    // 5) 取消之后表与记录仍然一致（缺省字母与记录字段逐卷对齐）
    check_consistency(&mut checks)
        .map_err(|_| "storage: letter table and volume record diverged after clearing")?;
    // 步骤 2 的反例必须**没有**改动表：期望值在步骤 2 之前就取好了
    let _ = (first_letter, before, first_id);

    finish(checks)
}

/// 自测收尾（快路径与完整路径共用同一行证据行）。
fn finish(checks: usize) -> Result<(), &'static str> {
    serial::print("storage: letters selftest PASS (checks=");
    serial::print_dec(checks as u64);
    serial::println(")");
    Ok(())
}
