//! virtio-net 网卡驱动（v0.9 实现，**legacy virtio 0.9.5**）
//!
//! 与 [`crate::driver::virtio_blk`] 同源：同一套 legacy 寄存器、同样的「一个 PFN
//! 指向物理连续的队列区」硬约束、同样的**同步轮询** I/O 模型（不配置 MSI-X/INTx，
//! 提交后自旋等 used ring 前进）。差异只在队列数量与设备头部：
//!
//! | 项 | virtio-blk | virtio-net |
//! | ---- | ---- | ---- |
//! | 设备 ID | legacy `0x1001` / modern `0x1042` | legacy `0x1000` / modern `0x1041` |
//! | 队列 | 1 个（请求队列） | 2 个（0 = 接收，1 = 发送） |
//! | 每帧头部 | 无 | **10 字节** virtio-net 头（legacy，未协商 MRG_RXBUF） |
//! | 设备配置 | `capacity`（扇区数） | `mac`（6 字节 MAC） |
//!
//! `disable-modern=on`（Makefile / tests）强制设备暴露 legacy 接口；遇到 modern
//! 时 `attach` 打印明确原因而不是静默失败。
//!
//! ## 10 字节头从哪来
//!
//! virtio 1.0（或协商了 `VIRTIO_NET_F_MRG_RXBUF`）时头部是 12 字节；legacy 且未协商
//! MRG_RXBUF 时是 `struct virtio_net_hdr` 的 10 字节。本驱动只接受 `VIRTIO_NET_F_MAC`，
//! 因此头长固定 10。**发送**时这 10 字节要清零（flags=0 = 不做任何 offload）；
//! **接收**时设备把头部写在缓冲最前面，真正的以太网帧从偏移 10 开始。
//!
//! ## 缓冲与队列
//!
//! - 接收：预投 8 个 2 KiB 的静态缓冲（[`RX_POOL`]），每个缓冲带一个「设备可写」
//!   描述符；收到一帧后把它拷给上层并把该描述符重新投回 avail 环；
//! - 发送：一块静态缓冲（[`TX_POOL`]），每次把 `[10 字节头 + 帧]` 放进去，
//!   单个描述符交给设备，等 used ring 前进后返回（同一时刻只有一个发送在飞）。

use core::sync::atomic::{fence, Ordering};

use crate::arch::x86_64::port;
use crate::driver::driver::Driver;
use crate::driver::net::{self, NetworkDevice};
use crate::driver::pci::PciDevice;
use crate::driver::serial;
use crate::memory::frame;
use crate::memory::page::{align_up, PAGE_SIZE};
use crate::time::clock;

/// virtio 设备的 PCI 厂商 ID。
pub const VIRTIO_VENDOR: u16 = 0x1AF4;
/// virtio-net 的 legacy 设备 ID。
pub const VIRTIO_NET_LEGACY: u16 = 0x1000;
/// virtio-net 的 modern（virtio 1.0）设备 ID。
pub const VIRTIO_NET_MODERN: u16 = 0x1041;

// ---- legacy 寄存器偏移（相对 BAR0 I/O 窗口，与 virtio-blk 相同） ----
const REG_HOST_FEATURES: u16 = 0x00;
const REG_GUEST_FEATURES: u16 = 0x04;
const REG_QUEUE_ADDRESS: u16 = 0x08;
const REG_QUEUE_SIZE: u16 = 0x0C;
const REG_QUEUE_SELECT: u16 = 0x0E;
const REG_QUEUE_NOTIFY: u16 = 0x10;
const REG_STATUS: u16 = 0x12;
const REG_ISR: u16 = 0x13;
/// virtio-net 的设备配置起始偏移：`mac`（6 字节）。
const REG_CONFIG_MAC: u16 = 0x14;

// ---- 状态位 ----
const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FEATURES_OK: u8 = 8;

/// 特性位：`VIRTIO_NET_F_MAC`（bit 5）—— 协商后设备配置里的 MAC 才作数。
const FEATURE_MAC: u32 = 1 << 5;

/// 队列下标：接收队列。
const QUEUE_RX: u16 = 0;
/// 队列下标：发送队列。
const QUEUE_TX: u16 = 1;

