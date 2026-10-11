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
//! v0.7 的初始文件树（由 [`init`] 建立；v0.9 把 `/bin` 从「只放 Shell」扩成
//! 「放全部外部程序」）：
//!
//! ```text
//! /            ramfs
//! ├── init     用户程序 ELF（内核内嵌，由 kernel/build.rs 产出）
//! ├── bin/
//! │   ├── sh   交互式 Shell ELF（v0.9 第一步）
//! │   ├── hello   外部程序：pid + argv（v0.9 第三步）
//! │   └── ls / cat / echo / ps / dynhello   基础与动态链接用户程序（v0.9 第五步/动态链接）
//! ├── lib/     v0.9 动态链接：解释器与共享库（内核按 PT_INTERP/DT_NEEDED 的名字在此查找）
//! │   ├── ld.so          PT_INTERP 的落点
//! │   └── libgreet.so    DT_NEEDED=libgreet.so 的落点
//! ├── etc/
//! │   └── motd 文本文件（用户程序用 open/read 读它）
//! └── tmp/     用户程序写文件的落点
//! ```
//!
//! 为什么外部程序都放在 `/bin` 而不是根目录：`/bin` 是 v0.7 就预留好的落点，
//! Shell 只是其中第一个 —— 目录先建立，后续只添文件（`ls`/`ps` 直接列举它，
//! 见 `make test-programs`）。
//!
//! v0.8 在同一个 trait 下加了第二个后端：[`fat`]（块设备上的 FAT12/16/32，**只读**），
//! 由 [`mount_disks`] 挂到 `/disk`（启动盘）与 `/mnt/fat`（`mkfs` 生成的测试盘）。
//! syscall 层一行都不用改 —— 这正是「VFS 收口」想要的结果。

pub mod fat;
pub mod file;
pub mod inode;
pub mod mount;
pub mod ramfs;
pub mod vfs;

use alloc::boxed::Box;

use crate::driver::block::BlockDevice;
use crate::driver::serial;
use crate::fs::inode::NodeKind;
use crate::fs::vfs::FileSystem;

/// `/etc/motd` 的内容。
///
/// 23 字节，是「用户程序通过 `open`/`read` 读到内核提供的文件」这条链路的
/// 可断言证据（`make test-fs` / `make test-user` 都断言它的字节数）。
pub const MOTD: &[u8] = b"motd: hello from ramfs\n";

/// 挂载根 RAMFS 并建立初始文件树。
///
/// `programs` 是内嵌的用户程序 ELF 清单 `(RAMFS 路径, ELF 字节)`（见
/// `syscall::user::program_images`）—— `/init`、`/bin/sh`、`/bin/hello` 都以
/// **普通文件**落在 RAMFS 上：内核稍后从 VFS 读它们、交给 ELF Loader 装载。
/// v0.7 起用户程序就是「文件系统里的一个文件」，没有任何一条是特例
/// （`/bin/hello` 还是由 Shell `fork` + `execve` 起来的）。
pub fn init(
    programs: &[(&'static str, &'static [u8])],
    libraries: &[(&'static str, &'static [u8])],
) -> Result<(), i64> {
    mount::table().mount("/", Box::new(ramfs::RamFs::new("ramfs")))?;
    serial::print("fs: mount point=/ fs=");
    serial::println(
        vfs::backend_name("/").expect("根挂载点刚建立，必须能路由"),
    );

    // /bin 是外部程序的落点：目录先建立，随后往里放文件
    vfs::mkdir("/bin")?;

    // 用户程序：每个 `(path, elf)` 都走 `create` + `write`（与用户态看到的完全一致）
    for (path, elf) in programs {
        vfs::create(path)?;
        let written = vfs::write(path, 0, elf)?;
        log_write(path, written);
    }

    // v0.9 动态链接：/lib 放解释器与共享库 —— 同样是普通文件，内核按名字查找
    vfs::mkdir("/lib")?;
    for (path, image) in libraries {
        vfs::create(path)?;
        let written = vfs::write(path, 0, image)?;
        log_write(path, written);
    }

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

/// 把块设备上的 FAT 卷挂进 VFS（v0.8）。
///
/// 与 [`init`] 分开的原因：RAMFS 在 `memory::init` 之后就绪，而块设备要等 PCI 枚举 +
/// 驱动挂接（v0.8 启动链的后半段）。因此调用顺序是 `fs::init` → … → `fs::mount_disks`。
///
/// 挂载点约定：
/// - `blk0`（启动盘，固件写的 ESP FAT）→ `/disk`；
/// - `blk1`（`mkfs` 生成的测试盘，只有测试时才挂第二块盘）→ `/mnt/fat`。
///   `/mnt/fat` 挂在 `/mnt`（自测用的 RAMFS）下面，因此顺带验证了挂载表的**最长前缀路由**。
///
/// 返回挂载成功的卷数。
pub fn mount_disks() -> Result<usize, &'static str> {
    if crate::driver::block::count() == 0 {
        return Err("fs: no block device to mount");
    }
    let mut mounted = 0;
    if let Some(dev) = crate::driver::block::get(0) {
        mount_fat(dev, "/disk")?;
        mounted += 1;
    }
    if let Some(dev) = crate::driver::block::get(1) {
        mount_fat(dev, "/mnt/fat")?;
        mounted += 1;
    }
    Ok(mounted)
}

