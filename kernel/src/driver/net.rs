//! 网络设备抽象与注册表（v0.9 实现）
//!
//! 与 [`crate::driver::block`] 同一种收口思路：上层（[`crate::network`] 的
//! Ethernet/ARP/IPv4/UDP）只认 [`NetworkDevice`] 这个 trait，不认 virtio-net / e1000。
//! **后端可换，接口不变**；v0.9 只有一个后端（[`crate::driver::virtio_net`]）。
//!
//! 粒度约定：
//!
//! - 交换单位是**完整以太网帧**（不含 4 字节 FCS/校验尾 —— 网卡与驱动都不把它交给上层）；
//! - `transmit` 收的是调用方拼好的整帧，驱动只负责加设备自己的封装（virtio-net 的
//!   12/10 字节头），不解析上层协议；
//! - `receive` 是**非阻塞轮询**：没有帧就返回 `Ok(None)`，由上层自己决定超时
//!   （[`crate::network::Stack`] 用 PIT tick 做上限）。这一版没有中断/等待队列 ——
//!   与 v0.8 的块设备同步轮询是同一个取舍；
//! - 调用方缓冲不得小于 [`FRAME_MAX`]：驱动会把整帧拷进去，缓冲不足直接报错
//!   而不是静默截断。
//!
//! 注册表是固定容量数组（[`MAX_NET_DEVICES`]）而不是 `Vec`：它保存
//! `&'static mut dyn NetworkDevice`，写入一次、读多次，容量固定能让借用关系一眼看清。

use crate::driver::serial;

/// 单帧字节上限（不含 FCS）。
///
/// 经典以太网 MTU = 1500，帧头 14 字节 → 1514；留到 2048 以吸收
/// virtio-net 的头部与驱动内部对齐，不依赖 1514 这个精确值。
pub const FRAME_MAX: usize = 2048;

/// 网络设备数量上限（v0.9 只有一块 virtio-net；留一个位置给未来的多网卡）。
pub const MAX_NET_DEVICES: usize = 2;

/// 网络设备：按以太网帧收发。
pub trait NetworkDevice {
    /// 设备名（日志与错误信息里用，例如 `virtio-net0`）。
    fn name(&self) -> &'static str;

    /// 本机 MAC（6 字节，设备配置空间里读到的，不是硬编码）。
    fn mac(&self) -> [u8; 6];

    /// 发送一帧（`frame` 是**不含 FCS** 的完整以太网帧）。
    fn transmit(&mut self, frame: &[u8]) -> Result<(), &'static str>;

    /// 轮询接收一帧：没有可收的帧时返回 `Ok(None)`。
    ///
    /// 收到帧时返回帧长（`<= FRAME_MAX`），并把它拷进 `buf`。
    fn receive(&mut self, buf: &mut [u8]) -> Result<Option<usize>, &'static str>;
}

// 注册表：`register` 只写一次（驱动 probe 期间），之后只读。
static mut DEVICES: [Option<&'static mut dyn NetworkDevice>; MAX_NET_DEVICES] = [None, None];

/// 注册一个网络设备，返回它的下标（供 [`get`] 取回）。
///
/// 失败：设备数超过 [`MAX_NET_DEVICES`]。
pub fn register(dev: &'static mut dyn NetworkDevice) -> Result<usize, &'static str> {
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
    Err("network device registry full")
}

/// 已注册的网络设备数量。
pub fn count() -> usize {
    // SAFETY: 只读访问注册表。
    unsafe {
        (&raw const DEVICES)
            .as_ref()
            .map(|table| table.iter().filter(|slot| slot.is_some()).count())
            .unwrap_or(0)
    }
}

/// 取第 `index` 个网络设备。
pub fn get(index: usize) -> Option<&'static mut dyn NetworkDevice> {
    // SAFETY: 注册表在 probe 之后不再变化；调用期间由 `&mut` 保证独占。
    unsafe {
        (&raw mut DEVICES)
            .as_mut()?
            .get_mut(index)?
            .as_mut()
            .map(|dev| &mut **dev)
    }
}

/// 第一个网络设备（v0.9 只有一块网卡）。
pub fn first() -> Option<&'static mut dyn NetworkDevice> {
    get(0)
}

/// 打印设备概况的一行（启动日志与验收断言的来源）。
pub fn report() {
    let mut index = 0;
    while let Some(dev) = get(index) {
        serial::print("net: device=");
        serial::print(dev.name());
        serial::print(" mac=");
        print_mac(dev.mac());
        serial::println("");
        index += 1;
    }
}

/// 按 `aa:bb:cc:dd:ee:ff` 打印一个 MAC 地址。
pub fn print_mac(mac: [u8; 6]) {
    for (i, byte) in mac.iter().enumerate() {
        if i > 0 {
            serial::print(":");
        }
        serial::print_hex_pad(*byte as u64, 2);
    }
}
