//! TCP（未实现；v0.9 的「简化网络」不含 TCP）
//!
//! v0.9 的网络目标只是打通 **Ethernet → ARP → IPv4 → UDP** 这条最小闭环
//!（见 [`crate::network`] 与 [`crate::network::udp`]），TCP 仍是占位：
//! `docs/architecture.md` 第 5 节把「完整 TCP/IP 栈」列为第一版非目标。
//!
//! 真要做时，动手顺序是：三次握手（SYN / SYN-ACK / ACK）→ 序号与确认号 → 一个
//! 固定的接收窗口 → 简单的重传定时器。它建立在现有的 IPv4 收发路径上，不需要
//! 改动 [`crate::network::ethernet`] / [`crate::network::ipv4`]。
// TODO(v0.9+): 三次握手与简单可靠传输
