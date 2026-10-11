//! CSS-1 解析路径自测 —— **内存盘**上的合成镜像（不碰真实磁盘、不依赖固件）。
//!
//! ## 为什么不靠镜像夹具来验这些路径
//!
//! 实测（本仓库开发环境，QEMU 6.2 + OVMF）：**固件会自愈 GPT**。给一块
//! 「主表头 CRC 坏、备表头完好」的盘，OVMF 在设备扫描阶段就把备份表头写回主位置
//! ——引导到内核时它已经是好的了（把同一镜像的 `snapshot=on` 去掉再启动，文件本身
//! 会被改写，CRC 变回正确值）。因此内核里「主表头不可用 → 用备表头」这条路径
//! **永远轮不到**固件跑过之后的镜像；`docs/storage.md` 6.7 的兼容性说明也记了这件事。
//!
//! 结论：这类**解析器**行为用内存盘自测（确定性、可断言每一处降级原因），
//! 镜像夹具（`tools/mkfs/mkfs.py` + `tests/test_storage.py`）负责**集成**路径：
//! 真实 virtio-blk → 分区表 → 文件系统探测 → 卷表 → 用户态记录。
//!
//! 合成镜像由本文件构造，是 `mkfs.py::render_gpt` 之外的**第二份独立实现**
//! （宿主侧的第三种在 `tests/test_storage.py`，三方互查）。

use alloc::vec;
use alloc::vec::Vec;

use crate::driver::block::{BlockDevice, SECTOR_SIZE};
use crate::driver::serial;
use crate::storage::gpt::{self, ESP_GUID, LINUX_FS_GUID};
use crate::storage::mbr;
use crate::storage::partition;

/// 合成盘的扇区数（64 KiB）。
///
/// 刻意取小：内核堆只有 1 MiB（`memory::heap::HEAP_SIZE`），而自测每个用例都要
/// 造一块盘 —— 盘大一点点就会把堆吃掉，反而掩盖掉真正要测的解析逻辑。
const SECTORS: usize = 128;
/// GPT 表项扇区数（128 项 × 128 字节）。
const ENTRY_SECTORS: u64 = 32;
/// 备表头/备表项保留扇区（与规范一致：最后 33 个扇区）。
const BACKUP_SECTORS: u64 = ENTRY_SECTORS + 1;

/// 内存盘：一个扇区对齐的字节缓冲就是一个块设备（自测只读路径）。
struct MemDisk {
    name: &'static str,
    data: Vec<u8>,
}

impl MemDisk {
    /// 全零的盘。
    fn blank(name: &'static str) -> Self {
        Self { name, data: vec![0u8; SECTORS * SECTOR_SIZE as usize] }
    }

    /// 第 `lba` 个扇区的可变切片。
    fn sector_mut(&mut self, lba: u64) -> &mut [u8] {
        let start = lba as usize * SECTOR_SIZE as usize;
        &mut self.data[start..start + SECTOR_SIZE as usize]
    }

}

impl BlockDevice for MemDisk {
    fn name(&self) -> &'static str {
        self.name
    }

    fn sector_size(&self) -> u32 {
        SECTOR_SIZE
    }

    fn sectors(&self) -> u64 {
        (self.data.len() / SECTOR_SIZE as usize) as u64
    }

    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<usize, &'static str> {
        if buf.len() % SECTOR_SIZE as usize != 0 {
            return Err("memdisk: read length not sector aligned");
        }
        let start = (lba as usize)
            .checked_mul(SECTOR_SIZE as usize)
            .ok_or("memdisk: lba overflow")?;
        let end = start.checked_add(buf.len()).ok_or("memdisk: lba overflow")?;
        let src = self.data.get(start..end).ok_or("memdisk: read beyond capacity")?;
        buf.copy_from_slice(src);
        Ok(buf.len())
    }

    fn write(&mut self, lba: u64, data: &[u8]) -> Result<usize, &'static str> {
        if data.len() % SECTOR_SIZE as usize != 0 {
            return Err("memdisk: write length not sector aligned");
        }
        let start = (lba as usize)
            .checked_mul(SECTOR_SIZE as usize)
            .ok_or("memdisk: lba overflow")?;
        let end = start.checked_add(data.len()).ok_or("memdisk: lba overflow")?;
        let dst = self.data.get_mut(start..end).ok_or("memdisk: write beyond capacity")?;
        dst.copy_from_slice(data);
        Ok(data.len())
    }
}