// ---- 描述符标志 ----
const DESC_F_WRITE: u16 = 2;

/// virtio-net 头长度（legacy，未协商 MRG_RXBUF）。
const NET_HDR_LEN: usize = 10;
/// 单个缓冲的字节数（2 KiB，足够最大的 1514 字节以太网帧 + 头）。
const BUF_BYTES: usize = 2048;
/// 预投的接收缓冲数量。
const RX_BUFFERS: usize = 8;

/// 一次发送/接收的等待上限（PIT tick；100Hz → 500 tick = 5 秒）。
const IO_TIMEOUT_TICKS: u64 = 500;
/// 自旋上限（tick 不推进时的兜底）。
const IO_SPIN_LIMIT: u64 = 200_000_000;

/// 接收缓冲池（`RX_BUFFERS` 块，每块 [`BUF_BYTES`]；`.bss` 恒等映射 → 地址即物理地址）。
#[repr(align(4096))]
struct RxPool([u8; RX_BUFFERS * BUF_BYTES]);
static mut RX_POOL: RxPool = RxPool([0; RX_BUFFERS * BUF_BYTES]);

/// 发送缓冲（单块）。
#[repr(align(4096))]
struct TxPool([u8; BUF_BYTES]);
static mut TX_POOL: TxPool = TxPool([0; BUF_BYTES]);

/// 接收池首个缓冲的地址。
fn rx_pool_ptr() -> *mut u8 {
    unsafe { core::ptr::addr_of_mut!(RX_POOL.0) as *mut u8 }
}

/// 第 `i` 个接收缓冲的地址。
fn rx_buffer_addr(i: usize) -> u64 {
    rx_pool_ptr() as u64 + (i * BUF_BYTES) as u64
}

/// 发送缓冲的地址。
fn tx_pool_ptr() -> *mut u8 {
    unsafe { core::ptr::addr_of_mut!(TX_POOL.0) as *mut u8 }
}

/// 读取 PIT tick 计数（`clock::tick_count` 是 unsafe：调用方须处于内核态）。
fn ticks() -> u64 {
    // SAFETY: 驱动只在 ring0 运行。
    unsafe { clock::tick_count() }
}

/// 驱动实例状态。`Copy` 只是为了能用 `[VirtioNet::EMPTY; N]` 初始化静态数组。
#[derive(Clone, Copy)]
struct VirtioNet {
    name: &'static str,
    /// BAR0 I/O 窗口基址
    base: u16,
    /// MAC（设备配置空间读到的，或本地生成的兜底）
    mac: [u8; 6],
    /// 接收队列区起始物理地址
    rx_queue: u64,
    /// 发送队列区起始物理地址
    tx_queue: u64,
    /// 接收队列深度
    rx_num: u16,
    /// 发送队列深度
    tx_num: u16,
    /// 已消费到的接收 used 位置
    rx_last_used: u16,
    /// 已消费到的发送 used 位置
    tx_last_used: u16,
    /// 已预投的接收描述符数（= min(rx_num, RX_BUFFERS)）
    rx_posted: usize,
}

impl VirtioNet {
    const EMPTY: Self = Self {
        name: "virtio-net0",
        base: 0,
        mac: [0; 6],
        rx_queue: 0,
        tx_queue: 0,
        rx_num: 0,
        tx_num: 0,
        rx_last_used: 0,
        tx_last_used: 0,
        rx_posted: 0,
    };
}

/// 能驱动的网卡数上限（= [`crate::driver::net::MAX_NET_DEVICES`]）。
pub const MAX_DEVICES: usize = crate::driver::net::MAX_NET_DEVICES;

/// 驱动实例数组。
static mut DEVICES: [VirtioNet; MAX_DEVICES] = [VirtioNet::EMPTY; MAX_DEVICES];
/// 已被 `attach` 占用的实例数。
static mut ATTACHED: usize = 0;

/// 驱动对象：由 [`crate::driver::driver`] 的注册表引用（零大小，状态在 [`DEVICES`] 里）。
pub struct VirtioNetDriver;

/// `driver` 注册表里的一项。
pub static VIRTIO_NET_DRIVER: VirtioNetDriver = VirtioNetDriver;

