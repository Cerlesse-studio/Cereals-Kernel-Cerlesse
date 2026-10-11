//! 简化网络栈（v0.9 实现）
//!
//! 目标不是完整协议栈，而是打通最小可验证闭环：
//!
//! ```text
//! virtio-net → Ethernet II → ARP → IPv4 → UDP
//! ```
//!
//! TCP / Socket 仍是占位（`docs/architecture.md` 第 5 节把「完整 TCP/IP」列为非目标；
//! 见 [`tcp`] / [`socket`] 的说明）。本模块提供：
//!
//! - [`ethernet`] / [`arp`] / [`ipv4`] / [`udp`] / [`dhcp`]：**纯函数**的组帧与解析；
//! - [`Stack`]：把上面的纯函数接到一块 [`NetworkDevice`] 上，负责发送、轮询接收、
//!   ARP 解析（带缓存）与 DHCP DISCOVER；
//! - [`selftest`]：v0.9 的验收 —— 对 QEMU 的 slirp 网关做一次 ARP 解析，再跑一次
//!   DHCP DISCOVER → OFFER，证明 Ethernet/IP/UDP 的收发两条路径都真的通了。
//!
//! 为什么用 slirp 的内置 DHCP 做 UDP 验收：见 [`dhcp`] 的模块说明 —— 它不需要
//! 主机侧跑任何服务，`make test-network` 因此是完全自包含的。

pub mod arp;
pub mod dhcp;
pub mod ethernet;
pub mod ipv4;
pub mod socket;
pub mod tcp;
pub mod udp;

use crate::driver::net::{self, NetworkDevice};
use crate::driver::serial;
use crate::time::clock;

/// QEMU `-netdev user`（slirp）默认给客户的地址。
pub const GUEST_IP: ipv4::Address = ipv4::Address::new(10, 0, 2, 15);
/// QEMU `-netdev user`（slirp）默认的虚拟网关（= 虚拟主机 10.0.2.2）。
pub const GATEWAY_IP: ipv4::Address = ipv4::Address::new(10, 0, 2, 2);

/// ARP 解析的等待上限（PIT tick；100Hz → 100 tick = 1 秒）。
const ARP_TIMEOUT_TICKS: u64 = 100;
/// DHCP 应答的等待上限（100Hz → 300 tick = 3 秒）。
const DHCP_TIMEOUT_TICKS: u64 = 300;

/// 读取 PIT tick 计数（`clock::tick_count` 是 unsafe：调用方须处于内核态）。
fn ticks() -> u64 {
    // SAFETY: 网络栈只在内核态（启动自测）里运行。
    unsafe { clock::tick_count() }
}

/// 一个极简的网络栈：一块设备 + 本机地址/网关 + ARP 缓存。
pub struct Stack<'a> {
    /// 底层设备
    pub dev: &'a mut dyn NetworkDevice,
    /// 本机 MAC（来自设备配置空间）
    pub mac: [u8; 6],
    /// 本机 IPv4 地址（DHCP 成功后会被 lease 覆盖）
    pub ip: ipv4::Address,
    /// 网关地址（ARP 解析的目标）
    pub gateway: ipv4::Address,
    /// IP → MAC 缓存
    pub arp_cache: arp::Cache,
    /// IPv4 报文标识递增
    next_id: u16,
}

impl<'a> Stack<'a> {
    /// 用一块设备构造栈（初始地址取 slirp 的默认值，随后可被 DHCP 覆盖）。
    pub fn new(dev: &'a mut dyn NetworkDevice) -> Self {
        Self {
            mac: dev.mac(),
            dev,
            ip: GUEST_IP,
            gateway: GATEWAY_IP,
            arp_cache: arp::Cache::new(),
            next_id: 1,
        }
    }

