//! 块设备抽象与注册表（v0.8 实现）
//!
//! 上层（FAT32 挂载、`syscall` 的文件语义）只认 [`BlockDevice`] 这个 trait，
//! 不认 virtio / AHCI / ATA —— 与 `fs::vfs::FileSystem` 同样的收口思路：
//! **后端可换，接口不变**。v0.8 只有一个后端（[`crate::driver::virtio_blk`]）。
//!
//! 粒度约定：
//!
//! - 扇区大小固定 512 字节（[`SECTOR_SIZE`]）—— MBR 分区表与 FAT 引导扇区的
//!   经典粒度；`read`/`write` 要求调用方给的缓冲长度是扇区大小的**整数倍**，
//!   不满足直接报错而不是静默取整（少一次「数据少写了一半」的调试）；
//! - `lba` 是**绝对逻辑块地址**（不分区的整盘寻址，v0.8 没有分区表解析）；
//! - 调用方负责缓冲对齐：驱动内部用一块页对齐的 bounce buffer 做整扇区搬运，
//!   因此这里不要求调用方缓冲对齐（多一次拷贝，换掉一整类对齐 bug）。
//!
//! 注册表是固定容量数组（[`MAX_BLOCK_DEVICES`]）而不是 `Vec`：它保存
//! `&'static mut dyn BlockDevice`，写入一次、读多次，容量固定能让借用关系一眼看清。

use crate::driver::serial;

/// 扇区大小（字节）：MBR / FAT 引导扇区的经典粒度。
pub const SECTOR_SIZE: u32 = 512;/// 块设备数量上限（v0.8 只有整块启动盘；留一个位置给 v0.9 的第二块盘）。
pub const MAX_BLOCK_DEVICES: usize = 2;

/// 块设备：按扇区读写。
pub trait BlockDevice {
    /// 设备名（日志与错误信息里用，例如 `virtio-blk0`）。
    fn name(&self) -> &'static str;

    /// 扇区大小（v0.8 恒为 512）。
    fn sector_size(&self) -> u32;

    /// 总扇区数（容量）。
    fn sectors(&self) -> u64;

    /// 从 `lba` 起读 `buf.len()` 字节（长度必须是扇区大小的整数倍）。
    ///
    /// 返回读到的字节数（v0.8 成功即等于 `buf.len()`）。
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<usize, &'static str>;

    /// 从 `lba` 起写 `data`（长度必须是扇区大小的整数倍）。
    fn write(&mut self, lba: u64, data: &[u8]) -> Result<usize, &'static str>;

    /// 容量（字节）。
    fn capacity_bytes(&self) -> u64 {
        self.sectors() * self.sector_size() as u64
    }
}

/// 长度是否为整扇区（`read`/`write` 的前置条件）。
///
/// 是自由函数而不是 trait 的关联函数：关联函数没有 `self` 会让 trait 不再
/// dyn compatible（`&dyn BlockDevice` 才是上层依赖的形态）。
pub fn is_sector_aligned(len: usize) -> bool {
    len % SECTOR_SIZE as usize == 0
}

// 注册表：`register` 只写一次（驱动 probe 期间），之后只读。
static mut DEVICES: [Option<&'static mut dyn BlockDevice>; MAX_BLOCK_DEVICES] = [None, None];

/// 注册一个块设备，返回它的下标（供 `block::get` 取回）。
///
/// 失败：设备数超过 [`MAX_BLOCK_DEVICES`]。
pub fn register(dev: &'static mut dyn BlockDevice) -> Result<usize, &'static str> {
    // SAFETY: 注册表是静态量；驱动 probe 期间单线程写入，此后只读。
    unsafe {
        let table = (&raw mut DEVICES).as_mut().expect("DEVICES 是静态量");
        for (i, slot) in table.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(dev);
                return Ok(i);
            }
        }
    }
    Err("block device registry full")
}

/// 已注册的块设备数量。
pub fn count() -> usize {
    // SAFETY: 只读访问注册表。
    unsafe {
        (&raw const DEVICES)
            .as_ref()
            .map(|table| table.iter().filter(|slot| slot.is_some()).count())
            .unwrap_or(0)
    }
}

/// 取第 `index` 个块设备。
pub fn get(index: usize) -> Option<&'static mut dyn BlockDevice> {
    // SAFETY: 注册表在 probe 之后不再变化；调用期间由 `&mut` 保证独占。
    unsafe {
        (&raw mut DEVICES)
            .as_mut()?
            .get_mut(index)?
            .as_mut()
            .map(|dev| &mut **dev)
    }
}

/// 第一个块设备（v0.8 只有一块盘）。
pub fn first() -> Option<&'static mut dyn BlockDevice> {
    get(0)
}

// ---- 验收自测（v0.8） ----

