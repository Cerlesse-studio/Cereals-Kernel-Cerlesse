//! Cerlesse Storage System —— CSS-1「底层识别」（以 `docs/storage.md` 为准）
//!
//! 这一层只做**只读识别**：块设备 →（GPT/MBR）分区 → 文件系统探测 → 卷表。
//! 它不挂载、不写盘、不认识盘符——挂载仍是 `fs` 的事（CSS-2 才把挂载表接到卷身份上）。
//!
//! 模块划分（与文档 5.3 一致）：
//!
//! | 文件 | 职责 |
//! | ---- | ---- |
//! | [`partition`] | 分区抽象 + GPT/MBR 调度 + 「无表 = 整盘一卷」的退化 |
//! | [`gpt`] | GPT 只读解析（表头/表项 CRC32、备表头回退、类型 GUID） |
//! | [`mbr`] | MBR/EBR 只读解析（4 主分区 + 扩展链，防环） |
//! | [`volume`] | 卷对象、`vol_id`、文件系统探测、64 字节记录布局 |
//! | [`registry`] | 卷表、确定性枚举、冲突检测、`storage_volumes` 快照 |
//!
//! 启动顺序：`fs::mount_disks` **之前**调用 [`init`]（先看清有哪些卷，再谈挂载）。

use crate::driver::serial;

pub mod gpt;
pub mod mbr;
pub mod partition;
pub mod registry;
pub mod selftest;
pub mod volume;

/// 卷表容量（固定槽位；满了明确拒绝，见 `registry`）。
pub const MAX_VOLUMES: usize = 16;
/// 每个卷最多挂载点数（CSS-2 使用；CSS-1 只把字段写进记录）。
pub const MAX_MOUNTS_PER_VOL: usize = 2;

/// 标准 IEEE CRC32（GPT 校验与 `vol_id` 退化共用）。
pub use self::gpt::crc32;

/// 扫描所有块设备、建立卷表并打印证据行（幂等；重复调用会重建）。
pub fn init() -> Result<(), &'static str> {
    serial::print("storage: init MAX_VOLUMES=");
    serial::print_dec(MAX_VOLUMES as u64);
    serial::print(" MAX_MOUNTS_PER_VOL=");
    serial::print_dec(MAX_MOUNTS_PER_VOL as u64);
    serial::print(" max_devices=");
    serial::print_dec(crate::driver::block::MAX_BLOCK_DEVICES as u64);
    serial::println("");
    registry::scan_all(true)?;
    Ok(())
}

/// CSS-1 自测：分两部分 ——
///
/// 1. [`selftest::run`]：**解析路径**用内存盘上的合成镜像验证（GPT 主表头坏→用备表头、
///    表项 CRC 坏、两张表头都坏、MBR 扩展链/断链/超深链、`start=0` 表项……）。
///    为什么不用镜像：实测固件（OVMF）会自愈坏掉的主表头，引导到内核时已经修好了，
///    这些分支永远轮不到（见 `selftest` 模块头）。
/// 2. 卷表的**记录性质**：两次枚举逐字节相同（确定性）、布局与取值域、保留位为零。
///
/// 具体某张盘的卷由 `make test-storage` 的镜像与断言给出。
pub fn selftest() -> Result<(), &'static str> {
    let parsed = selftest::run()?;

    // 1. 两次快照逐字节相同（6.5 的确定性是可断言的前提）
    let mut first = [0u8; MAX_VOLUMES * volume::VOLUME_RECORD_SIZE];
    let mut second = [0u8; MAX_VOLUMES * volume::VOLUME_RECORD_SIZE];
    let n = registry::snapshot(&mut first)?;
    if registry::snapshot(&mut second)? != n || first[..n * 64] != second[..n * 64] {
        return Err("storage: volume enumeration is not deterministic");
    }
    let mut checks = 1u64;

    // 2. 每条记录的字段都在取值域内（布局断言，与 libc 解析器的镜像定义对齐）
    for i in 0..n {
        let rec = &first[i * volume::VOLUME_RECORD_SIZE..(i + 1) * volume::VOLUME_RECORD_SIZE];
        let id_len = rec[..16].iter().position(|&b| b == 0).unwrap_or(16);
        if id_len == 0 || id_len > volume::VOL_ID_MAX {
            return Err("storage: vol id length out of range");
        }
        let charset_ok = rec[..id_len].iter().all(|&b| {
            b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-' || b == b':'
        });
        if !charset_ok {
            return Err("storage: vol id contains characters outside [A-Za-z0-9._:-]");
        }
        if rec[19] > 8 {
            return Err("storage: fs_kind out of range");
        }
        if rec[20] > 4 {
            return Err("storage: read_only reason out of range");
        }
        if rec[21] > 3 {
            return Err("storage: health out of range");
        }
        let flags = u16::from_le_bytes([rec[22], rec[23]]);
        if flags & !0x000F != 0 {
            return Err("storage: unknown volume record flags set");
        }
        if rec[60..64] != [0, 0, 0, 0] {
            return Err("storage: reserved bytes in volume record must be zero");
        }
        checks += 6;
    }

    serial::print("storage: selftest PASS (checks=");
    serial::print_dec(checks + parsed as u64);
    serial::println(")");
    Ok(())
}
