//! 以太网帧（Ethernet II，v0.9 实现）
//!
//! ```text
//! 偏移  长度  字段
//! 0     6    目的 MAC
//! 6     6    源 MAC
//! 12    2    EtherType（大端）
//! 14    ...  payload
//! ```
//!
//! 只做 **Ethernet II**（把握手/长度字段当 EtherType 用）：v0.9 不碰 802.3 LLC/SNAP。
//! 帧尾的 4 字节 FCS 由网卡收发，不进驱动与上层（[`crate::driver::net`] 的约定）。
//!
//! 这里只有**纯函数**（`build` / `parse`）：不知道设备、不分配内存，因此可以在
//! host 侧或内核里被单独验证。

/// 以太网帧头长度（不含 FCS）。
pub const HEADER_LEN: usize = 14;

/// EtherType：IPv4。
pub const ETHERTYPE_IPV4: u16 = 0x0800;
/// EtherType：ARP。
pub const ETHERTYPE_ARP: u16 = 0x0806;

/// 广播 MAC。
pub const BROADCAST_MAC: [u8; 6] = [0xFF; 6];
/// 全零 MAC（DHCP 请求里代表「未知」）。
pub const ZERO_MAC: [u8; 6] = [0x00; 6];

/// 解析出的一帧：目的/源 MAC、EtherType 与 payload 切片。
#[derive(Clone, Copy, Debug)]
pub struct EthernetFrame<'a> {
    /// 目的 MAC
    pub dst: [u8; 6],
    /// 源 MAC
    pub src: [u8; 6],
    /// 上层协议类型（大端读入的主机序）
    pub ethertype: u16,
    /// 上层 payload（不含帧头与 FCS）
    pub payload: &'a [u8],
}

/// 在 `buf` 开头写一个以太网头，返回 payload 的可写切片。
///
/// 返回 `None`：`buf` 比帧头还短（调用方按 [`HEADER_LEN`] 预检即可，这里兜底）。
pub fn build(
    buf: &mut [u8],
    dst: [u8; 6],
    src: [u8; 6],
    ethertype: u16,
) -> Option<&mut [u8]> {
    if buf.len() < HEADER_LEN {
        return None;
    }
    buf[0..6].copy_from_slice(&dst);
    buf[6..12].copy_from_slice(&src);
    buf[12..14].copy_from_slice(&ethertype.to_be_bytes());
    Some(&mut buf[HEADER_LEN..])
}

/// 解析一帧。帧长不足一个帧头时返回 `None`。
pub fn parse(frame: &[u8]) -> Option<EthernetFrame<'_>> {
    if frame.len() < HEADER_LEN {
        return None;
    }
    let mut dst = [0u8; 6];
    let mut src = [0u8; 6];
    dst.copy_from_slice(&frame[0..6]);
    src.copy_from_slice(&frame[6..12]);
    let ethertype = u16::from_be_bytes([frame[12], frame[13]]);
    Some(EthernetFrame {
        dst,
        src,
        ethertype,
        payload: &frame[HEADER_LEN..],
    })
}

/// 目的 MAC 是否为广播地址。
pub fn is_broadcast(dst: [u8; 6]) -> bool {
    dst == BROADCAST_MAC
}