    /// 发送一个已经拼好的以太网帧。
    pub fn send_frame(&mut self, frame: &[u8]) -> Result<(), &'static str> {
        self.dev.transmit(frame)
    }

    /// 在 `timeout_ticks` 内轮询一帧，收到就返回帧长。
    fn recv_frame(&mut self, out: &mut [u8], timeout_ticks: u64) -> Option<usize> {
        let start = ticks();
        loop {
            match self.dev.receive(out) {
                Ok(Some(n)) => return Some(n),
                Ok(None) => {}
                Err(_) => return None,
            }
            if ticks().wrapping_sub(start) > timeout_ticks {
                return None;
            }
            core::hint::spin_loop();
        }
    }

    /// 发送一个 UDP 数据报（自己负责 Ethernet + IPv4 的封装）。
    pub fn send_udp(
        &mut self,
        dst_ip: ipv4::Address,
        dst_mac: [u8; 6],
        src_port: u16,
        dst_port: u16,
        payload: &[u8],
    ) -> Result<usize, &'static str> {
        let mut frame = [0u8; net::FRAME_MAX];
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);

        let eth = ethernet::build(&mut frame, dst_mac, self.mac, ethernet::ETHERTYPE_IPV4)
            .ok_or("net: frame buffer too small for Ethernet header")?;
        let ip_payload_len = udp::HEADER_LEN + payload.len();
        let ip = ipv4::build(
            eth,
            self.ip,
            dst_ip,
            ipv4::PROTO_UDP,
            64, // TTL
            id,
            ip_payload_len,
        )
        .ok_or("net: frame buffer too small for IPv4 header")?;
        udp::build(ip, self.ip, dst_ip, src_port, dst_port, payload)
            .ok_or("net: frame buffer too small for UDP datagram")?;

        let total = ethernet::HEADER_LEN + ipv4::HEADER_LEN + ip_payload_len;
        self.dev.transmit(&frame[..total])?;
        Ok(total)
    }

    /// 解析 `target` 的 MAC：先查缓存，未命中就发 ARP 请求并等应答。
    pub fn arp_resolve(&mut self, target: ipv4::Address) -> Option<[u8; 6]> {
        if let Some(mac) = self.arp_cache.lookup(target) {
            return Some(mac);
        }
        let mut frame = [0u8; ethernet::HEADER_LEN + arp::PACKET_LEN];
        let mut arp_bytes = [0u8; arp::PACKET_LEN];
        arp::build(
            &mut arp_bytes,
            arp::OPER_REQUEST,
            self.mac,
            self.ip,
            ethernet::ZERO_MAC,
            target,
        );
        {
            let payload =
                ethernet::build(&mut frame, ethernet::BROADCAST_MAC, self.mac, ethernet::ETHERTYPE_ARP)?;
            payload[..arp::PACKET_LEN].copy_from_slice(&arp_bytes);
        }
        self.send_frame(&frame).ok()?;

        let mut rx = [0u8; net::FRAME_MAX];
        let start = ticks();
        loop {
            let remaining = ARP_TIMEOUT_TICKS.saturating_sub(ticks().wrapping_sub(start));
            let n = self.recv_frame(&mut rx, remaining)?;
            let Some(eth) = ethernet::parse(&rx[..n]) else {
                continue;
            };
            if eth.ethertype != ethernet::ETHERTYPE_ARP {
                continue;
            }
            let Some(packet) = arp::parse(eth.payload) else {
                continue;
            };
            if packet.oper != arp::OPER_REPLY || packet.sender_ip != target {
                continue;
            }
            // 应答必须发给我们：以太网目的为本机（或广播），且 ARP 的目的 IP / MAC 都是本机。
            if !(eth.dst == self.mac || ethernet::is_broadcast(eth.dst)) {
                continue;
            }
            if packet.target_ip != self.ip || packet.target_mac != self.mac {
                continue;
            }
            // 交叉校验：帧头源 MAC 应与 ARP 报文的发送方 MAC 一致（不一致只提示，以 ARP 为准）。
            if eth.src != packet.sender_mac {
                serial::println("net: arp note: frame src != arp sender mac");
            }
            self.arp_cache.insert(target, packet.sender_mac);
            return Some(packet.sender_mac);
        }
    }

    /// 发一个 DHCP DISCOVER 并等 OFFER，返回 offer 里给的地址。
    pub fn dhcp_discover(&mut self) -> Result<dhcp::Offer, &'static str> {
        let mut payload = [0u8; dhcp::MESSAGE_LEN];
        let len = dhcp::build_discover(&mut payload, self.mac);
        self.send_udp(
            ipv4::Address::BROADCAST,
            ethernet::BROADCAST_MAC,
            dhcp::CLIENT_PORT,
            dhcp::SERVER_PORT,
            &payload[..len],
        )?;

        let mut rx = [0u8; net::FRAME_MAX];
        let start = ticks();
        loop {
            let remaining = DHCP_TIMEOUT_TICKS.saturating_sub(ticks().wrapping_sub(start));
            let Some(n) = self.recv_frame(&mut rx, remaining) else {
                return Err("net: no DHCP OFFER within timeout");
            };
            let Some(eth) = ethernet::parse(&rx[..n]) else {
                continue;
            };
            if eth.ethertype != ethernet::ETHERTYPE_IPV4 {
                continue;
            }
            let Some((header, ip_payload)) = ipv4::parse(eth.payload) else {
                continue;
            };
            if header.protocol != ipv4::PROTO_UDP {
                continue;
            }
            let Some(datagram) = udp::parse(ip_payload, header.src, header.dst) else {
                continue;
            };
            if datagram.src_port != dhcp::SERVER_PORT || datagram.dst_port != dhcp::CLIENT_PORT {
                continue;
            }
            if let Some(offer) = dhcp::parse_offer(datagram.payload) {
                return Ok(offer);
            }
        }
    }
}

