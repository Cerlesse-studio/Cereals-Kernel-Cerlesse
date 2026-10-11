//! MBR / EBR 只读解析（CSS-1 实现）
//!
//! 覆盖范围（`docs/storage.md` 3.3）：
//!
//! - 4 个主分区表项（只按 LBA，不解释 CHS）；
//! - 扩展分区链（EBR 跳转，最多 [`EBR_MAX_DEPTH`] 层，带防环检查）；
//! - `0x55AA` 签名校验 —— 没有签名由上层当成「无分区表」（合法状态）。
//!
//! 明确不做：写表项、创建/删除分区、逻辑分区的 CHS 语义。解析失败/可疑一律
//! 通过返回值告诉上层（`degraded`），而不是 panic 或静默截断。

use alloc::vec::Vec;

use crate::driver::block::{BlockDevice, SECTOR_SIZE};
use crate::storage::partition::{Partition, TableKind};

/// 扩展分区链的最大层数（超过即视为可疑，停止并标 degraded）。
pub const EBR_MAX_DEPTH: usize = 8;

/// MBR 表项在引导扇区里的相对偏移（每个 16 字节）。
const ENTRY_OFFSET: usize = 0x1BE;
/// 单个表项的字节数。
const ENTRY_SIZE: usize = 16;

/// 是否有保护性 MBR 表项（类型 `0xEE`）——有则按 GPT 解析，不回退当 MBR。
pub fn is_protective_mbr(lba0: &[u8; SECTOR_SIZE as usize]) -> bool {
    (0..4).any(|i| lba0[ENTRY_OFFSET + i * ENTRY_SIZE + 4] == 0xEE)
}

/// 扫描 MBR：返回（数据分区, degraded）。空表返回空 `Vec`（上层视为「无分区表」）。
pub fn scan(
    dev: &mut dyn BlockDevice,
    disk_id: u16,
    lba0: &[u8; SECTOR_SIZE as usize],
) -> Result<(Vec<Partition>, bool), &'static str> {
    let mut partitions = Vec::new();
    let mut degraded = false;
    let mut next_logical: u8 = 5;

    for i in 0..4 {
        let entry = &lba0[ENTRY_OFFSET + i * ENTRY_SIZE..ENTRY_OFFSET + (i + 1) * ENTRY_SIZE];
        let ptype = entry[4];
        if ptype == 0 {
            continue;
        }
        let start = u32::from_le_bytes([entry[8], entry[9], entry[10], entry[11]]) as u64;
        let sectors = u32::from_le_bytes([entry[12], entry[13], entry[14], entry[15]]) as u64;
        // `start == 0` 是**空表项**而不是坏表项：分区数据不可能从 0 号扇区开始（表就在那儿）。
        // 这条规则的必要性来自实测：`mformat` 写的 ESP（超级软盘）在表里留了一条
        // `start=0 / type=0x06 / sectors=整盘` 的“兼容性”表项，若当坏表项就会让每次
        // 启动都报 `health=degraded`（`docs/storage.md` 3.3 把这种盘归为 `table=none`）。
        if start == 0 {
            continue;
        }
        if sectors == 0 {
            degraded = true;
            continue;
        }
        if matches!(ptype, 0x05 | 0x0F | 0x85) {
            // 扩展分区：本身不是数据分区，把它下面的逻辑分区链拉出来
            degraded |= scan_extended(dev, disk_id, start, &mut partitions, &mut next_logical)?;
            continue;
        }
        if partitions.len() >= crate::storage::partition::MAX_PARTITIONS_PER_DISK {
            degraded = true;
            break;
        }
        partitions.push(Partition {
            disk_id,
            index: (i + 1) as u8,
            table: TableKind::Mbr,
            start_lba: start,
            sectors,
            type_name: mbr_type_name(ptype),
        });
    }

    Ok((partitions, degraded))
}

/// 沿 EBR 链枚举逻辑分区（起始编号 5）。
fn scan_extended(
    dev: &mut dyn BlockDevice,
    disk_id: u16,
    first_ebr: u64,
    partitions: &mut Vec<Partition>,
    next_logical: &mut u8,
) -> Result<bool, &'static str> {
    let mut visited = [0u64; EBR_MAX_DEPTH];
    let mut ebr_lba = first_ebr;
    let mut depth = 0usize;

    loop {
        if depth == EBR_MAX_DEPTH {
            return Ok(true); // 链太长：可疑，停止
        }
        if visited[..depth].contains(&ebr_lba) {
            return Ok(true); // 环：可疑，停止
        }
        visited[depth] = ebr_lba;
        depth += 1;

        let mut buf = [0u8; SECTOR_SIZE as usize];
        if dev.read(ebr_lba, &mut buf).map(|n| n == buf.len()) != Ok(true) {
            return Ok(true); // 读不到下一层：链断了，可疑（不是崩溃）
        }
        if buf[510..512] != [0x55, 0xAA] {
            return Ok(true);
        }

        // 表项 0：本层逻辑分区（起始 LBA 相对于 EBR 所在扇区）
        let entry = &buf[ENTRY_OFFSET..ENTRY_OFFSET + ENTRY_SIZE];
        let ptype = entry[4];
        let rel_start = u32::from_le_bytes([entry[8], entry[9], entry[10], entry[11]]) as u64;
        let sectors = u32::from_le_bytes([entry[12], entry[13], entry[14], entry[15]]) as u64;
        if ptype != 0 && sectors != 0 {
            if partitions.len() >= crate::storage::partition::MAX_PARTITIONS_PER_DISK {
                return Ok(true);
            }
            partitions.push(Partition {
                disk_id,
                index: *next_logical,
                table: TableKind::Mbr,
                start_lba: ebr_lba + rel_start,
                sectors,
                type_name: mbr_type_name(ptype),
            });
            *next_logical = next_logical.saturating_add(1);
        }

        // 表项 1：下一层 EBR 的指针（相对本 EBR；为 0 表示链结束）
        let next = &buf[ENTRY_OFFSET + ENTRY_SIZE..ENTRY_OFFSET + 2 * ENTRY_SIZE];
        let next_type = next[4];
        let next_rel = u32::from_le_bytes([next[8], next[9], next[10], next[11]]) as u64;
        if next_type == 0 || next_rel == 0 {
            return Ok(false);
        }
        ebr_lba = ebr_lba.saturating_add(next_rel);
    }
}

/// MBR 分区类型字节 → 证据行里的短名（够用即可，未知打 `0xNN` 由调用方决定）。
pub fn mbr_type_name(ptype: u8) -> &'static str {
    match ptype {
        0x01 | 0x04 | 0x06 | 0x0B | 0x0C | 0x0E => "fat",
        0x07 => "ntfs/exfat",
        0x82 => "linux-swap",
        0x83 => "linux-fs",
        0x8E => "lvm",
        0xEF => "efi",
        _ => "unknown",
    }
}