impl Driver for VirtioNetDriver {
    fn name(&self) -> &'static str {
        "virtio-net-pci"
    }

    fn matches(&self, dev: &PciDevice) -> bool {
        dev.vendor == VIRTIO_VENDOR
            && (dev.device_id == VIRTIO_NET_LEGACY || dev.device_id == VIRTIO_NET_MODERN)
    }

    fn attach(&self, dev: &PciDevice) -> Result<(), &'static str> {
        attach(dev)
    }
}

/// 探测并初始化设备（填好实例状态、预投接收缓冲、注册到网络设备表）。
fn attach(dev: &PciDevice) -> Result<(), &'static str> {
    if dev.device_id == VIRTIO_NET_MODERN {
        serial::println(
            "virtio-net: modern (virtio 1.0, device 0x1041) not supported yet; expose legacy with disable-modern=on",
        );
        return Err("virtio 1.0 modern interface not supported yet");
    }

    let bar0 = dev.bar(0).ok_or("virtio-net: BAR0 missing")?;
    if !bar0.io {
        return Err("virtio-net: BAR0 is not an I/O window");
    }
    let base = bar0.address as u16;
    serial::print("virtio-net: legacy BAR0=");
    serial::print_hex(base as u64);
    serial::print(" size=");
    serial::print_hex(bar0.size);
    serial::println("");

    let host_features = unsafe { port::inl(base + REG_HOST_FEATURES) };
    serial::print("virtio-net: host features=");
    serial::print_hex(host_features as u64);
    serial::println("");

    // 1. 复位 → ACKNOWLEDGE → DRIVER
    unsafe {
        port::outb(base + REG_STATUS, 0);
        port::outb(base + REG_STATUS, STATUS_ACKNOWLEDGE);
        port::outb(base + REG_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER);
    }
    // 2. 只协商 MAC（拿到设备配置里的 MAC）；其余 offload 特性一律不接受
    let guest_features = if host_features & FEATURE_MAC != 0 {
        FEATURE_MAC
    } else {
        0
    };
    unsafe { port::outl(base + REG_GUEST_FEATURES, guest_features) };
    serial::print("virtio-net: guest features=");
    serial::print_hex(guest_features as u64);
    serial::println("");

    // 3. FEATURES_OK
    unsafe {
        let status = port::inb(base + REG_STATUS) | STATUS_FEATURES_OK;
        port::outb(base + REG_STATUS, status);
    }
    if unsafe { port::inb(base + REG_STATUS) } & STATUS_FEATURES_OK == 0 {
        serial::println("virtio-net: FEATURES_OK not retained (legacy device behavior, continuing)");
    }

    // 4. 两个队列：0 = 接收，1 = 发送
    let (rx_queue, rx_num) = setup_queue(base, QUEUE_RX)?;
    let (tx_queue, tx_num) = setup_queue(base, QUEUE_TX)?;
    serial::print("virtio-net: rx queue size=");
    serial::print_dec(rx_num as u64);
    serial::print(" tx queue size=");
    serial::print_dec(tx_num as u64);
    serial::println("");

    // 5. MAC（协商了 MAC 就读设备配置，否则用本地管理的兜底地址）
    let mut mac = [0u8; 6];
    if guest_features & FEATURE_MAC != 0 {
        for (i, byte) in mac.iter_mut().enumerate() {
            *byte = unsafe { port::inb(base + REG_CONFIG_MAC + i as u16) };
        }
    }
    // 兜底用一个本地管理的地址，且**刻意不同于 QEMU 的默认 MAC（52:54:00:12:34:56）**：
    // 这样验收里看到 52:54:... 就只可能来自「真的读了设备配置空间」，而不是兜底。
    if mac == [0u8; 6] || mac == [0xFFu8; 6] {
        mac = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
    }
    serial::print("virtio-net: mac=");
    net::print_mac(mac);
    serial::println("");

    // 6. 预投接收缓冲（写描述符 + avail 环）
    let rx_posted = (rx_num as usize).min(RX_BUFFERS);
    post_rx_buffers(rx_queue, rx_num, rx_posted);

    // 7. DRIVER_OK → 通知接收队列「有缓冲可用」
    unsafe {
        port::outb(
            base + REG_STATUS,
            STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK,
        );
    }
    let status = unsafe { port::inb(base + REG_STATUS) };
    serial::print("virtio-net: status=");
    serial::print_hex(status as u64);
    serial::println(if status & STATUS_DRIVER_OK != 0 {
        " (DRIVER_OK)"
    } else {
        " (DRIVER_OK missing!)"
    });
    if status & STATUS_DRIVER_OK == 0 {
        return Err("virtio-net: DRIVER_OK not set");
    }
    fence(Ordering::SeqCst);
    unsafe { port::outw(base + REG_QUEUE_NOTIFY, QUEUE_RX) };
    let _ = unsafe { port::inb(base + REG_ISR) }; // 清 legacy 中断位（用轮询）

    // 8. 占用实例槽位并注册
    let slot = unsafe {
        let used = *(&raw const ATTACHED);
        if used >= MAX_DEVICES {
            return Err("virtio-net: no free device slot");
        }
        let table = &mut *(&raw mut DEVICES);
        let state = &mut table[used];
        state.name = "virtio-net0";
        state.base = base;
        state.mac = mac;
        state.rx_queue = rx_queue;
        state.tx_queue = tx_queue;
        state.rx_num = rx_num;
        state.tx_num = tx_num;
        state.rx_last_used = 0;
        state.tx_last_used = 0;
        state.rx_posted = rx_posted;
        *(&raw mut ATTACHED) = used + 1;
        used
    };
    let index = net::register(unsafe { &mut *(&raw mut DEVICES[slot]) })?;
    serial::print("virtio-net: registered as network device ");
    serial::print_dec(index as u64);
    serial::println(" name=virtio-net0");
    Ok(())
}