/// 块设备验收自测。
///
/// 断言的是**磁盘内容与语义**，不是日志措辞：
///
/// 1. 引导扇区来自本次构建的 FAT 镜像（`make image` 写进去的）—— 读到它说明驱动
///    真的从设备取回了数据，而不是返回缓冲区里的旧字节；`0xAA55` 是引导签名，
///    类型串由 BPB 的根目录项数决定（非 0 → FAT12/16 的 offset 54，0 → FAT32 的 82）；
/// 2. 4608 字节的读跨越了驱动内部的搬运边界（bounce = 8 扇区 = 4096 字节），
///    与 4096 / 512 两次独立读的拼接结果逐字节比对，验证分块重组与偏移换算；
/// 3. 最后一个扇区做「写 → 读回 → 还原 → 再读回」：写路径与读路径都走真实设备；
/// 4. 负例（长度非整扇区 / 越过容量 / lba 溢出）必须报错，而不是静默取整或越界访问。
pub fn selftest(dev: &mut dyn BlockDevice) -> Result<(), &'static str> {
    let sectors = dev.sectors();
    if sectors < 16 {
        return Err("block: device too small for selftest");
    }

    // 1. 设备概况（容量是设备自己报的，不是常量）
    serial::print("block: device=");
    serial::print(dev.name());
    serial::print(" sector_size=");
    serial::print_dec(dev.sector_size() as u64);
    serial::print(" sectors=");
    serial::print_dec(sectors);
    serial::print(" bytes=");
    serial::print_dec(dev.capacity_bytes());
    serial::println("");

    // 2. 引导扇区
    let mut boot = [0u8; SECTOR_SIZE as usize];
    read_all(dev, 0, &mut boot)?;
    let signature = u16::from_le_bytes([boot[510], boot[511]]);
    if signature != 0xAA55 {
        return Err("block: boot sector signature is not 0xAA55");
    }
    let root_entries = u16::from_le_bytes([boot[17], boot[18]]);
    let fs_type = if root_entries == 0 { 82 } else { 54 };
    serial::print("block: boot sector sig=0x55aa fstype=");
    print_ascii(&boot[fs_type..fs_type + 8]);
    serial::print(" oem=");
    print_ascii(&boot[3..11]);
    serial::print(" sectors_per_cluster=");
    serial::print_dec(boot[13] as u64);
    serial::println("");

    // 3. 分块读一致性（8 扇区 = 驱动一次搬运的上限）
    let mut span = [0u8; 4608];
    read_all(dev, 0, &mut span)?;
    let mut chunk = [0u8; 4096];
    read_all(dev, 0, &mut chunk)?;
    let mut tail = [0u8; SECTOR_SIZE as usize];
    read_all(dev, 8, &mut tail)?;
    if span[..4096] != chunk[..] {
        return Err("block: 4608-byte read disagrees with 4096-byte read");
    }
    if span[4096..] != tail[..] {
        return Err("block: cross-chunk tail disagrees with 512-byte read at lba 8");
    }
    if span[..SECTOR_SIZE as usize] != boot[..] {
        return Err("block: read at lba 0 is not stable");
    }
    serial::println("block: chunked read PASS (512/4096/4608 bytes consistent at lba 0)");

    // 4. 写回读：最后一个扇区（位于 FAT 数据区之外，且随后还原原值）
    let scratch = sectors - 1;
    let mut original = [0u8; SECTOR_SIZE as usize];
    read_all(dev, scratch, &mut original)?;
    let mut pattern = [0u8; SECTOR_SIZE as usize];
    for (i, byte) in pattern.iter_mut().enumerate() {
        *byte = (i as u8) ^ 0xA5;
    }
    if dev.write(scratch, &pattern)? != pattern.len() {
        return Err("block: short write");
    }
    let mut readback = [0u8; SECTOR_SIZE as usize];
    read_all(dev, scratch, &mut readback)?;
    if readback != pattern {
        return Err("block: write/readback mismatch");
    }
    // 还原：`make run` 直接写镜像（没有 snapshot=on），不留测试垃圾
    dev.write(scratch, &original)?;
    let mut restored = [0u8; SECTOR_SIZE as usize];
    read_all(dev, scratch, &mut restored)?;
    if restored != original {
        return Err("block: restoring the scratch sector failed");
    }
    serial::print("block: write/readback PASS (lba=");
    serial::print_dec(scratch);
    serial::println(" pattern=0xa5)");

    // 5. 负例
    let mut odd = [0u8; 100];
    if dev.read(0, &mut odd).is_ok() {
        return Err("block: unaligned read length was accepted");
    }
    if dev.write(0, &odd).is_ok() {
        return Err("block: unaligned write length was accepted");
    }
    let mut one = [0u8; SECTOR_SIZE as usize];
    if dev.read(sectors, &mut one).is_ok() {
        return Err("block: read beyond capacity was accepted");
    }
    if dev.write(sectors, &one).is_ok() {
        return Err("block: write beyond capacity was accepted");
    }
    if dev.read(u64::MAX - 1, &mut one).is_ok() {
        return Err("block: overflowing lba was accepted");
    }
    serial::println("block: negative cases PASS (unaligned len, lba beyond capacity, overflow)");

    serial::println("block: selftest PASS");
    Ok(())
}

/// 读满整个 `buf`，否则报错（短读说明设备语义被破坏）。
fn read_all(dev: &mut dyn BlockDevice, lba: u64, buf: &mut [u8]) -> Result<(), &'static str> {
    let got = dev.read(lba, &mut *buf)?;
    if got != buf.len() {
        return Err("block: short read");
    }
    Ok(())
}

/// 打印定长 ASCII 字段（FAT 的 OEM 名与类型串）：去尾部空格，不可打印字节打成 `.`。
fn print_ascii(bytes: &[u8]) {
    let end = bytes
        .iter()
        .rposition(|byte| *byte != b' ')
        .map_or(0, |index| index + 1);
    for &byte in &bytes[..end] {
        serial::putc(if (0x20..0x7F).contains(&byte) { byte } else { b'.' });
    }
}
