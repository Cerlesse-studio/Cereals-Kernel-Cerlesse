//! 卷登记表（CSS-1 实现）
//!
//! 它是 `storage_volumes` 的数据源：枚举顺序**必须确定性**（`docs/storage.md` 6.5）——
//! `disk_id` 升序 → `part_index` 升序 → `id` 字节序升序。两次调用对同一组设备
//! 必须给出逐字节相同的记录（`selftest` 会真的比对两次快照）。
//!
//! 固定容量 [`MAX_VOLUMES`] 槽位、满了明确拒绝（与进程表/挂载表同样的风格）：
//! 「悄悄丢掉一个卷」比「报一句満了」难查得多。
//!
//! 卷表只增不减，除了 [`rescan_disk`]：重扫前先摘掉该磁盘的旧记录，
//! 否则拔盘重插会留下指向旧盘的幽灵卷。

use alloc::string::String;
use alloc::vec::Vec;

use crate::driver::block;
use crate::driver::serial;
use crate::storage::partition::{self, TableKind};
use crate::storage::volume::{self, Health, Volume, VOLUME_RECORD_SIZE};
use crate::storage::MAX_VOLUMES;

/// 卷表（固定容量；`static mut` + 访问函数，与 `mount`/`frame` 一致）。
static mut VOLUMES: [Option<Volume>; MAX_VOLUMES] = [const { None }; MAX_VOLUMES];

/// 槽位满的告警只打一次，避免每次重扫刷屏。
static mut FULL_WARNED: bool = false;

fn slots() -> &'static mut [Option<Volume>; MAX_VOLUMES] {
    // SAFETY: 卷表是静态量；内核里对它的读写都是单线程的启动/系统调用路径。
    unsafe { &mut *(&raw mut VOLUMES) }
}

/// 第 `i` 个槽位里的卷（空槽返回 `None`）。
pub fn volume_at(i: usize) -> Option<&'static Volume> {
    // SAFETY: 只读访问静态卷表。
    unsafe {
        let table = &*(&raw const VOLUMES);
        table.get(i)?.as_ref()
    }
}

/// 当前登记的有效卷数。
pub fn volume_count() -> usize {
    let mut count = 0;
    for i in 0..MAX_VOLUMES {
        if volume_at(i).is_some() {
            count += 1;
        }
    }
    count
}

/// 按稳定标识找卷（CSS-2：挂载表用它把「挂载点」接到「卷」上）。
pub fn find(vol_id: &str) -> Option<&'static Volume> {
    for i in 0..MAX_VOLUMES {
        if let Some(volume) = volume_at(i) {
            if volume.id.as_str() == vol_id {
                return Some(volume);
            }
        }
    }
    None
}

/// 按「块设备号 + 分区号」找卷（CSS-2：挂载一块整盘时用它取卷身份）。
///
/// 分区表里的卷 `part_index >= 1`；无分区表的整盘卷是 `part_index == 0`。
pub fn find_at(disk_id: u16, part_index: u8) -> Option<&'static Volume> {
    for i in 0..MAX_VOLUMES {
        if let Some(volume) = volume_at(i) {
            if volume.disk_id == disk_id && volume.part_index == part_index {
                return Some(volume);
            }
        }
    }
    None
}

/// 更新某卷的挂载点数（记录偏移 56）。
///
/// **唯一写入方是挂载表**（`fs::mount::MountTable::sync_counts`）：挂载计数是
/// 「挂载表的状态」而不是「卷自身的属性」，因此不允许别处各自 `+= 1` ——
/// 那种写法迟早会漏掉一次卸载，然后永远多 1。返回是否找到该卷。
pub fn set_mount_count(vol_id: &str, count: u32) -> bool {
    for i in 0..MAX_VOLUMES {
        if let Some(slot) = slots().get_mut(i).and_then(|s| s.as_mut()) {
            if slot.id.as_str() == vol_id {
                slot.mount_count = count;
                return true;
            }
        }
    }
    false
}

