//! GPT 只读解析（CSS-1 实现）
//!
//! 覆盖范围（`docs/storage.md` 3.3）：主表头（LBA 1）、128 个表项、类型 GUID 判定、
//! 主/备表头 CRC32 与表 CRC32 校验。
//!
//! 判定顺序（必须确定性，见 3.3）：
//!
//! 1. 调用方先看到保护性 MBR（类型 `0xEE`）才会进来 —— 因此这里**不**回退去
//!    把保护表当 MBR 解析；
//! 2. 主表头 CRC 不过 → 用**备表头**（磁盘最后一个扇区）继续，整表标 `degraded`
//!    （开放问题 9 的第一版选择：用备份并说清用的是哪张表）；
//! 3. 备表头也读不出/校验不过 → `invalid`（上层退化为「整盘一个卷」）；
//! 4. 表项 CRC 不过 → 仍解析，但整表 `degraded`。
//!
//! CRC32 用标准 IEEE 多项式（与 `mkfs.py` 的 `zlib.crc32` 一致），实现是
//! 逐位版：表项最多 128 KiB，速度在这里不是指标，少一张表才是。

use alloc::vec::Vec;

use crate::driver::block::{BlockDevice, SECTOR_SIZE};
use crate::driver::serial;
use crate::storage::partition::{Partition, TableKind};

/// GPT 表头签名。
const SIGNATURE: &[u8; 8] = b"EFI PART";

/// GPT 一次解析的结果。
pub struct GptScan {
    /// 数据分区。
    pub partitions: Vec<Partition>,
    /// 结构可疑（CRC 失败 / 用了备表头），仍然可用。
    pub degraded: bool,
    /// 主/备表头都不可用（上层退化为整盘一卷）。
    pub invalid: bool,
}

/// 解析 GPT（调用方必须已经确认保护性 MBR）。
pub fn scan(dev: &mut dyn BlockDevice, disk_id: u16) -> Result<GptScan, &'static str> {
    let sectors = dev.sectors();

    // ---- 表头：先主后备 ----
    let mut header = [0u8; SECTOR_SIZE as usize];
    if !read_sector(dev, 1, &mut header) {
        return Ok(GptScan { partitions: Vec::new(), degraded: true, invalid: true });
    }
    let mut degraded = false;
    let parsed = parse_header(&header, sectors);
    let header = match parsed {
        Some(header) if header.current_lba == 1 => header,
        _ => {
            // 主表头不可用：读最后一个扇区的备表头（证据行说清**用的是哪张表**，
            // `docs/storage.md` 开放问题 9 的第一版选择：降级使用并说出来，而不是静默）
            serial::print("storage: gpt primary header unusable on disk=");
            serial::print_dec(disk_id as u64);
            serial::println(" (falling back to the backup header)");
            let mut backup = [0u8; SECTOR_SIZE as usize];
            if !read_sector(dev, sectors.saturating_sub(1), &mut backup) {
                return Ok(GptScan { partitions: Vec::new(), degraded: true, invalid: true });
            }
            match parse_header(&backup, sectors) {
                Some(header) if header.current_lba == sectors.saturating_sub(1) => {
                    degraded = true;
                    header
                }
                _ => {
                    return Ok(GptScan { partitions: Vec::new(), degraded: true, invalid: true });
                }
            }
        }
    };

    // 表项区域：数量/宽度必须在合理范围，且完全落在磁盘内
    if header.entry_size < 128
        || header.entry_size as usize > SECTOR_SIZE as usize
        || header.entry_size % 8 != 0
        || header.count == 0
        || header.count > 256
    {
        return Ok(GptScan { partitions: Vec::new(), degraded: true, invalid: true });
    }
    let entries_len = header.count as u64 * header.entry_size as u64;
    let entries_sectors = entries_len.div_ceil(SECTOR_SIZE as u64);
    if header.entries_lba == 0
        || header.entries_lba.saturating_add(entries_sectors) > sectors
        || entries_len > 128 * 1024
    {
        return Ok(GptScan { partitions: Vec::new(), degraded: true, invalid: true });
    }

    let mut raw = alloc::vec![0u8; (entries_sectors * SECTOR_SIZE as u64) as usize];
    if !read_sector_range(dev, header.entries_lba, &mut raw) {
        return Ok(GptScan { partitions: Vec::new(), degraded: true, invalid: true });
    }
    if crc32(&raw[..entries_len as usize]) != header.entries_crc {
        // 表项 CRC 不过：仍然解析，但整表降级（原因说清楚，不只是一个 `degraded`）
        serial::print("storage: gpt entries CRC mismatch on disk=");
        serial::print_dec(disk_id as u64);
        serial::println(" (parsed anyway; disk marked degraded)");
        degraded = true;
    }

    // ---- 逐表项 ----
    let mut partitions = Vec::new();
    for i in 0..header.count as usize {
        if partitions.len() >= crate::storage::partition::MAX_PARTITIONS_PER_DISK {
            degraded = true;
            break;
        }
        let entry = &raw[i * header.entry_size as usize..(i + 1) * header.entry_size as usize];
        let type_guid: &[u8; 16] = entry[..16].try_into().expect("表项至少 16 字节");
        if type_guid == &[0u8; 16] {
            continue;
        }
        let start = u64::from_le_bytes(entry[0x20..0x28].try_into().expect("表项含起止 LBA"));
        let end = u64::from_le_bytes(entry[0x28..0x30].try_into().expect("表项含起止 LBA"));
        if end < start || end >= sectors {
            degraded = true;
            continue;
        }
        partitions.push(Partition {
            disk_id,
            index: (i + 1) as u8,
            table: TableKind::Gpt,
            start_lba: start,
            sectors: end - start + 1,
            type_name: type_name(type_guid),
        });
    }

    Ok(GptScan { partitions, degraded, invalid: false })
}

