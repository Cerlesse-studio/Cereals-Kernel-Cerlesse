//! virtio-blk 块设备驱动（v0.8 实现，**legacy virtio 0.9.5**）
//!
//! ## 为什么是 virtio-blk（而不是 AHCI / 老式 ATA）
//!
//! | 方案 | 代价 |
//! | ---- | ---- |
//! | 老式 ATA（PIO，端口 `0x1F0`） | 代码最短，但 q35 的存储控制器是 **ich9-ahci**，不保证暴露 0x1F0 端口组；换机器就废 |
//! | AHCI（MMIO + 命令表 + FIS） | 功能齐全，但要实现命令列表/FIS/PRDT 一大套，v0.8 的收益不足 |
//! | **virtio-blk（本模块）** | 队列语义简单、QEMU 与云上真实机器都常见；legacy 接口只要一个 I/O BAR |
//!
//! ## legacy（0.9.5）与 modern（1.0）的分界
//!
//! 设备 ID 决定接口：`0x1001` = legacy（BAR0 是 I/O 窗口，寄存器是简单偏移），
//! `0x1042` = modern（寄存器在 capability 指向的 MMIO 里，队列布局也不同）。
//! QEMU 的 `virtio-blk-pci` 默认 `disable-legacy=auto`，在 q35（PCIe）上可能只暴露
//! modern；本模块只实现 legacy，因此**测试与 run 的 QEMU 命令行固定加
//! `disable-modern=on`**（见 Makefile / tests/*.py）。真遇到 modern 设备时
//! `attach` 会打印明确原因而不是静默失败。
//!
//! ## 队列布局（legacy 的硬约束）
//!
//! 队列地址寄存器只装一个 **PFN**（物理页号），所以三张表必须落在**同一块物理连续**
//! 区域内（[`crate::memory::frame::allocate_contiguous`] 就是为它加的）：
//!
//! ```text
//! base + 0        descriptor table   16 B × num
//! base + 16*num   avail ring         4 B + 2 B × num
//! base + 4096     used ring          4 B + 8 B × num   ← 必须 4 KiB 对齐
//! ```
//!
//! ## I/O 模型：**同步轮询**
//!
//! 不使用中断（v0.8 不配置 MSI-X/INTx），每次请求提交后自旋等待 used ring 的 idx 前进，
//! 超时上限由 PIT tick（[`IO_TIMEOUT_TICKS`]）给出。轮询让驱动不依赖 IRQ 分发，
//! 也避免在中断上下文里做块 I/O；代价是等待期间忙等 —— v0.9 起再改为中断 + 等待队列。
//!
//! 缓冲策略：调用方的缓冲长度任意（只要整扇区对齐），驱动内部用一块**页对齐静态
//! bounce buffer**（[`BOUNCE_BYTES`] = 8 个扇区）分块搬运，因此不要求调用方缓冲
//! 物理连续、也不要求对齐 —— 用一次拷贝换掉一整类 DMA 对齐问题。

use core::sync::atomic::{fence, Ordering};

use crate::arch::x86_64::port;
use crate::driver::block::{self, is_sector_aligned, BlockDevice, SECTOR_SIZE};
use crate::driver::driver::Driver;
use crate::driver::pci::PciDevice;
use crate::driver::serial;
use crate::memory::frame;
use crate::memory::page::{align_up, PAGE_SIZE};
use crate::time::clock;

/// virtio 设备的 PCI 厂商 ID。
pub const VIRTIO_VENDOR: u16 = 0x1AF4;
/// virtio-blk 的 legacy 设备 ID。
pub const VIRTIO_BLK_LEGACY: u16 = 0x1001;
/// virtio-blk 的 modern（virtio 1.0）设备 ID。
pub const VIRTIO_BLK_MODERN: u16 = 0x1042;