/// 扫描全部块设备并（可选）重建卷表。
///
/// 证据行的顺序是「磁盘/分区行 → （冲突检测后）卷行」：卷的健康状态会被**卷表整体**
/// 决定（3.4 的克隆卷冲突要靠全表才看得出来），所以卷行必须等 `detect_conflicts`
/// 之后才打 —— 否则冲突的那个卷会先打一行 `health=ok`，与后面那行冲突证据自相矛盾。
pub fn scan_all(reset: bool) -> Result<usize, &'static str> {
    if reset {
        for slot in slots() {
            *slot = None;
        }
    }
    for index in 0..block::count() {
        scan_disk(index)?;
    }
    detect_conflicts();
    apply_letters();
    for index in 0..block::count() {
        log_disk_volumes(index);
    }
    // 重建出来的卷记录里 `mounts` 是 0，但已经挂着的卷并没有卸载 ——
    // 按挂载表重放一次（CSS-2：`/bin/volumes` 看到的计数必须与挂载表一致）。
    crate::fs::mount::table().sync_counts();
    Ok(volume_count())
}

/// 重扫一个磁盘（`storage_rescan` 的落点）：先摘旧记录，再按当前内容重建。
pub fn rescan_disk(index: usize) -> Result<usize, &'static str> {
    if index >= block::count() {
        return Err("storage: rescan of a missing device");
    }
    for slot in slots() {
        if slot.as_ref().is_some_and(|v| v.disk_id as usize == index) {
            *slot = None;
        }
    }
    scan_disk(index)?;
    detect_conflicts();
    apply_letters();
    log_disk_volumes(index);
    crate::fs::mount::table().sync_counts();
    serial::print("storage: rescan disk=");
    serial::print_dec(index as u64);
    serial::print(" volumes=");
    serial::print_dec(volume_count() as u64);
    serial::println("");
    Ok(volume_count())
}

/// 打印属于 `disk_id` 的卷的证据行（表内插入顺序 = 分区升序，确定性）。
fn log_disk_volumes(disk_id: usize) {
    for i in 0..MAX_VOLUMES {
        if let Some(volume) = volume_at(i) {
            if volume.disk_id as usize == disk_id {
                volume.log();
            }
        }
    }
}

/// 扫描一个磁盘：分区表 → 每个分区的文件系统探测 → 登记 + 打印证据。
fn scan_disk(index: usize) -> Result<(), &'static str> {
    let Some(dev) = block::get(index) else {
        return Err("storage: missing block device");
    };
    let disk_id = index as u16;
    let scan = partition::scan(&mut *dev, disk_id)?;

    serial::print("storage: disk index=");
    serial::print_dec(index as u64);
    serial::print(" dev=");
    serial::print(dev.name());
    serial::print(" sectors=");
    serial::print_dec(dev.sectors());
    serial::print(" table=");
    serial::print(scan.evidence_table);
    serial::print(" partitions=");
    serial::print_dec(scan.partitions.len() as u64);
    if scan.degraded {
        serial::print(" health=degraded");
    }
    serial::println("");

    for part in &scan.partitions {
        if part.table != TableKind::None {
            part.log();
        }
        let probe = volume::probe(&mut *dev, part.start_lba, part.sectors);
        let volume = volume::make_volume(disk_id, part.index, part.start_lba, &probe, scan.degraded);
        match insert(volume) {
            // 卷的证据行**不在这里打**：要等所有盘扫完、冲突检测跑过（见 `scan_all`）
            Some(_) => {}
            None => {
                if !unsafe { FULL_WARNED } {
                    serial::println("storage: warning volume table full; extra volumes dropped");
                    unsafe { FULL_WARNED = true };
                }
            }
        }
    }
    Ok(())
}

/// 把一个卷放进第一个空槽，返回槽位下标；满则返回 `None`。
fn insert(volume: Volume) -> Option<usize> {
    let slots = slots();
    for (i, slot) in slots.iter_mut().enumerate() {
        if slot.is_none() {
            *slot = Some(volume);
            return Some(i);
        }
    }
    None
}

// ---- CSS-3：盘符（`docs/storage.md` 3.1/4.1/6.6）----

/// 某卷当前的盘符（`0` = 未分配）。
pub fn letter_of(vol_id: &str) -> u8 {
    crate::storage::letters::letter_of(vol_id).unwrap_or(crate::storage::letters::NONE)
}