/// 打开一个块设备上的 FAT 卷并挂到 `point`。
fn mount_fat(dev: &'static mut dyn BlockDevice, point: &str) -> Result<(), &'static str> {
    let fs = fat::FatFs::open(dev)?;
    let backend = fs.name();
    let geometry = fs.geometry();
    mount::table()
        .mount(point, Box::new(fs))
        .map_err(|_| "fs: FAT mount failed (duplicate mount point or table full)")?;
    serial::print("fs: mount point=");
    serial::print(point);
    serial::print(" fs=");
    serial::print(backend);
    serial::print(" sectors=");
    serial::print_dec(geometry.total_sectors as u64);
    serial::print(" cluster_bytes=");
    serial::print_dec(geometry.sectors_per_cluster as u64 * geometry.bytes_per_sector as u64);
    serial::print(" clusters=");
    serial::print_dec(geometry.cluster_count as u64);
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
    // 根目录条目数：`init` + `bin` + `lib` + `etc` + `tmp`（v0.9 动态链接加上了 `/lib`）。
    // 这是精确断言而不是「至少有几个」：新增/潦掉的根条目必须同步改这里与
    // `make test-fs` 的期望，否则「文件树悄悄变了」不会被发现。
    if entries.len() != 5 {
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

// ---- v0.8：磁盘（FAT）后端验收 ----

/// 按**大端**读 4 字节（打印文件头魔数用：小端读回会把 `\x7fELF` 变成 `0x464c457f`）。
fn be32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// 按大端读 2 字节（`MZ` / `0xAA55` 这类头部标记用）。
fn be16(bytes: &[u8]) -> u16 {
    u16::from_be_bytes([bytes[0], bytes[1]])
}

/// `dir/nested.bin` 的期望字节（公式与 tests/fatroot/README.md 一致）。
fn nested_pattern(index: usize) -> u8 {
    ((index * 7 + 3) & 0xFF) as u8
}

/// 磁盘文件系统自测（`make test-fat` 的断言来源，v0.8）。
///
/// 断言的都是「磁盘上的事实」：
///
/// 1. 启动盘（**固件写的** FAT）能被解析，且能读到内核**自己的映像** `/disk/KERNEL.ELF`
///    （前 4 字节是 ELF 魔数）与引导程序 `/disk/EFI/BOOT/BOOTX64.EFI`（前 2 字节是 `MZ`）——
///    这条链路是「VFS → FAT → 块设备 → virtio」的一次完整往返，而且文件不是我们放进 RAMFS 的；
/// 2. `mkfs` 生成的测试盘（FAT32）按名字/大小/内容逐项对上：跨簇文件的**内容公式逐字节校验**
///    （3000 字节 = 6 个簇，不是只比长度）、任意偏移读、空文件、长文件名（LFN）、二级子目录；
/// 3. 挂载路由：`/mnt/note.txt` 走 RAMFS 而 `/mnt/fat/*` 走 FAT（最长前缀路由）；
/// 4. 负例：`ENOENT` / `ENOTDIR` / `EISDIR` / `EROFS` 各自具体，不笼统失败。
pub fn disk_selftest() -> Result<(), &'static str> {
    serial::println("fs: disk selftest begin");

    // ---- 1. 启动盘（FAT16/固件写的 ESP）----
    let boot_backend = vfs::backend_name("/disk").ok_or("fs: /disk is not mounted")?;
    if !boot_backend.starts_with("fat") {
        return Err("fs: /disk did not route to a FAT backend");
    }
    let entries = vfs::list("/disk").map_err(|_| "fs: list /disk failed")?;
    serial::print("fs: ls /disk entries=");
    serial::print_dec(entries.len() as u64);
    serial::println("");
    for entry in &entries {
        serial::print("fs: ls /disk name=");
        serial::print(&entry.name);
        serial::print(" kind=");
        serial::print(entry.meta.kind.name());
        serial::print(" size=");
        serial::print_dec(entry.meta.size);
        serial::print(" perms=");
        serial::println(entry.meta.perms());
    }
    let kernel_entry = entries
        .iter()
        .find(|e| e.name.eq_ignore_ascii_case("KERNEL.ELF"))
        .ok_or("fs: KERNEL.ELF not found on the boot volume")?;
    if kernel_entry.meta.kind.is_dir() || kernel_entry.meta.size < 4096 {
        return Err("fs: KERNEL.ELF metadata looks wrong");
    }
    let mut magic = [0u8; 4];
    let read = vfs::read("/disk/KERNEL.ELF", 0, &mut magic).map_err(|_| "fs: read /disk/KERNEL.ELF failed")?;
    if read != 4 || be32(&magic) != 0x7F45_4C46 {
        return Err("fs: /disk/KERNEL.ELF does not start with the ELF magic");
    }
    serial::print("fs: read /disk/KERNEL.ELF magic=");
    serial::print_hex(be32(&magic) as u64);
    serial::print(" size=");
    serial::print_dec(kernel_entry.meta.size);
    serial::print(" (our own kernel image, read from the boot volume)");
    serial::println("");

    let mut pe = [0u8; 2];
    vfs::read("/disk/EFI/BOOT/BOOTX64.EFI", 0, &mut pe)
        .map_err(|_| "fs: read /disk/EFI/BOOT/BOOTX64.EFI failed")?;
    if be16(&pe) != 0x4D5A {
        return Err("fs: BOOTX64.EFI does not start with MZ (not a PE image)");
    }
    serial::print("fs: read /disk/EFI/BOOT/BOOTX64.EFI magic=");
    serial::print_hex(be16(&pe) as u64);
    serial::print(" route=");
    serial::println(vfs::backend_name("/disk/EFI/BOOT/BOOTX64.EFI").unwrap_or("?"));

    // ---- 2. mkfs 生成的测试盘（只有挂了第二块盘才检查）----
    //
    // 判断条件是「`/mnt/fat` 是挂载点」，**不能**用 `backend_name("/mnt/fat").is_some()`：
    // 没有第二块盘时它依然返回 `ramfs-scratch`（最长前缀落到挂在 `/mnt` 的后端），
    // 于是单盘启动会在“检查测试卷”上失败并 halt。
    let second_volume = vfs::is_mount_point("/mnt/fat");
    if second_volume {
        serial::print("fs: second volume at /mnt/fat fs=");
        serial::println(vfs::backend_name("/mnt/fat").unwrap_or("?"));
        check_fixture_volume()?;
    } else {
        serial::println("fs: no second volume (single-disk boot; run `make test-fat` for the full check)");
    }

    // ---- 3. 挂载路由（最长前缀）----
    let note_route = vfs::backend_name("/mnt/note.txt").ok_or("fs: /mnt/note.txt is not routed")?;
    let fat_route = if second_volume {
        vfs::backend_name("/mnt/fat/hello.txt").ok_or("fs: /mnt/fat/hello.txt is not routed")?
    } else {
        "-"
    };
    serial::print("fs: route /mnt/note.txt fs=");
    serial::print(note_route);
    serial::print(" /mnt/fat/hello.txt fs=");
    serial::println(fat_route);

    // ---- 4. 负例：只读后端 + 路径错误各自具体 ----
    if vfs::stat("/disk/nope") != Err(vfs::ENOENT) {
        return Err("fs: expected ENOENT for /disk/nope");
    }
    if vfs::read_all("/disk/KERNEL.ELF/deeper") != Err(vfs::ENOTDIR) {
        return Err("fs: expected ENOTDIR for a path under a file");
    }
    if vfs::read_all("/disk/EFI") != Err(vfs::EISDIR) {
        return Err("fs: expected EISDIR for /disk/EFI");
    }
    if vfs::create("/disk/new.txt") != Err(vfs::EROFS) {
        return Err("fs: expected EROFS when creating a file on the FAT backend");
    }
    if vfs::write("/disk/KERNEL.ELF", 0, b"x") != Err(vfs::EROFS) {
        return Err("fs: expected EROFS when writing to the FAT backend");
    }
    serial::println("fs: disk negative cases PASS (ENOENT/ENOTDIR/EISDIR/EROFS)");

    serial::println("fs: disk selftest PASS");
    Ok(())
}

/// 检查 `mkfs` 生成的测试卷（挂在 `/mnt/fat`）：文件名、大小、内容公式、偏移读。
fn check_fixture_volume() -> Result<(), &'static str> {
    let entries = vfs::list("/mnt/fat").map_err(|_| "fs: list /mnt/fat failed")?;
    serial::print("fs: ls /mnt/fat entries=");
    serial::print_dec(entries.len() as u64);
    serial::println("");
    for entry in &entries {
        serial::print("fs: ls /mnt/fat name=");
        serial::print(&entry.name);
        serial::print(" kind=");
        serial::print(entry.meta.kind.name());
        serial::print(" size=");
        serial::print_dec(entry.meta.size);
        serial::println("");
    }

    let hello = vfs::read_all("/mnt/fat/hello.txt").map_err(|_| "fs: read /mnt/fat/hello.txt failed")?;
    if hello != b"hello from FAT32\n" {
        return Err("fs: /mnt/fat/hello.txt content mismatch");
    }
    serial::print("fs: read /mnt/fat/hello.txt len=");
    serial::print_dec(hello.len() as u64);
    serial::print(" data=");
    serial::put_bytes(&hello, hello.len());
    serial::println("");

    // 跨簇文件：逐字节对公式（3000 字节 / 512 = 6 个簇）
    let nested = vfs::read_all("/mnt/fat/dir/nested.bin").map_err(|_| "fs: read nested.bin failed")?;
    if nested.len() != 3000 {
        return Err("fs: /mnt/fat/dir/nested.bin length mismatch");
    }
    let mut mismatch = None;
    for (index, byte) in nested.iter().enumerate() {
        if *byte != nested_pattern(index) {
            mismatch = Some(index);
            break;
        }
    }
    if let Some(index) = mismatch {
        serial::print("fs: nested.bin mismatch at offset ");
        serial::print_dec(index as u64);
        serial::println("");
        return Err("fs: /mnt/fat/dir/nested.bin content mismatch (cluster chain)");
    }
    serial::print("fs: content check PASS /mnt/fat/dir/nested.bin len=");
    serial::print_dec(nested.len() as u64);
    serial::println(" (formula verified byte by byte across 6 clusters)");

    // 任意偏移读（fd 的 read 路径就是这种调用形态）
    let mut window = [0u8; 8];
    let got = vfs::read("/mnt/fat/dir/nested.bin", 1024, &mut window)
        .map_err(|_| "fs: offset read failed")?;
    if got != 8 {
        return Err("fs: offset read returned the wrong length");
    }
    for (i, byte) in window.iter().enumerate() {
        if *byte != nested_pattern(1024 + i) {
            return Err("fs: offset read content mismatch");
        }
    }
    serial::print("fs: offset read PASS /mnt/fat/dir/nested.bin offset=1024 len=8 data=");
    serial::print_hex(window[0] as u64);
    serial::print(" ");
    serial::println(if window[7] == nested_pattern(1031) { "(matches the formula)" } else { "(MISMATCH)" });

    // 空文件：首簇 0，读它必须返回 0 字节（而不是去看簇 0）
    let empty = vfs::read_all("/mnt/fat/empty.txt").map_err(|_| "fs: read empty.txt failed")?;
    if !empty.is_empty() {
        return Err("fs: /mnt/fat/empty.txt is not empty");
    }
    serial::println("fs: empty file PASS /mnt/fat/empty.txt len=0");

    // 长文件名（LFN）+ 二级子目录
    let long = vfs::read_all("/mnt/fat/a long file name.txt")
        .map_err(|_| "fs: read the long-named file failed (LFN)")?;
    if long != b"long name ok\n" {
        return Err("fs: long-named file content mismatch");
    }
    serial::println("fs: long file name PASS /mnt/fat/a long file name.txt (LFN entry)");

    let deep = vfs::read_all("/mnt/fat/dir/deep/marker.txt")
        .map_err(|_| "fs: read /mnt/fat/dir/deep/marker.txt failed")?;
    if deep != b"deep\n" {
        return Err("fs: /mnt/fat/dir/deep/marker.txt content mismatch");
    }
    serial::println("fs: nested directory PASS /mnt/fat/dir/deep/marker.txt");

    Ok(())
}
