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
    for index in 0..block::count() {
        log_disk_volumes(index);
    }
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
    log_disk_volumes(index);
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
