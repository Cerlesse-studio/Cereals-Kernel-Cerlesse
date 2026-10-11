//! UDP（v0.9 实现）
//!
//! 无连接数据报：8 字节头 + 载荷。发送时计算校验和（含 IPv4 伪头）；
//! 接收时校验非零校验和（RFC 768 允许发送方置 0 表示「不计算」，此时不校验）。
//!
//! ```text
//! 偏移  长度  字段
//! 0     2    源端口
//! 2     2    目的端口
//! 4     2    长度（头 + 载荷，含自身）
//! 6     2    校验和（0 = 未计算）
//! ```

use crate::network::ipv4::{self, Address};

/// UDP 头长度。
pub const HEADER_LEN: usize = 8;

/// 解析出的 UDP 数据报。
#[derive(Clone, Copy, Debug)]
pub struct Datagram<'a> {
    /// 源端口
    pub src_port: u16,
    /// 目的端口
    pub dst_port: u16,
    /// 载荷（不含 UDP 头）
    pub payload: &'a [u8],
}

/// 写一个 UDP 数据报（头 + 载荷），返回总字节数（[`HEADER_LEN`] + `payload.len()`）。
///
/// 校验和需要 IPv4 伪头（源/目的地址），因此这里显式收 `src`/`dst` ——
/// 不把地址藏进设备状态，函数保持可直接验证。
pub fn build(
    buf: &mut [u8],
    src: Address,
    dst: Address,
    src_port: u16,
    dst_port: u16,
    payload: &[u8],
) -> Option<usize> {
    let total = HEADER_LEN.checked_add(payload.len())?;
    if total > u16::MAX as usize || buf.len() < total {
        return None;
    }
    buf[0..2].copy_from_slice(&src_port.to_be_bytes());
    buf[2..4].copy_from_slice(&dst_port.to_be_bytes());
    buf[4..6].copy_from_slice(&(total as u16).to_be_bytes());
    buf[6..8].copy_from_slice(&0u16.to_be_bytes()); // 校验和占位
    buf[HEADER_LEN..total].copy_from_slice(payload);

    // 伪头：源 IP(4) + 目的 IP(4) + 零(1) + 协议(1) + UDP 长度(2)
    let mut pseudo = [0u8; 12];
    pseudo[0..4].copy_from_slice(&src.octets());
    pseudo[4..8].copy_from_slice(&dst.octets());
    pseudo[8] = 0;
    pseudo[9] = ipv4::PROTO_UDP;
    pseudo[10..12].copy_from_slice(&(total as u16).to_be_bytes());

    let sum = ipv4::sum16(0, &pseudo);
    let sum = ipv4::sum16(sum, &buf[..total]);
    let mut csum = ipv4::finish(sum);
    if csum == 0 {
        // RFC 768：校验结果为 0 时用 0xFFFF 发送（0 表示「未计算」）
        csum = 0xFFFF;
    }
    buf[6..8].copy_from_slice(&csum.to_be_bytes());
    Some(total)
}

/// 解析一个 UDP 数据报（校验长度与（非零）校验和）。
pub fn parse(buf: &[u8], src: Address, dst: Address) -> Option<Datagram<'_>> {
    if buf.len() < HEADER_LEN {
        return None;
    }
    let length = u16::from_be_bytes([buf[4], buf[5]]) as usize;
    if length < HEADER_LEN || length > buf.len() {
        return None;
    }
    let checksum = u16::from_be_bytes([buf[6], buf[7]]);
    if checksum != 0 {
        let mut pseudo = [0u8; 12];
        pseudo[0..4].copy_from_slice(&src.octets());
        pseudo[4..8].copy_from_slice(&dst.octets());
        pseudo[8] = 0;
        pseudo[9] = ipv4::PROTO_UDP;
        pseudo[10..12].copy_from_slice(&(length as u16).to_be_bytes());
        let sum = ipv4::sum16(0, &pseudo);
        let sum = ipv4::sum16(sum, &buf[..length]);
        if ipv4::finish(sum) != 0 {
            return None;
        }
    }
    Some(Datagram {
        src_port: u16::from_be_bytes([buf[0], buf[1]]),
        dst_port: u16::from_be_bytes([buf[2], buf[3]]),
        payload: &buf[HEADER_LEN..length],
    })
}