/// 解析后的表头字段。
struct Header {
    /// 本表头所在 LBA（主表 = 1，备表 = 最后一扇区）。
    current_lba: u64,
    /// 表项数组起始 LBA。
    entries_lba: u64,
    /// 表项数量。
    count: u32,
    /// 单个表项字节数。
    entry_size: u32,
    /// 表项数组的 CRC32。
    entries_crc: u32,
}

/// 校验并解析一个 GPT 表头（含自校验的 CRC32）。
fn parse_header(raw: &[u8; SECTOR_SIZE as usize], sectors: u64) -> Option<Header> {
    if &raw[0..8] != SIGNATURE {
        return None;
    }
    let header_size = u32::from_le_bytes([raw[12], raw[13], raw[14], raw[15]]);
    if !(92..=SECTOR_SIZE).contains(&header_size) {
        return None;
    }
    let stored_crc = u32::from_le_bytes([raw[16], raw[17], raw[18], raw[19]]);
    // CRC 计算时把 CRC 字段本身置 0（规范要求）
    let mut tmp = [0u8; SECTOR_SIZE as usize];
    tmp.copy_from_slice(raw);
    tmp[16..20].copy_from_slice(&[0, 0, 0, 0]);
    if crc32(&tmp[..header_size as usize]) != stored_crc {
        return None;
    }
    let current_lba = u64::from_le_bytes(raw[24..32].try_into().ok()?);
    let entries_lba = u64::from_le_bytes(raw[72..80].try_into().ok()?);
    let count = u32::from_le_bytes(raw[80..84].try_into().ok()?);
    let entry_size = u32::from_le_bytes(raw[84..88].try_into().ok()?);
    let entries_crc = u32::from_le_bytes(raw[88..92].try_into().ok()?);
    if current_lba >= sectors {
        return None;
    }
    Some(Header { current_lba, entries_lba, count, entry_size, entries_crc })
}

/// GPT 类型 GUID → 证据行短名。
pub fn type_name(guid: &[u8; 16]) -> &'static str {
    if guid == ESP_GUID {
        return "efi";
    }
    if guid == LINUX_FS_GUID {
        return "linux-fs";
    }
    if guid == MS_DATA_GUID {
        return "ms-data";
    }
    if guid == BIOS_BOOT_GUID {
        return "bios-boot";
    }
    "unknown"
}

/// EFI 系统分区（ESP）：`C12A7328-F81F-11D2-BA4B-00A0C93EC93B`（磁盘字节序）。
///
/// `pub(crate)`：`storage::selftest` 的合成镜像要写同样的类型 GUID，
/// 这样「已知 GUID 表」只有一份定义（改一处，解析与自测一起变）。
pub(crate) const ESP_GUID: &[u8; 16] = &[
    0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B,
];
/// Linux 文件系统数据分区：`0FC63DAF-8483-4772-8E79-3D69D8477DE4`。
pub(crate) const LINUX_FS_GUID: &[u8; 16] = &[
    0xAF, 0x3D, 0xC6, 0x0F, 0x83, 0x84, 0x72, 0x47, 0x8E, 0x79, 0x3D, 0x69, 0xD8, 0x47, 0x7D, 0xE4,
];
/// Microsoft 基本数据分区：`EBD0A0A2-B9E5-4433-87C0-68B6B72699C7`。
const MS_DATA_GUID: &[u8; 16] = &[
    0xA2, 0xA0, 0xD0, 0xEB, 0xE5, 0xB9, 0x33, 0x44, 0x87, 0xC0, 0x68, 0xB6, 0xB7, 0x26, 0x99, 0xC7,
];
/// BIOS boot 分区：`21686148-6449-6E6F-744E-656564454649`。
const BIOS_BOOT_GUID: &[u8; 16] = &[
    0x48, 0x61, 0x68, 0x21, 0x49, 0x64, 0x6F, 0x6E, 0x74, 0x4E, 0x65, 0x65, 0x64, 0x45, 0x46, 0x49,
];

/// 标准 IEEE CRC32（与 `zlib.crc32`、GPT 规范一致）。
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn read_sector(dev: &mut dyn BlockDevice, lba: u64, buf: &mut [u8; SECTOR_SIZE as usize]) -> bool {
    dev.read(lba, buf).map(|n| n == buf.len()) == Ok(true)
}

fn read_sector_range(dev: &mut dyn BlockDevice, lba: u64, buf: &mut [u8]) -> bool {
    dev.read(lba, buf).map(|n| n == buf.len()) == Ok(true)
}