// ---- legacy 寄存器偏移（相对 BAR0 I/O 窗口） ----
const REG_HOST_FEATURES: u16 = 0x00;
const REG_GUEST_FEATURES: u16 = 0x04;
const REG_QUEUE_ADDRESS: u16 = 0x08;
const REG_QUEUE_SIZE: u16 = 0x0C;
const REG_QUEUE_SELECT: u16 = 0x0E;
const REG_QUEUE_NOTIFY: u16 = 0x10;
const REG_STATUS: u16 = 0x12;
const REG_ISR: u16 = 0x13;
/// virtio-blk 的设备配置起始偏移：`capacity`（u64，扇区数）。
const REG_CONFIG_CAPACITY: u16 = 0x14;

// ---- 状态位 ----
const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FEATURES_OK: u8 = 8;

// ---- 请求类型 ----
const BLK_T_IN: u32 = 0; // 读（设备 → 内存）
const BLK_T_OUT: u32 = 1; // 写（内存 → 设备）

// ---- 描述符标志 ----
const DESC_F_NEXT: u16 = 1;
const DESC_F_WRITE: u16 = 2;

/// 单次请求最多搬运的扇区数（bounce buffer 大小 / 扇区大小）。
const BOUNCE_SECTORS: usize = 8;
/// bounce buffer 字节数（页对齐，正好 4 KiB）。
const BOUNCE_BYTES: usize = BOUNCE_SECTORS * SECTOR_SIZE as usize;
/// 一次 I/O 的等待上限（PIT tick；100Hz → 500 tick = 5 秒）。
const IO_TIMEOUT_TICKS: u64 = 500;
/// 自旋上限（tick 不推进时的兜底，避免内核永久挂住）。
const IO_SPIN_LIMIT: u64 = 200_000_000;

/// virtio-blk 请求头（16 字节，`#[repr(C)]`）。
#[repr(C)]
struct BlkReq {
    type_: u32,
    reserved: u32,
    sector: u64,
}

/// 页对齐的 bounce buffer（静态 `.bss`，内核恒等映射 → 虚拟地址即物理地址）。
#[repr(align(4096))]
struct Bounce([u8; BOUNCE_BYTES]);

static mut BOUNCE: Bounce = Bounce([0; BOUNCE_BYTES]);
/// 请求头（同一时刻只有一个请求在飞，见模块头注释）。
static mut REQ: BlkReq = BlkReq {
    type_: 0,
    reserved: 0,
    sector: 0,
};
/// 状态字节：设备在请求完成后写入（0 = OK）。
static mut REQ_STATUS: u8 = 0xFF;

/// 驱动状态（`attach` 期间填好，之后经 `block` 注册表访问）。
///
/// 每个实例对应一块盘；`Copy` 只是为了能用 `[VirtioBlk::EMPTY; N]` 初始化静态数组。
#[derive(Clone, Copy)]
struct VirtioBlk {
    name: &'static str,
    /// BAR0 I/O 窗口基址
    base: u16,
    /// 队列区起始物理地址（2 页连续帧）
    queue: u64,
    /// 队列深度（= 设备报告的最大值）
    num: u16,
    /// used ring 中已消费到的位置
    last_used: u16,
    /// 容量（扇区）
    capacity: u64,
}

impl VirtioBlk {
    const EMPTY: Self = Self {
        name: "virtio-blk0",
        base: 0,
        queue: 0,
        num: 0,
        last_used: 0,
        capacity: 0,
    };
}

/// 同一时刻能驱动的盘数上限（= [`crate::driver::block::MAX_BLOCK_DEVICES`]）。
///
/// v0.8 需要两块：启动盘（ESP，固件写的 FAT）与 `mkfs` 生成的测试盘。
pub const MAX_DEVICES: usize = crate::driver::block::MAX_BLOCK_DEVICES;