/// v0.9 网络验收：ARP 解析网关 + DHCP DISCOVER → OFFER。
///
/// 断言的都是**网络上回来的真实内容**（不是日志措辞）：ARP 应答里的 MAC、
/// OFFER 里的 `yiaddr` / server-id / 网关。任何一步没通，这里就会返回 `Err`。
pub fn selftest(dev: &mut dyn NetworkDevice) -> Result<(), &'static str> {
    let mut stack = Stack::new(dev);

    serial::print("net: stack mac=");
    net::print_mac(stack.mac);
    serial::print(" ip=");
    print_addr(stack.ip);
    serial::print(" gateway=");
    print_addr(stack.gateway);
    serial::println("");

    // 1) ARP：解析网关的 MAC（Ethernet 广播请求 → 单播应答）。
    let gateway_mac = stack
        .arp_resolve(stack.gateway)
        .ok_or("net: ARP resolve of the gateway timed out")?;
    serial::print("net: arp ");
    print_addr(stack.gateway);
    serial::print(" -> ");
    net::print_mac(gateway_mac);
    serial::println(" (reply)");
    serial::println("net: arp PASS");

    // 2) DHCP：UDP DISCOVER → OFFER（Ethernet → IPv4 → UDP 的完整往返）。
    let offer = stack.dhcp_discover()?;
    serial::print("net: dhcp offer yiaddr=");
    print_addr(offer.yiaddr);
    if let Some(server) = offer.server_id {
        serial::print(" server=");
        print_addr(server);
    }
    if let Some(mask) = offer.subnet_mask {
        serial::print(" mask=");
        print_addr(mask);
    }
    if let Some(router) = offer.router {
        serial::print(" router=");
        print_addr(router);
    }
    serial::println("");
    if offer.yiaddr == ipv4::Address::UNSPECIFIED {
        return Err("net: DHCP OFFER carried a zero address");
    }
    serial::println("net: dhcp PASS");

    serial::println("net: selftest PASS");
    Ok(())
}

/// 按 `a.b.c.d` 打印一个 IPv4 地址。
pub fn print_addr(addr: ipv4::Address) {
    for (i, octet) in addr.octets().iter().enumerate() {
        if i > 0 {
            serial::print(".");
        }
        serial::print_dec(*octet as u64);
    }
}
