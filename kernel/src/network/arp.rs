//! ARP（IPv4 over Ethernet，v0.9 实现）
//!
//! 只做 RFC 826 里「以太网 + IPv4」这一种组合（`htype=1`、`ptype=0x0800`、
//! `hlen=6`、`plen=4`），其余组合明确拒绝而不是猜。
//!
//! ```text
//! 偏移  长度  字段
//! 0     2    htype = 1（以太网）
//! 2     2    ptype = 0x0800（IPv4）
//! 4     1    hlen = 6
//! 5     1    plen = 4
//! 6     2    oper（1 = request，2 = reply）
//! 8     6    sender MAC
//! 14    4    sender IP
//! 18    6    target MAC（request 里全 0）
//! 24    4    target IP
//! ```
//!
//! 提供一张固定容量的 **IP → MAC 缓存**（[`Cache`]）：ARP 解析一次即可复用，
//! 缓存满了按「最久未使用」覆盖 —— v0.9 只有网关一个目标，命中率不是问题，
//! 但把「解析一次就记住」这件事写进接口，后面的 UDP 发送路径才不必每次重新 ARP。

use crate::network::ipv4::Address;

/// ARP 报文长度（以太网 + IPv4）。
pub const PACKET_LEN: usize = 28;

/// 操作码：请求。
pub const OPER_REQUEST: u16 = 1;
/// 操作码：应答。
pub const OPER_REPLY: u16 = 2;

/// 解析出的 ARP 报文。
#[derive(Clone, Copy, Debug)]
pub struct ArpPacket {
    /// 操作码（[`OPER_REQUEST`] / [`OPER_REPLY`]）
    pub oper: u16,
    /// 发送方 MAC
    pub sender_mac: [u8; 6],
    /// 发送方 IP
    pub sender_ip: Address,
    /// 目标 MAC
    pub target_mac: [u8; 6],
    /// 目标 IP
    pub target_ip: Address,
}

/// 写一个 ARP 报文（28 字节）。`target_mac` 在 request 里传 [`ZERO_MAC`]。
pub fn build(
    buf: &mut [u8; PACKET_LEN],
    oper: u16,
    sender_mac: [u8; 6],
    sender_ip: Address,
    target_mac: [u8; 6],
    target_ip: Address,
) {
    buf[0..2].copy_from_slice(&1u16.to_be_bytes()); // htype = Ethernet
    buf[2..4].copy_from_slice(&0x0800u16.to_be_bytes()); // ptype = IPv4
    buf[4] = 6; // hlen
    buf[5] = 4; // plen
    buf[6..8].copy_from_slice(&oper.to_be_bytes());
    buf[8..14].copy_from_slice(&sender_mac);
    buf[14..18].copy_from_slice(&sender_ip.octets());
    buf[18..24].copy_from_slice(&target_mac);
    buf[24..28].copy_from_slice(&target_ip.octets());
}

/// 解析一个 ARP 报文；不是「以太网 + IPv4」组合时返回 `None`。
pub fn parse(buf: &[u8]) -> Option<ArpPacket> {
    if buf.len() < PACKET_LEN {
        return None;
    }
    let htype = u16::from_be_bytes([buf[0], buf[1]]);
    let ptype = u16::from_be_bytes([buf[2], buf[3]]);
    if htype != 1 || ptype != 0x0800 || buf[4] != 6 || buf[5] != 4 {
        return None;
    }
    let mut sender_mac = [0u8; 6];
    let mut target_mac = [0u8; 6];
    sender_mac.copy_from_slice(&buf[8..14]);
    target_mac.copy_from_slice(&buf[18..24]);
    Some(ArpPacket {
        oper: u16::from_be_bytes([buf[6], buf[7]]),
        sender_mac,
        sender_ip: Address::from_octets([buf[14], buf[15], buf[16], buf[17]]),
        target_mac,
        target_ip: Address::from_octets([buf[24], buf[25], buf[26], buf[27]]),
    })
}

/// 固定容量的 IP → MAC 缓存（线性探测，容量小到不值得上哈希表）。
pub struct Cache {
    entries: [Option<(Address, [u8; 6])>; Cache::CAPACITY],
    /// 下一个要覆盖的槽位（简单的轮转替换）。
    next: usize,
}

impl Cache {
    /// 缓存容量（v0.9 只解析网关，4 条足够）。
    pub const CAPACITY: usize = 4;

    /// 空缓存（`const` 构造，可直接放静态量里）。
    pub const fn new() -> Self {
        Self {
            entries: [None; Self::CAPACITY],
            next: 0,
        }
    }

    /// 查表。
    pub fn lookup(&self, ip: Address) -> Option<[u8; 6]> {
        self.entries
            .iter()
            .flatten()
            .find(|(cached, _)| *cached == ip)
            .map(|(_, mac)| *mac)
    }

    /// 插入/更新一条。
    pub fn insert(&mut self, ip: Address, mac: [u8; 6]) {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .flatten()
            .find(|(cached, _)| *cached == ip)
        {
            entry.1 = mac;
            return;
        }
        let slot = self.next % Self::CAPACITY;
        self.entries[slot] = Some((ip, mac));
        self.next = (slot + 1) % Self::CAPACITY;
    }

}