/// 实例名表（`BlockDevice::name` 要 `&'static str`，所以名字得来自静态表）。
///
/// CSS-1 起盘数上限就是 [`MAX_DEVICES`]：名字必须跟着实例号走 —— v0.8 只区分
/// 「0 号」与「非 0 号」，第三块盘起会全部显示成 `virtio-blk1`（证据行/日志对不上设备）。
static NAMES: [&str; MAX_DEVICES] = [
    "virtio-blk0",
    "virtio-blk1",
    "virtio-blk2",
    "virtio-blk3",
    "virtio-blk4",
    "virtio-blk5",
    "virtio-blk6",
    "virtio-blk7",
];

/// 驱动实例数组：`attach` 按顺序占用，槽位号就是块设备号。
static mut DEVICES: [VirtioBlk; MAX_DEVICES] = [VirtioBlk::EMPTY; MAX_DEVICES];
/// 已被 `attach` 占用的实例数。
static mut ATTACHED: usize = 0;

/// bounce buffer 的起始地址（4 KiB 对齐；`.bss` 恒等映射 → 虚拟地址即物理地址）。
///
/// `addr_of_mut!` 只取地址、不建引用（因此不违反 `static mut` 的别名规则），
/// 但「对 static mut 做字段投影」仍必须写在 `unsafe` 块里（E0133）。
/// 相比 `&mut BOUNCE`，这里从不构造引用，取到的指针就是数组首字节的地址。
fn bounce_ptr() -> *mut u8 {
    // SAFETY: 仅计算地址。
    unsafe { core::ptr::addr_of_mut!(BOUNCE.0) as *mut u8 }
}

/// 驱动对象：由 [`crate::driver::driver`] 的注册表引用（零大小，状态在 [`DEVICES`] 里）。
pub struct VirtioBlkDriver;

/// `driver` 注册表里的一项。
pub static VIRTIO_BLK_DRIVER: VirtioBlkDriver = VirtioBlkDriver;

impl Driver for VirtioBlkDriver {
    fn name(&self) -> &'static str {
        "virtio-blk-pci"
    }

    fn matches(&self, dev: &PciDevice) -> bool {
        dev.vendor == VIRTIO_VENDOR
            && (dev.device_id == VIRTIO_BLK_LEGACY || dev.device_id == VIRTIO_BLK_MODERN)
    }

    fn attach(&self, dev: &PciDevice) -> Result<(), &'static str> {
        attach(dev)
    }
}

