//! 文件系统子系统（v0.7 实现：VFS + RAMFS + fd 表）
//!
//! 组成：
//!
//! | 模块 | 职责 |
//! | ---- | ---- |
//! | [`inode`] | 节点类型与元数据（`NodeKind` / `Metadata` / `DirEntry`） |
//! | [`vfs`] | `FileSystem` trait、路径工具、系统级入口（经挂载表路由） |
//! | [`mount`] | 挂载表（挂载点 → 后端，最长前缀路由） |
//! | [`ramfs`] | 内存文件系统（第一个后端，挂在 `/`） |
//! | [`file`] | 进程级 fd 表（`open/read/write/close` 的落点） |
//!
//! 初始化顺序：RAMFS 用内核堆保存文件内容，因此 [`init`] 必须在
//! `memory::init`（帧分配器 + 内核堆就绪）之后调用，见 `main.rs`。
//!
//! v0.7 的初始文件树（由 [`init`] 建立）：
//!
//! ```text
//! /            ramfs
//! ├── init     用户程序 ELF（内核内嵌，由 kernel/build.rs 产出）
//! ├── etc/
//! │   └── motd 文本文件（用户程序用 open/read 读它）
//! └── tmp/     用户程序写文件的落点
//! ```
//!
//! 磁盘文件系统（FAT32 on virtio-blk）是 v0.8 的工作：届时 [`vfs::FileSystem`]
//! 已经稳定，加一个后端即可，syscall 层不需要改动。

pub mod file;
pub mod inode;
pub mod mount;
pub mod ramfs;
pub mod vfs;


use alloc::boxed::Box;

use crate::driver::serial;
use crate::fs::inode::NodeKind;

/// `/etc/motd` 的内容。
///
/// 23 字节，是「用户程序通过 `open`/`read` 读到内核提供的文件」这条链路的
/// 可断言证据（`make test-fs` / `make test-user` 都断言它的字节数）。
pub const MOTD: &[u8] = b"motd: hello from ramfs\n";

/// 挂载根 RAMFS 并建立初始文件树。
///
/// `init_elf` 是内嵌的用户程序 ELF（见 `syscall::user::init_image`），
/// 以 `/init` 落在 RAMFS 上 —— 内核稍后从 VFS 读它、交给 ELF Loader 装载，
/// 这就是 v0.7「用户程序从文件加载」的完整路径（v0.6 是构建期摊平 + 直接拷贝）。
pub fn init(init_elf: &'static [u8]) -> Result<(), i64> {
    mount::table().mount("/", Box::new(ramfs::RamFs::new("ramfs")))?;
    serial::print("fs: mount point=/ fs=");
    serial::println(
        vfs::backend_name("/").expect("根挂载点刚建立，必须能路由"),
    );

    // /init：用户程序（内容来自内核映像里内嵌的 ELF）
    vfs::create("/init")?;
    let written = vfs::write("/init", 0, init_elf)?;
    log_write("/init", written);

    // /etc/motd：用户程序读的对象
    vfs::mkdir("/etc")?;
    vfs::create("/etc/motd")?;
    let written = vfs::write("/etc/motd", 0, MOTD)?;
    log_write("/etc/motd", written);

    // /tmp：用户程序写文件的落点
    vfs::mkdir("/tmp")?;

    let (files, bytes) = vfs::usage("/").expect("根挂载点刚建立，必须能取到统计");
    serial::print("fs: ramfs files=");
    serial::print_dec(files as u64);
    serial::print(" bytes=");
    serial::print_dec(bytes as u64);
    serial::println("");
    Ok(())
}

/// `fs: write <path> bytes=<n>`。
fn log_write(path: &str, bytes: usize) {
    serial::print("fs: write ");
    serial::print(path);
    serial::print(" bytes=");
    serial::print_dec(bytes as u64);
    serial::println("");
}