/// 给在场卷分配盘符并把结果写回记录（扫描/重扫之后调用）。
///
/// 顺序**必须**与 6.5 的枚举顺序一致（`disk_id` → `part_index` → `id`），但系统卷
/// （启动盘 = `disk_id 0`）先拿 `C`：缺省规则要可复述，「谁拿了 C」不能取决于
/// 分区表里谁排在前。自定义映射（盘上装载的）在这个函数里只是**被沿用**：
/// `letters::default_for` 见到已有绑定就直接返回它，因此重启不漂移。
fn apply_letters() {
    let mut order = [0usize; MAX_VOLUMES];
    let mut count = 0;
    for i in 0..MAX_VOLUMES {
        if volume_at(i).is_some() {
            order[count] = i;
            count += 1;
        }
    }
    for a in 0..count {
        for b in a + 1..count {
            if matches!(before(order[b], order[a]), Ok(true)) {
                order.swap(a, b);
            }
        }
    }

    // 分配顺序：系统卷（启动盘）先拿 C，其余按 6.5 的枚举顺序拿 D/E/…。
    //
    // 两遍都必须对**已有绑定**的卷调 `bind_one`：绑定可能来自状态区（重启后的
    // 自定义字母），而记录里的 `letter` 字段是刚扫出来的 0 ——「跳过已有绑定」
    // 会把盘上的 `Z` 留在字母表里、却让卷行打印 `letter=-`。
    let mut queue: Vec<usize> = Vec::with_capacity(count * 2);
    for &slot in order[..count].iter() {
        if volume_at(slot).is_some_and(|v| v.disk_id == 0) {
            queue.push(slot);
        }
    }
    for &slot in order[..count].iter() {
        if volume_at(slot).is_some_and(|v| v.disk_id != 0) {
            queue.push(slot);
        }
    }
    // `seen` 是「已分配过的身份」：`vol_id` 相同的第二个卷（克隆盘/克隆卷，
    // 已在 `detect_conflicts` 里标 degraded）不带盘符 —— 盘符是卷的**入口**，
    // 一个字母不能同时是两个卷的入口。
    let mut seen: Vec<String> = Vec::with_capacity(count);
    for slot in queue {
        bind_one(slot, &mut seen);
    }

    serial::print("storage: letters table n=");
    serial::print_dec(crate::storage::letters::entry_count() as u64);
    serial::print(" present=");
    serial::print_dec(crate::storage::letters::present_count() as u64);
    serial::print(" first=");
    serial::println(crate::storage::letters::ascii(&crate::storage::letters::FIRST_DEFAULT));
}

/// 把一个在场卷的盘符定下来（已有绑定就用它，否则按缺省规则分配）并写回记录。
///
/// 唯一的分配入口：`apply_letters` 按「系统卷优先」的顺序逐个调它，「谁拿 C」
/// 因此只取决于 `disk_id == 0` 这条规则，而不取决于分区表里谁排在前。
/// `seen` 保存已经分配过的 `vol_id`：重复身份（克隆卷）的第二个卷不带盘符。
fn bind_one(slot: usize, seen: &mut Vec<String>) {
    let Some(volume) = volume_at(slot) else {
        return;
    };
    let id = String::from(volume.id.as_str());
    if seen.iter().any(|s| s == &id) {
        set_letter_at(slot, crate::storage::letters::NONE);
        return;
    }
    seen.push(id.clone());
    // 在场标记：表里可能有**不在场**的卷（拔掉的盘留着字母，插回来还是原来那个 —— 3.1）。
    crate::storage::letters::mark_present(&id);
    let letter = match crate::storage::letters::letter_of(&id) {
        Some(letter) => letter,
        None => crate::storage::letters::default_for(&id, volume.disk_id == 0),
    };
    set_letter_at(slot, letter);
}

/// 按**槽位**写盘符（按身份写会在克隆卷上打到错的槽 —— 两个槽的 `id` 相同）。
fn set_letter_at(slot: usize, letter: u8) {
    if let Some(entry) = slots().get_mut(slot).and_then(|s| s.as_mut()) {
        entry.letter = letter;
    }
}

/// 按身份找槽位（`assign_letter` 用：改盘符要落在**具体的那个卷**上）。
pub fn slot_of(vol_id: &str) -> Option<usize> {
    for i in 0..MAX_VOLUMES {
        if volume_at(i).is_some_and(|v| v.id.as_str() == vol_id) {
            return Some(i);
        }
    }
    None
}