// ---- 合成镜像的小工具 ----

fn le32(buf: &mut [u8], off: usize, value: u32) {
    buf[off..off + 4].copy_from_slice(&value.to_le_bytes());
}

fn le64(buf: &mut [u8], off: usize, value: u64) {
    buf[off..off + 8].copy_from_slice(&value.to_le_bytes());
}

/// 一个数据分区的描述（类型 GUID + 起止 LBA）。
struct Entry {
    guid: &'static [u8; 16],
    start: u64,
    end: u64,
}

/// 故意的损坏（模拟镜像里的各种坏法）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Damage {
    None,
    /// 主表头自校验 CRC 不对（备表头仍然完好）。
    PrimaryHeaderCrc,
    /// 表项被改坏（表头里存的是**完好**表项的 CRC）。
    EntriesCrc,
    /// 主/备表头都坏。
    BothHeaders,
}

/// MBR 表项（16 字节）。
fn mbr_entry(buf: &mut [u8], index: usize, ptype: u8, start: u64, sectors: u64) {
    let off = 0x1BE + index * 16;
    buf[off + 4] = ptype;
    le32(buf, off + 8, start as u32);
    le32(buf, off + 12, sectors as u32);
}

/// 写引导签名。
fn sign(buf: &mut [u8]) {
    buf[510] = 0x55;
    buf[511] = 0xAA;
}

/// 合成一张 GPT 盘。
fn gpt_disk(entries: &[Entry], damage: Damage) -> MemDisk {
    let mut disk = MemDisk::blank("memdisk-gpt");
    let disk_sectors = disk.sectors();

    // 保护性 MBR
    {
        let boot = disk.sector_mut(0);
        mbr_entry(boot, 0, 0xEE, 1, disk_sectors - 1);
        sign(boot);
    }

    // 表项数组（主/备各写一份，与规范一致）
    let mut entries_raw = vec![0u8; (ENTRY_SECTORS * SECTOR_SIZE as u64) as usize];
    for (index, entry) in entries.iter().enumerate() {
        let off = index * 128;
        entries_raw[off..off + 16].copy_from_slice(entry.guid);
        le64(&mut entries_raw, off + 0x20, entry.start);
        le64(&mut entries_raw, off + 0x28, entry.end);
    }
    // 关键顺序（与 `mkfs.py` 一致）：**先**按完好表项算 CRC 写进表头，**再**改坏表项
    // —— 这样「表头声称的 CRC 对不上表项」才是真的对不上；反过来先改坏再算 CRC
    // 会自己骗自己（两边一致就不会降级）。
    let entries_crc = gpt::crc32(&entries_raw);
    if damage == Damage::EntriesCrc {
        // 只动**未使用**表项的名字字节：表项 CRC 会变，分区清单不变
        entry_unused_byte(&mut entries_raw);
    }
    let backup_entries_lba = disk_sectors - BACKUP_SECTORS;
    for lba in [2u64, backup_entries_lba] {
        let start = lba as usize * SECTOR_SIZE as usize;
        disk.data[start..start + entries_raw.len()].copy_from_slice(&entries_raw);
    }

    // 表头（主在 1，备在最后一个扇区）
    let mut primary = vec![0u8; SECTOR_SIZE as usize];
    let mut backup = vec![0u8; SECTOR_SIZE as usize];
    write_header(
        &mut primary,
        disk_sectors,
        1,
        disk_sectors - 1,
        2,
        entries_crc,
        damage == Damage::PrimaryHeaderCrc || damage == Damage::BothHeaders,
    );
    write_header(
        &mut backup,
        disk_sectors,
        disk_sectors - 1,
        1,
        backup_entries_lba,
        entries_crc,
        damage == Damage::BothHeaders,
    );
    disk.sector_mut(1).copy_from_slice(&primary);
    disk.sector_mut(disk_sectors - 1).copy_from_slice(&backup);
    disk
}