/// VFS / RAMFS 自测（`make test-fs` 的断言来源）。
///
/// 覆盖四类「错了必然看得见」的行为：
///
/// 1. 元数据与内容读回（`stat` / `read_all`）；
/// 2. **挂载点路由**：把第二个 RAMFS 挂到 `/mnt`，`/mnt/*` 必须落到它而不是根后端；
/// 3. 目录列举（`ls` 语义：名字 + 类型 + 大小 + 权限串）；
/// 4. 反例：`ENOENT` / `ENOTDIR` / `EISDIR` / `EEXIST` 各自返回对应 errno。
pub fn selftest() -> Result<(), &'static str> {
    serial::println("fs: selftest begin");

    // 1. 元数据：/etc/motd 是 23 字节的文件
    let meta = vfs::stat("/etc/motd").map_err(|_| "fs: stat /etc/motd failed")?;
    if meta.kind != NodeKind::File || meta.size != MOTD.len() as u64 {
        return Err("fs: /etc/motd metadata mismatch");
    }
    serial::print("fs: stat /etc/motd kind=");
    serial::print(meta.kind.name());
    serial::print(" size=");
    serial::print_dec(meta.size);
    serial::print(" perms=");
    serial::println(meta.perms());

    // 2. 内容读回（这条链路的终点是用户态 open/read，见 user/hello）
    let data = vfs::read_all("/etc/motd").map_err(|_| "fs: read /etc/motd failed")?;
    if data != MOTD {
        return Err("fs: /etc/motd content mismatch");
    }
    serial::print("fs: read /etc/motd len=");
    serial::print_dec(data.len() as u64);
    serial::print(" data=");
    serial::put_bytes(&data, data.len());
    serial::println("");

    // 3. 挂载点路由：/mnt 是另一个 RAMFS 实例
    const SCRATCH_NAME: &str = "ramfs-scratch";
    mount::table()
        .mount("/mnt", Box::new(ramfs::RamFs::new(SCRATCH_NAME)))
        .map_err(|_| "fs: mount /mnt failed")?;
    serial::print("fs: mount point=/mnt fs=");
    serial::println(SCRATCH_NAME);

    vfs::create("/mnt/note.txt").map_err(|_| "fs: create /mnt/note.txt failed")?;
    vfs::write("/mnt/note.txt", 0, b"v0.7-ok").map_err(|_| "fs: write /mnt/note.txt failed")?;

    // 路由证据：解析 /mnt/note.txt 得到的后端必须是 ramfs-scratch（不是 ramfs）
    let routed = vfs::backend_name("/mnt/note.txt").ok_or("fs: route lookup failed")?;
    if routed != SCRATCH_NAME {
        return Err("fs: /mnt/note.txt did not route to /mnt backend");
    }
    serial::print("fs: route /mnt/note.txt fs=");
    serial::println(routed);

    let note = vfs::read_all("/mnt/note.txt").map_err(|_| "fs: read /mnt/note.txt failed")?;
    serial::print("fs: readback /mnt/note.txt len=");
    serial::print_dec(note.len() as u64);
    serial::print(" data=");
    serial::put_bytes(&note, note.len());
    serial::println("");

    // 4. 目录列举（ls 语义；顺序 = 创建顺序）
    let entries = vfs::list("/").map_err(|_| "fs: list / failed")?;
    serial::print("fs: ls / entries=");
    serial::print_dec(entries.len() as u64);
    serial::println("");
    for entry in &entries {
        serial::print("fs: ls / name=");
        serial::print(&entry.name);
        serial::print(" kind=");
        serial::print(entry.meta.kind.name());
        serial::print(" size=");
        serial::print_dec(entry.meta.size);
        serial::print(" perms=");
        serial::println(entry.meta.perms());
    }
    if entries.len() != 3 {
        return Err("fs: root directory entry count mismatch");
    }

    // 5. 反例：errno 必须是具体的，不能笼统地失败
    if vfs::stat("/nope") != Err(vfs::ENOENT) {
        return Err("fs: expected ENOENT for /nope");
    }
    if vfs::read_all("/etc/motd/deeper") != Err(vfs::ENOTDIR) {
        return Err("fs: expected ENOTDIR for /etc/motd/deeper");
    }
    if vfs::read_all("/etc") != Err(vfs::EISDIR) {
        return Err("fs: expected EISDIR for /etc");
    }
    if vfs::create("/etc") != Err(vfs::EEXIST) {
        return Err("fs: expected EEXIST for /etc");
    }
    if vfs::stat("..") != Err(vfs::EINVAL) {
        return Err("fs: expected EINVAL for ..");
    }
    serial::println("fs: negative cases PASS (ENOENT/ENOTDIR/EISDIR/EEXIST/EINVAL)");

    serial::println("fs: selftest PASS");
    Ok(())
}