/// 探测并初始化设备（把内存里的设备状态填好，并注册到块设备表）。
fn attach(dev: &PciDevice) -> Result<(), &'static str> {
    if dev.device_id == VIRTIO_BLK_MODERN {
        serial::println(
            "virtio-blk: modern (virtio 1.0, device 0x1042) not supported yet; expose legacy with disable-modern=on",
        );
        return Err("virtio 1.0 modern interface not supported yet");
    }

    let bar0 = dev.bar(0).ok_or("virtio-blk: BAR0 missing")?;
    if !bar0.io {
        return Err("virtio-blk: BAR0 is not an I/O window");
    }
    let base = bar0.address as u16;
    serial::print("virtio-blk: legacy BAR0=");
    serial::print_hex(base as u64);
    serial::print(" size=");
    serial::print_hex(bar0.size);
    serial::println("");

    let host_features = unsafe { port::inl(base + REG_HOST_FEATURES) };
    serial::print("virtio-blk: host features=");
    serial::print_hex(host_features as u64);
    serial::println("");

    // 1. 复位 → ACKNOWLEDGE → DRIVER
    unsafe {
        port::outb(base + REG_STATUS, 0);
        port::outb(base + REG_STATUS, STATUS_ACKNOWLEDGE);
        port::outb(base + REG_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER);
        // 2. 不接受任何可选特性（v0.8 不需要 INDIRECT/EVENT_IDX 等）
        port::outl(base + REG_GUEST_FEATURES, 0);
    }
    serial::println("virtio-blk: guest features=0x0");

    // 3. FEATURES_OK（legacy 设备可能忽略该位，因此只记录不强制）
    unsafe {
        let status = port::inb(base + REG_STATUS) | STATUS_FEATURES_OK;
        port::outb(base + REG_STATUS, status);
    }
    let status = unsafe { port::inb(base + REG_STATUS) };
    if status & STATUS_FEATURES_OK == 0 {
        serial::println("virtio-blk: FEATURES_OK not retained (legacy device behavior, continuing)");
    }

    // 4. 队列 0：读最大深度 → 分配连续帧 → 报 PFN
    unsafe { port::outw(base + REG_QUEUE_SELECT, 0) };
    let num = unsafe { port::inw(base + REG_QUEUE_SIZE) };
    if num == 0 || num > 1024 {
        return Err("virtio-blk: queue size out of range");
    }
    let queue_bytes = queue_region_bytes(num);
    let pages = (queue_bytes.div_ceil(PAGE_SIZE)) as usize;
    let queue_frame = frame::allocate_contiguous(pages)
        .ok_or("virtio-blk: cannot allocate contiguous virtqueue frames")?;
    let queue = queue_frame.start_address();
    unsafe {
        core::ptr::write_bytes(queue as *mut u8, 0, pages * PAGE_SIZE as usize);
        // 告诉设备我们使用它的最大深度；PFN = 物理页号
        port::outw(base + REG_QUEUE_SIZE, num);
        port::outl(base + REG_QUEUE_ADDRESS, (queue >> 12) as u32);
    }
    serial::print("virtio-blk: queue0 size=");
    serial::print_dec(num as u64);
    serial::print(" region=");
    serial::print_hex(queue);
    serial::print(" (");
    serial::print_dec(pages as u64);
    serial::println(" pages contiguous)");

    // 5. 容量（设备配置：u64 扇区数）
    let capacity = unsafe { port::inl(base + REG_CONFIG_CAPACITY) as u64 }
        | ((unsafe { port::inl(base + REG_CONFIG_CAPACITY + 4) } as u64) << 32);
    if capacity == 0 {
        return Err("virtio-blk: device reports zero capacity");
    }
    serial::print("virtio-blk: capacity=");
    serial::print_dec(capacity);
    serial::print(" sectors (");
    serial::print_dec(capacity * SECTOR_SIZE as u64 / (1024 * 1024));
    serial::println(" MiB)");

    // 6. DRIVER_OK
    unsafe { port::outb(base + REG_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK) };
    let status = unsafe { port::inb(base + REG_STATUS) };
    serial::print("virtio-blk: status=");
    serial::print_hex(status as u64);
    serial::println(if status & STATUS_DRIVER_OK != 0 {
        " (DRIVER_OK)"
    } else {
        " (DRIVER_OK missing!)"
    });
    if status & STATUS_DRIVER_OK == 0 {
        return Err("virtio-blk: DRIVER_OK not set");
    }
    // ISR 读一次清掉 legacy 中断位（我们用轮询，不处理中断）
    let _ = unsafe { port::inb(base + REG_ISR) };

    // 7. 占用一个实例槽位（槽位号 = 块设备号）并填状态
    //
    // 多块盘的关键就在这里：每块盘的队列/容量/used 游标都在**各自的实例**里，
    // 静止的请求缓冲（REQ/BOUNCE/REQ_STATUS）则共用 —— 单核内核里一次只有一个请求在飞
    //（`transfer` 收 `&mut self`，`block::get()` 也只给一个 `&mut`）。
    let slot = unsafe {
        let used = *(&raw const ATTACHED);
        if used >= MAX_DEVICES {
            return Err("virtio-blk: no free device slot");
        }
        let table = &mut *(&raw mut DEVICES);
        let state = &mut table[used];
        state.name = NAMES[used];
        state.base = base;
        state.queue = queue;
        state.num = num;
        state.last_used = 0;
        state.capacity = capacity;
        *(&raw mut ATTACHED) = used + 1;
        used
    };
    let index = block::register(unsafe { &mut *(&raw mut DEVICES[slot]) })?;
    serial::print("virtio-blk: registered as block device ");
    serial::print_dec(index as u64);
    serial::print(" name=");
    serial::println(NAMES[slot]);
    Ok(())
}