/// 故意改坏一个**未使用**表项的字节（第 128 项的名字区）。
fn entry_unused_byte(entries: &mut [u8]) {
    let off = 127 * 128 + 0x38;
    entries[off] ^= 0xFF;
}

/// 写一个 GPT 表头（`damaged` 时把自校验 CRC 写错）。
fn write_header(buf: &mut [u8], disk_sectors: u64, current: u64, backup: u64,
                entries_lba: u64, entries_crc: u32, damaged: bool) {
    buf[0..8].copy_from_slice(b"EFI PART");
    le32(buf, 8, 0x0001_0000);
    le32(buf, 12, 92);
    le64(buf, 24, current);
    le64(buf, 32, backup);
    le64(buf, 40, 34);
    le64(buf, 48, disk_sectors - BACKUP_SECTORS - 1);
    le64(buf, 72, entries_lba);
    le32(buf, 80, 128);
    le32(buf, 84, 128);
    le32(buf, 88, entries_crc);
    let mut crc = gpt::crc32(&buf[..92]);
    if damaged {
        crc ^= 0xFF;
    }
    le32(buf, 16, crc);
}

/// 一张 MBR 盘：`primaries` 是 4 个主表项（`None` = 空），`logicals` 走扩展链。
struct MbrLayout {
    primaries: [(u8, u64, u64); 4],
    extended: Option<(u64, u64)>,
    /// （EBR 所在 LBA, 逻辑分区类型, 起始 LBA 相对 EBR 的偏移, 扇区数）
    logicals: Vec<(u64, u8, u32, u32)>,
    /// 最后一层 EBR 的「下一层」指针（`None` = 正常结束）
    last_next: Option<u32>,
}

impl MbrLayout {
    fn empty() -> Self {
        Self { primaries: [(0, 0, 0); 4], extended: None, logicals: Vec::new(), last_next: None }
    }
}

fn mbr_disk(layout: &MbrLayout) -> MemDisk {
    let mut disk = MemDisk::blank("memdisk-mbr");
    {
        let boot = disk.sector_mut(0);
        for (index, &(ptype, start, sectors)) in layout.primaries.iter().enumerate() {
            if ptype != 0 {
                mbr_entry(boot, index, ptype, start, sectors);
            }
        }
        if let Some((start, sectors)) = layout.extended {
            mbr_entry(boot, 3, 0x0F, start, sectors);
        }
        sign(boot);
    }
    for (index, &(ebr_lba, ptype, rel_start, sectors)) in layout.logicals.iter().enumerate() {
        let ebr = disk.sector_mut(ebr_lba);
        mbr_entry(ebr, 0, ptype, rel_start as u64, sectors as u64);
        if index + 1 == layout.logicals.len() {
            if let Some(next) = layout.last_next {
                mbr_entry(ebr, 1, 0x0F, next as u64, sectors as u64);
            }
        } else {
            let next = layout.logicals[index + 1].0 - ebr_lba;
            mbr_entry(ebr, 1, 0x0F, next, sectors as u64);
        }
        sign(ebr);
    }
    disk
}

/// 断言助手：失败时返回带用例名的错误（halt 时能直接看出是哪条路径坏了）。
macro_rules! check {
    ($case:expr, $cond:expr, $what:expr) => {
        if !$cond {
            return Err(concat!($case, ": ", $what));
        }
    };
}