/// 初始化一个队列：读深度 → 分配物理连续的队列区 → 写回 size 与 PFN。
fn setup_queue(base: u16, index: u16) -> Result<(u64, u16), &'static str> {
    unsafe { port::outw(base + REG_QUEUE_SELECT, index) };
    let num = unsafe { port::inw(base + REG_QUEUE_SIZE) };
    if num == 0 || num > 1024 {
        return Err("virtio-net: queue size out of range");
    }
    let bytes = queue_region_bytes(num);
    let pages = (bytes.div_ceil(PAGE_SIZE)) as usize;
    let frame = frame::allocate_contiguous(pages)
        .ok_or("virtio-net: cannot allocate contiguous virtqueue frames")?;
    let queue = frame.start_address();
    unsafe {
        core::ptr::write_bytes(queue as *mut u8, 0, pages * PAGE_SIZE as usize);
        port::outw(base + REG_QUEUE_SIZE, num);
        port::outl(base + REG_QUEUE_ADDRESS, (queue >> 12) as u32);
    }
    Ok((queue, num))
}

/// 把前 `count` 个接收描述符指向各自的池缓冲，并把它们投进 avail 环。
fn post_rx_buffers(queue: u64, num: u16, count: usize) {
    for i in 0..count {
        let desc = desc_addr(queue, i);
        unsafe {
            write_u64(desc, rx_buffer_addr(i));
            write_u32(desc + 8, BUF_BYTES as u32);
            write_u16(desc + 12, DESC_F_WRITE); // 设备写这块缓冲
            write_u16(desc + 14, 0);
        }
    }
    let avail = avail_addr(queue, num);
    for i in 0..count {
        unsafe { write_u16(avail + 4 + 2 * i as u64, i as u16) };
    }
    unsafe { write_u16(avail + 2, count as u16) }; // avail.idx
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

fn desc_addr(queue: u64, i: usize) -> u64 {
    queue + (i as u64) * 16
}

fn avail_addr(queue: u64, num: u16) -> u64 {
    queue + 16 * num as u64
}

fn used_addr(queue: u64, num: u16) -> u64 {
    let desc = 16 * num as u64;
    let avail = 4 + 2 * num as u64;
    queue + align_up(desc + avail, PAGE_SIZE)
}

// 环上的读写一律用 volatile：这些内存同时被设备读写，编译器不能缓存或重排。
unsafe fn read_u16(addr: u64) -> u16 {
    unsafe { core::ptr::read_volatile(addr as *const u16) }
}
unsafe fn read_u32(addr: u64) -> u32 {
    unsafe { core::ptr::read_volatile(addr as *const u32) }
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

impl NetworkDevice for VirtioNet {
    fn name(&self) -> &'static str {
        self.name
    }

    fn mac(&self) -> [u8; 6] {
        self.mac
    }

    fn transmit(&mut self, frame: &[u8]) -> Result<(), &'static str> {
        let len = NET_HDR_LEN + frame.len();
        if frame.is_empty() || len > BUF_BYTES {
            return Err("virtio-net: frame length out of range");
        }
        // 组装 [10 字节头（全 0）+ 帧] 到发送缓冲
        unsafe {
            core::ptr::write_bytes(tx_pool_ptr(), 0, NET_HDR_LEN);
            core::ptr::copy_nonoverlapping(frame.as_ptr(), tx_pool_ptr().add(NET_HDR_LEN), frame.len());
        }
        let d0 = desc_addr(self.tx_queue, 0);
        unsafe {
            write_u64(d0, tx_pool_ptr() as u64);
            write_u32(d0 + 8, len as u32);
            write_u16(d0 + 12, 0); // 设备读这块缓冲
            write_u16(d0 + 14, 0);
        }
        // avail 环：放入描述符 0
        let avail = avail_addr(self.tx_queue, self.tx_num);
        let idx = unsafe { read_u16(avail + 2) };
        unsafe { write_u16(avail + 4 + 2 * (idx % self.tx_num) as u64, 0) };
        unsafe { write_u16(avail + 2, idx.wrapping_add(1)) };
        fence(Ordering::SeqCst);
        unsafe { port::outw(self.base + REG_QUEUE_NOTIFY, QUEUE_TX) };

        // 轮询发送 used ring
        let used = used_addr(self.tx_queue, self.tx_num);
        let start = ticks();
        let mut spins = 0u64;
        loop {
            fence(Ordering::SeqCst);
            if unsafe { read_u16(used + 2) } != self.tx_last_used {
                break;
            }
            spins += 1;
            if spins > IO_SPIN_LIMIT || ticks().wrapping_sub(start) > IO_TIMEOUT_TICKS {
                return Err("virtio-net: transmit timed out");
            }
            core::hint::spin_loop();
        }
        self.tx_last_used = self.tx_last_used.wrapping_add(1);
        Ok(())
    }

    fn receive(&mut self, buf: &mut [u8]) -> Result<Option<usize>, &'static str> {
        let used = used_addr(self.rx_queue, self.rx_num);
        let used_idx = unsafe { read_u16(used + 2) };
        if used_idx == self.rx_last_used {
            return Ok(None);
        }
        let slot = (self.rx_last_used % self.rx_num) as u64;
        let entry = used + 4 + 8 * slot;
        let id = unsafe { read_u32(entry) } as usize;
        let len = unsafe { read_u32(entry + 4) } as usize;
        self.rx_last_used = self.rx_last_used.wrapping_add(1);

        if id >= self.rx_posted {
            return Err("virtio-net: rx descriptor id out of range");
        }
        // 头部之外才是以太网帧；长度异常就丢掉并重新投递，不谎报
        if len < NET_HDR_LEN || len > BUF_BYTES {
            self.repost_rx(id);
            return Ok(None);
        }
        let payload_len = len - NET_HDR_LEN;
        if payload_len > buf.len() {
            self.repost_rx(id);
            return Err("virtio-net: receive buffer too small");
        }
        unsafe {
            let src = (rx_buffer_addr(id) + NET_HDR_LEN as u64) as *const u8;
            core::ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), payload_len);
        }
        self.repost_rx(id);
        Ok(Some(payload_len))
    }
}

impl VirtioNet {
    /// 把第 `id` 个接收描述符重新投回 avail 环并通知设备。
    fn repost_rx(&mut self, id: usize) {
        let avail = avail_addr(self.rx_queue, self.rx_num);
        let idx = unsafe { read_u16(avail + 2) };
        unsafe { write_u16(avail + 4 + 2 * (idx % self.rx_num) as u64, id as u16) };
        unsafe { write_u16(avail + 2, idx.wrapping_add(1)) };
        fence(Ordering::SeqCst);
        unsafe { port::outw(self.base + REG_QUEUE_NOTIFY, QUEUE_RX) };
    }
}