/// 分配/取消盘符（受控动作，判定点 8；`docs/ckm.md` 3.4 / `storage.md` 6.3 的 264）。
///
/// `letter == 0` 表示**取消自定义映射并回到缺省规则**（不是「没有盘符」）。
/// 失败必须不改变任何状态：卷不存在 → [`letters::ENOENT`]；字母非法 →
/// [`letters::EINVAL`]；字母被别的卷占着 → [`letters::EEXIST`]；自定义映射超出
/// 持久化容量 → [`letters::ENOSPC`]。成功时留一行 `storage: letter …` 证据。
pub fn assign_letter(vol_id: &str, letter: u8) -> Result<(), i64> {
    let Some(volume) = find(vol_id) else {
        return Err(crate::storage::letters::ENOENT);
    };
    let system = volume.disk_id == 0;
    let id = String::from(volume.id.as_str());

    if letter == crate::storage::letters::NONE {
        crate::storage::letters::clear(&id)?;
        let fallback = crate::storage::letters::default_for(&id, system);
        if let Some(slot) = slot_of(&id) {
            set_letter_at(slot, fallback);
        }
        log_letter(&id, fallback, false);
        return Ok(());
    }

    crate::storage::letters::assign(&id, letter)?;
    if let Some(slot) = slot_of(&id) {
        set_letter_at(slot, letter);
    }
    log_letter(&id, letter, true);
    Ok(())
}

/// `storage: letter vol=… letter=… custom=…`（盘符变动的证据行）。
fn log_letter(vol_id: &str, letter: u8, custom: bool) {
    serial::print("storage: letter vol=");
    serial::print(vol_id);
    serial::print(" letter=");
    serial::print(crate::storage::letters::ascii(&letter));
    serial::print(" custom=");
    serial::print_dec(custom as u64);
    serial::println("");
}

/// `vol_id` 冲突检测：克隆盘/克隆卷必须**报出来**而不是静默合并（3.4）。
fn detect_conflicts() {
    let mut seen: Vec<String> = Vec::new();
    for i in 0..MAX_VOLUMES {
        let Some(volume) = volume_at(i) else {
            continue;
        };
        let id = String::from(volume.id.as_str());
        if seen.contains(&id) {
            continue;
        }
        // 并列的卷用 `磁盘:分区` 标出（两个分区可能在**同一块盘**上，例如 dd 出来的克隆卷）
        let places: Vec<(u16, u8)> = (0..MAX_VOLUMES)
            .filter_map(|j| volume_at(j))
            .filter(|v| v.id.as_str() == id)
            .map(|v| (v.disk_id, v.part_index))
            .collect();
        if places.len() > 1 {
            for j in 0..MAX_VOLUMES {
                if let Some(v) = slots().get_mut(j).and_then(|s| s.as_mut()) {
                    if v.id.as_str() == id {
                        v.health = Health::Degraded;
                    }
                }
            }
            serial::print("storage: vol conflict id=");
            serial::print(&id);
            serial::print(" at=");
            for (n, (disk, part)) in places.iter().enumerate() {
                if n != 0 {
                    serial::print(",");
                }
                serial::print_dec(*disk as u64);
                serial::print(":");
                serial::print_dec(*part as u64);
            }
            serial::println(" (all marked degraded)");
        }
        seen.push(id);
    }
}

/// 把卷表按 6.5 的确定性顺序写进 `buf`（每条 64 字节），返回条数。
pub fn snapshot(buf: &mut [u8]) -> Result<usize, &'static str> {
    let mut order = [0usize; MAX_VOLUMES];
    let mut count = 0;
    for i in 0..MAX_VOLUMES {
        if volume_at(i).is_some() {
            order[count] = i;
            count += 1;
        }
    }
    // 插入顺序已经按磁盘/分区升序；这里仍然按 (disk_id, part_index, id) 显式排序，
    // 让「确定性」不依赖插入路径的巧合。
    for a in 0..count {
        for b in a + 1..count {
            if before(order[b], order[a])? {
                order.swap(a, b);
            }
        }
    }
    let need = count * VOLUME_RECORD_SIZE;
    if buf.len() < need {
        return Err("storage: volume snapshot buffer too small");
    }
    for (n, &slot) in order[..count].iter().enumerate() {
        let volume = volume_at(slot).ok_or("storage: volume slot vanished")?;
        volume.write_record(&mut buf[n * VOLUME_RECORD_SIZE..(n + 1) * VOLUME_RECORD_SIZE])?;
    }
    Ok(count)
}

/// 排序比较：(disk_id, part_index, id 字节序)。
fn before(a: usize, b: usize) -> Result<bool, &'static str> {
    let (x, y) = (
        volume_at(a).ok_or("storage: volume slot vanished")?,
        volume_at(b).ok_or("storage: volume slot vanished")?,
    );
    let key = |v: &Volume| (v.disk_id, v.part_index);
    if key(x) != key(y) {
        return Ok(key(x) < key(y));
    }
    Ok(x.id.as_str() < y.id.as_str())
}