/// 队列区需要的字节数（descriptor + avail + 4 KiB 对齐的 used）。
fn queue_region_bytes(num: u16) -> u64 {
    let desc = 16 * num as u64;
    let avail = 4 + 2 * num as u64;
    let used_start = align_up(desc + avail, PAGE_SIZE);
    let used = 4 + 8 * num as u64;
    used_start + used
}

// ---- 队列字段寻址 ----

/// 第 `i` 个描述符的地址。
fn desc_addr(queue: u64, i: usize) -> u64 {
    queue + (i as u64) * 16
}

/// avail ring 起始地址。
fn avail_addr(queue: u64, num: u16) -> u64 {
    queue + 16 * num as u64
}

/// used ring 起始地址（必须 4 KiB 对齐）。
fn used_addr(queue: u64, num: u16) -> u64 {
    let desc = 16 * num as u64;
    let avail = 4 + 2 * num as u64;
    queue + align_up(desc + avail, PAGE_SIZE)
}

// 环上的读写一律用 volatile：这些内存同时被设备读写，编译器不能缓存或重排。
unsafe fn read_u16(addr: u64) -> u16 {
    unsafe { core::ptr::read_volatile(addr as *const u16) }
}
unsafe fn write_u16(addr: u64, value: u16) {
    unsafe { core::ptr::write_volatile(addr as *mut u16, value) }
}
unsafe fn write_u32(addr: u64, value: u32) {
    unsafe { core::ptr::write_volatile(addr as *mut u32, value) }
}
unsafe fn write_u64(addr: u64, value: u64) {
    unsafe { core::ptr::write_volatile(addr as *mut u64, value) }
}

impl BlockDevice for VirtioBlk {
    fn name(&self) -> &'static str {
        self.name
    }

    fn sector_size(&self) -> u32 {
        SECTOR_SIZE
    }

    fn sectors(&self) -> u64 {
        self.capacity
    }

    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<usize, &'static str> {
        if !is_sector_aligned(buf.len()) {
            return Err("virtio-blk: read length not a multiple of sector size");
        }
        let mut done = 0usize;
        while done < buf.len() {
            let chunk = (buf.len() - done).min(BOUNCE_BYTES);
            let sectors = chunk / SECTOR_SIZE as usize;
            self.transfer(
                BLK_T_IN,
                lba + (done / SECTOR_SIZE as usize) as u64,
                sectors,
                sectors * SECTOR_SIZE as usize,
            )?;
            // SAFETY: bounce buffer 是 `BOUNCE_BYTES` 大小的静态数组，且上面的
            // `transfer` 已经把它填好；`chunk <= BOUNCE_BYTES`。
            let bounce = unsafe { core::slice::from_raw_parts(bounce_ptr() as *const u8, chunk) };
            buf[done..done + chunk].copy_from_slice(bounce);
            done += chunk;
        }
        Ok(done)
    }

    fn write(&mut self, lba: u64, data: &[u8]) -> Result<usize, &'static str> {
        if !is_sector_aligned(data.len()) {
            return Err("virtio-blk: write length not a multiple of sector size");
        }
        let mut done = 0usize;
        while done < data.len() {
            let chunk = (data.len() - done).min(BOUNCE_BYTES);
            let sectors = chunk / SECTOR_SIZE as usize;
            // SAFETY: 同 `read`；`chunk <= BOUNCE_BYTES`。
            let to_bounce = unsafe { core::slice::from_raw_parts_mut(bounce_ptr(), chunk) };
            to_bounce.copy_from_slice(&data[done..done + chunk]);
            self.transfer(BLK_T_OUT, lba + (done / SECTOR_SIZE as usize) as u64, sectors, chunk)?;
            done += chunk;
        }
        Ok(done)
    }
}