/// 跑全部解析用例，返回检查项数量。
pub fn run() -> Result<usize, &'static str> {
    serial::println("storage: parse selftest begin");
    let mut checks = 0usize;

    // ---- 1. 完好的 GPT：2 个分区，类型名与起止都对 ----
    {
        let entries = [
            Entry { guid: ESP_GUID, start: 40, end: 47 },
            Entry { guid: LINUX_FS_GUID, start: 48, end: 63 },
        ];
        let mut disk = gpt_disk(&entries, Damage::None);
        let scan = partition::scan(&mut disk, 0)?;
        check!("gpt_ok", scan.evidence_table == "GPT", "table is not GPT");
        check!("gpt_ok", !scan.degraded, "healthy GPT marked degraded");
        check!("gpt_ok", scan.partitions.len() == 2, "wrong partition count");
        check!("gpt_ok", scan.partitions[0].index == 1, "first entry index");
        check!("gpt_ok", scan.partitions[0].start_lba == 40, "first entry start");
        check!("gpt_ok", scan.partitions[0].sectors == 8, "first entry size");
        check!("gpt_ok", scan.partitions[0].type_name == "efi", "ESP type name");
        check!("gpt_ok", scan.partitions[1].type_name == "linux-fs", "linux type name");
        checks += 8;
    }

    // ---- 2. 主表头坏、备表头好：必须降级仍可用（固件自愈之后轮不到的路径）----
    {
        let entries = [
            Entry { guid: ESP_GUID, start: 40, end: 47 },
            Entry { guid: LINUX_FS_GUID, start: 48, end: 63 },
        ];
        let mut disk = gpt_disk(&entries, Damage::PrimaryHeaderCrc);
        let scan = partition::scan(&mut disk, 0)?;
        check!("gpt_primary_bad", scan.evidence_table == "GPT", "table is not GPT");
        check!("gpt_primary_bad", scan.degraded, "backup header use must mark degraded");
        check!("gpt_primary_bad", scan.partitions.len() == 2, "backup entries not parsed");
        check!("gpt_primary_bad", scan.partitions[1].start_lba == 48, "backup entry start");
        checks += 4;
    }

    // ---- 3. 表项 CRC 不过：仍然解析，但整盘降级 ----
    {
        let entries = [
            Entry { guid: ESP_GUID, start: 40, end: 47 },
            Entry { guid: LINUX_FS_GUID, start: 48, end: 63 },
        ];
        let mut disk = gpt_disk(&entries, Damage::EntriesCrc);
        let scan = partition::scan(&mut disk, 0)?;
        check!("gpt_entries_bad", scan.evidence_table == "GPT", "table is not GPT");
        check!("gpt_entries_bad", scan.degraded, "entries CRC failure must degrade");
        check!("gpt_entries_bad", scan.partitions.len() == 2, "entries parsed anyway");
        checks += 3;
    }

    // ---- 4. 主/备表头都坏：判 invalid（整盘一个分区），且**不回退**解析保护表 ----
    {
        let entries = [Entry { guid: ESP_GUID, start: 40, end: 47 }];
        let mut disk = gpt_disk(&entries, Damage::BothHeaders);
        let scan = partition::scan(&mut disk, 0)?;
        check!("gpt_both_bad", scan.evidence_table == "invalid", "must be invalid");
        check!("gpt_both_bad", scan.degraded, "invalid table must be degraded");
        check!("gpt_both_bad", scan.partitions.len() == 1, "must fall back to the whole disk");
        check!("gpt_both_bad", scan.partitions[0].index == 0, "whole-disk 分区号必须是 0");
        check!(
            "gpt_both_bad",
            scan.partitions[0].start_lba == 0 && scan.partitions[0].sectors == disk.sectors(),
            "whole-disk 必须覆盖整盘"
        );
        checks += 5;
    }

    // ---- 5. 表项越界：跳过该表项并降级（不是 panic，也不是静默当有效）----
    {
        let entries = [
            Entry { guid: ESP_GUID, start: 40, end: 47 },
            Entry { guid: LINUX_FS_GUID, start: 60, end: u64::MAX / 2 },
        ];
        let mut disk = gpt_disk(&entries, Damage::None);
        let scan = partition::scan(&mut disk, 0)?;
        check!("gpt_entry_range", scan.evidence_table == "GPT", "table is not GPT");
        check!("gpt_entry_range", scan.degraded, "out-of-range entry must degrade");
        check!("gpt_entry_range", scan.partitions.len() == 1, "bad entry must be skipped");
        checks += 3;
    }

    // ---- 6. MBR 主分区 + 扩展链：逻辑分区号从 5 起 ----
    {
        let mut layout = MbrLayout::empty();
        layout.primaries[0] = (0x0C, 1, 8);
        layout.extended = Some((16, 32));
        layout.logicals = vec![(16, 0x07, 1, 4), (24, 0x83, 1, 4)];
        let mut disk = mbr_disk(&layout);
        let scan = partition::scan(&mut disk, 0)?;
        check!("mbr_chain", scan.evidence_table == "MBR", "table is not MBR");
        check!("mbr_chain", !scan.degraded, "clean chain marked degraded");
        check!("mbr_chain", scan.partitions.len() == 3, "wrong partition count");
        check!("mbr_chain", scan.partitions[0].index == 1, "primary index");
        check!("mbr_chain", scan.partitions[1].index == 5, "first logical index");
        check!("mbr_chain", scan.partitions[2].index == 6, "second logical index");
        check!("mbr_chain", scan.partitions[1].start_lba == 17, "逻辑分区的绝对起始 LBA");
        check!("mbr_chain", scan.partitions[2].start_lba == 25, "第二个逻辑分区起始");
        check!("mbr_chain", scan.partitions[0].type_name == "fat", "0x0C 类型名");
        checks += 9;
    }

    // ---- 7. 扩展链断掉（下一层指针指到盘外）：停下来并降级，不越界读 ----
    {
        let mut layout = MbrLayout::empty();
        layout.extended = Some((16, 32));
        layout.logicals = vec![(16, 0x07, 1, 4), (20, 0x07, 1, 4)];
        layout.last_next = Some(SECTORS as u32 + 512);
        let mut disk = mbr_disk(&layout);
        let scan = partition::scan(&mut disk, 0)?;
        check!("mbr_broken", scan.evidence_table == "MBR", "table is not MBR");
        check!("mbr_broken", scan.degraded, "broken chain must degrade");
        check!("mbr_broken", scan.partitions.len() == 2, "chain must stop after the last good EBR");
        checks += 3;
    }

    // ---- 8. 扩展链过长：按 EBR 深度上限截断（防 DoS，且必须有界）----
    {
        let mut layout = MbrLayout::empty();
        layout.extended = Some((16, 96));
        let depth = mbr::EBR_MAX_DEPTH + 4;
        layout.logicals =
            (0..depth).map(|i| (16 + i as u64 * 4, 0x07, 1, 4)).collect();
        let mut disk = mbr_disk(&layout);
        let scan = partition::scan(&mut disk, 0)?;
        check!("mbr_deep", scan.degraded, "over-deep chain must degrade");
        check!(
            "mbr_deep",
            scan.partitions.len() == mbr::EBR_MAX_DEPTH,
            "chain must be cut at EBR_MAX_DEPTH"
        );
        checks += 2;
    }

    // ---- 9. 有签名但表全空（合法状态）：整盘一个分区，**不**降级 ----
    {
        let mut disk = MemDisk::blank("memdisk-empty");
        sign(disk.sector_mut(0));
        let scan = partition::scan(&mut disk, 0)?;
        check!("mbr_empty", scan.evidence_table == "none", "empty table must be `none`");
        check!("mbr_empty", !scan.degraded, "empty table is not damage");
        check!("mbr_empty", scan.partitions.len() == 1 && scan.partitions[0].index == 0, "整盘一卷");
        checks += 3;
    }

    // ---- 10. `start == 0` 的表项（`mformat` 写的 ESP 就是这样）：当空表项 ----
    {
        let mut disk = MemDisk::blank("memdisk-mformat");
        let capacity = disk.sectors();
        {
            let boot = disk.sector_mut(0);
            mbr_entry(boot, 0, 0x06, 0, capacity);
            sign(boot);
        }
        let scan = partition::scan(&mut disk, 0)?;
        check!("mbr_start_zero", scan.evidence_table == "none", "start=0 表项必须当空表");
        check!("mbr_start_zero", !scan.degraded, "start=0 不是损坏（否则每次启动都 degraded）");
        check!("mbr_start_zero", scan.partitions.len() == 1, "整盘一卷");
        checks += 3;
    }

    // ---- 11. 没有引导签名：整盘一个分区 ----
    {
        let mut disk = MemDisk::blank("memdisk-raw");
        let scan = partition::scan(&mut disk, 0)?;
        check!("mbr_no_signature", scan.evidence_table == "none", "no signature must be `none`");
        check!("mbr_no_signature", !scan.degraded, "no signature is not damage");
        checks += 2;
    }

    serial::print("storage: parse selftest PASS (cases=11 checks=");
    serial::print_dec(checks as u64);
    serial::println(")");
    Ok(checks)
}
