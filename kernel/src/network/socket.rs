//! Socket 层（未实现；v0.9 的最小集留到网络自测之后再谈）
//!
//! v0.9 先在**内核内部**验证网络路径（ARP + DHCP 的 UDP 往返，见
//! [`crate::network::selftest`]）。面向用户态的 `socket`/`bind`/`send`/`recv`
//! 需要先有：
//!
//! 1. 一个 fd → socket 的映射（复用 [`crate::fs::file`] 的 fd 表思路）；
//! 2. 用户缓冲校验（与 `syscall::file` 的按页校验同源）;
//! 3. `sendto`/`recvfrom` 的地址结构（`sockaddr_in`）与字节序约定。
//!
//! 这些都属于「最小 Socket syscall 集」这条 v0.9 余项，不阻塞内核侧的网络验收。
// TODO(v0.9+): socket/bind/connect/sendto/recvfrom 最小集