impl VirtioBlk {
    /// 提交一次请求并轮询等待完成。
    ///
    /// `sectors` 个扇区的数据交换发生在 `BOUNCE` 的前 `len` 字节里
    /// （读：设备 → bounce；写：bounce → 设备）。
    fn transfer(
        &mut self,
        type_: u32,
        lba: u64,
        sectors: usize,
        len: usize,
    ) -> Result<(), &'static str> {
        if sectors == 0 || sectors * SECTOR_SIZE as usize > BOUNCE_BYTES {
            return Err("virtio-blk: request size out of range");
        }
        // `checked_add` 而不是 `lba + sectors`：调用方给的 lba 可能接近 u64::MAX，
        // 环绕后的和会「看起来」在容量内，于是把越界请求发给设备。
        let end = lba
            .checked_add(sectors as u64)
            .ok_or("virtio-blk: sector address overflow")?;
        if end > self.capacity {
            return Err("virtio-blk: request beyond device capacity");
        }

        let req = unsafe { &mut *(&raw mut REQ) };
        req.type_ = type_;
        req.reserved = 0;
        req.sector = lba;
        unsafe { core::ptr::write_volatile(&raw mut REQ_STATUS, 0xFF) };

        let req_addr = (&raw const REQ) as u64;
        let data_addr = bounce_ptr() as u64;
        let status_addr = (&raw const REQ_STATUS) as u64;

        // 描述符链：头(设备读) → 数据(读=设备写 / 写=设备读) → 状态(设备写)
        let d0 = desc_addr(self.queue, 0);
        unsafe {
            write_u64(d0, req_addr);
            write_u32(d0 + 8, core::mem::size_of::<BlkReq>() as u32);
            write_u16(d0 + 12, DESC_F_NEXT);
            write_u16(d0 + 14, 1);

            let d1 = desc_addr(self.queue, 1);
            write_u64(d1, data_addr);
            write_u32(d1 + 8, len as u32);
            let data_flags = DESC_F_NEXT | if type_ == BLK_T_IN { DESC_F_WRITE } else { 0 };
            write_u16(d1 + 12, data_flags);
            write_u16(d1 + 14, 2);

            let d2 = desc_addr(self.queue, 2);
            write_u64(d2, status_addr);
            write_u32(d2 + 8, 1);
            write_u16(d2 + 12, DESC_F_WRITE);
            write_u16(d2 + 14, 0);
        }

        // avail ring：把描述符链头（0）放进去并递增 idx
        let avail = avail_addr(self.queue, self.num);
        let avail_idx = unsafe { read_u16(avail + 2) };
        unsafe { write_u16(avail + 4 + 2 * (avail_idx % self.num) as u64, 0) };
        unsafe { write_u16(avail + 2, avail_idx.wrapping_add(1)) };

        // 设备必须看到描述符与 avail 更新之后才收到通知
        fence(Ordering::SeqCst);
        unsafe { port::outw(self.base + REG_QUEUE_NOTIFY, 0) };

        // 轮询 used ring
        let used = used_addr(self.queue, self.num);
        let start_tick = unsafe { clock::tick_count() };
        let mut spins = 0u64;
        loop {
            fence(Ordering::SeqCst);
            let used_idx = unsafe { read_u16(used + 2) };
            if used_idx != self.last_used {
                break;
            }
            spins += 1;
            if spins > IO_SPIN_LIMIT || unsafe { clock::tick_count() } - start_tick > IO_TIMEOUT_TICKS {
                serial::print("virtio-blk: request timeout (lba=");
                serial::print_dec(lba);
                serial::print(" sectors=");
                serial::print_dec(sectors as u64);
                serial::println(")");
                return Err("virtio-blk: request timed out");
            }
            core::hint::spin_loop();
        }
        self.last_used = self.last_used.wrapping_add(1);

        let status = unsafe { core::ptr::read_volatile(&raw const REQ_STATUS) };
        if status != 0 {
            serial::print("virtio-blk: device returned status=");
            serial::print_dec(status as u64);
            serial::println("");
            return Err("virtio-blk: device reported I/O error");
        }
        Ok(())
    }
}
