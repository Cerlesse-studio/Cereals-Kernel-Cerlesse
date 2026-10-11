//! 分区抽象与探测调度（CSS-1 实现）
//!
//! 一个磁盘的解析结果只有三种，**必须确定性**（`docs/storage.md` 3.3）：
//!
//! 1. 扇区 0 没有 `0x55AA` → `table=none`，整盘当一个分区（现状 `disk.img`/`fat.img` 的形态）；
//! 2. 保护性 MBR（类型 `0xEE`）→ GPT 解析；表头/表项不可用 → `table=invalid`，
//!    **不**回退去解析保护表（那会把保护表当成分区表）；
//! 3. 其它情况 → MBR 解析；空表同样退化为整盘。
//!
//! 「整盘一个分区」用 `part_index = 0` 表示（与卷记录里的约定一致）。

use alloc::vec::Vec;

use crate::driver::block::{BlockDevice, SECTOR_SIZE};
use crate::driver::serial;
use crate::storage::{gpt, mbr};

/// 单盘最多登记的分区数（防御畸形镜像；GPT 表本身有 128 项，这里够用）。
pub const MAX_PARTITIONS_PER_DISK: usize = 16;

/// 分区表类型（记录里的 `table` 字段取值）。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TableKind {
    /// 没有分区表（整盘一个分区）。
    None = 0,
    /// DOS/MBR（含 EBR 逻辑分区）。
    Mbr = 1,
    /// GPT。
    Gpt = 2,
}

impl TableKind {
    /// 证据行里的名字。
    pub fn name(self) -> &'static str {
        match self {
            TableKind::None => "none",
            TableKind::Mbr => "MBR",
            TableKind::Gpt => "GPT",
        }
    }
}

/// 一个分区（GPT/MBR 表项，或「整盘一个分区」的合成项）。
pub struct Partition {
    /// 所属块设备。
    pub disk_id: u16,
    /// 分区序号（`0` = 无表的整盘；MBR 主分区 1..=4、逻辑 5..；GPT 按表项序）。
    pub index: u8,
    /// 来自哪种表。
    pub table: TableKind,
    /// 起始 LBA（512 字节扇区，相对整盘）。
    pub start_lba: u64,
    /// 扇区数。
    pub sectors: u64,
    /// 证据行里的类型短名。
    pub type_name: &'static str,
}

impl Partition {
    /// 串口证据行（`storage: part …`，见 `docs/storage.md` 6.7）。
    pub fn log(&self) {
        serial::print("storage: part disk=");
        serial::print_dec(self.disk_id as u64);
        serial::print(" index=");
        serial::print_dec(self.index as u64);
        serial::print(" table=");
        serial::print(self.table.name());
        serial::print(" start_lba=");
        serial::print_dec(self.start_lba);
        serial::print(" sectors=");
        serial::print_dec(self.sectors);
        serial::print(" type=");
        serial::println(self.type_name);
    }
}

/// 一个磁盘的扫描结果。
pub struct DiskScan {
    /// 证据行的 `table=` 字段（`GPT` / `MBR` / `none` / `invalid`）。
    pub evidence_table: &'static str,
    /// 结构可疑（CRC 失败、EBR 链异常、越界表项……），仍然可用。
    pub degraded: bool,
    /// 分区列表（`table=none|invalid` 时是整盘一个分区）。
    pub partitions: Vec<Partition>,
}

/// 扫描一个块设备的分区表（只读，不修改任何字节）。
pub fn scan(dev: &mut dyn BlockDevice, disk_id: u16) -> Result<DiskScan, &'static str> {
    let sectors = dev.sectors();
    let mut lba0 = [0u8; SECTOR_SIZE as usize];
    if dev.read(0, &mut lba0).map(|n| n == lba0.len()) != Ok(true) {
        return Err("storage: cannot read partition table (lba 0)");
    }

    // 1. 没有引导签名 = 没有分区表（超级软盘布局，合法状态）
    if lba0[510..512] != [0x55, 0xAA] {
        return Ok(whole_disk(disk_id, sectors, "none", false));
    }

    // 2. 保护性 MBR → GPT（CRC 失败用备表头；都失败则整盘一卷 + degraded）
    if mbr::is_protective_mbr(&lba0) {
        let scan = gpt::scan(dev, disk_id)?;
        if scan.invalid {
            return Ok(whole_disk(disk_id, sectors, "invalid", true));
        }
        if scan.partitions.is_empty() {
            return Ok(whole_disk(disk_id, sectors, "none", false));
        }
        return Ok(DiskScan {
            evidence_table: "GPT",
            degraded: scan.degraded,
            partitions: scan.partitions,
        });
    }

    // 3. 其它情况按 MBR 解析；空表同样退化为整盘
    let (partitions, degraded) = mbr::scan(dev, disk_id, &lba0)?;
    if partitions.is_empty() {
        return Ok(whole_disk(disk_id, sectors, "none", degraded));
    }
    Ok(DiskScan {
        evidence_table: "MBR",
        degraded,
        partitions,
    })
}

/// 「整盘一个分区」的合成结果（`part_index = 0`）。
fn whole_disk(disk_id: u16, sectors: u64, evidence: &'static str, degraded: bool) -> DiskScan {
    DiskScan {
        evidence_table: evidence,
        degraded,
        partitions: alloc::vec![Partition {
            disk_id,
            index: 0,
            table: TableKind::None,
            start_lba: 0,
            sectors,
            type_name: "raw",
        }],
    }
}
