# 文件系统（阶段文档）

- 阶段：第六阶段（v0.7）
- 最后更新：2026-10-11
- 状态：🟡 v0.7 核心已实现并验收（VFS + RAMFS + 挂载点路由 + fd 表 → `open`/`read`/`write`/`close`，
  `make test-fs` 19 项串口断言 PASS）；磁盘后端（FAT32 / virtio-blk）与 `mkfs`（H-03）属 v0.8

> 本文档顶部保留阶段/最后更新/状态字段，与项目文档维护约定对齐。
- 依赖：syscall.md（open/read/write/close）、memory.md（内核堆：RAMFS 的数据就在堆上）

## 1. 概述

v0.7 文件系统阶段分两步，本版只做第一步：

1. **VFS 抽象 + RAMFS 后端**（本版完成）：把「文件」这件事的语义先立起来，
   并让 syscall 层的文件语义有真实落点；
2. **磁盘后端**（v0.8）：FAT32 + 块设备挂到同一套 VFS 上，并补 `mkfs`（H-03）。

非目标重申：不把具体文件系统写死进内核（ADR-004），`FileSystem` trait 是唯一入口；
`syscall` 层只认路径与 fd，不认「RAMFS 还是 FAT32」。

## 2. v0.7 实际实现（2026-10-11 落地）

### 2.1 模块划分（`kernel/src/fs/`）

| 模块 | 职责 | 关键类型/函数 |
| ---- | ---- | ---- |
| `inode.rs` | 与后端无关的节点形状 | `NodeKind`（File/Dir）、`Metadata`（kind/size/`perms()`）、`DirEntry` |
| `vfs.rs` | 抽象 + 路径工具 + 系统级入口 | `trait FileSystem`、`normalize/components/within`、`stat/read_all/read/write/create/mkdir/list/usage/backend_name` |
| `mount.rs` | 挂载表（**最长前缀路由**） | `MountTable::mount/resolve`、全局 `mount::table()` |
| `ramfs.rs` | 第一个后端（内存 FS） | `RamFs { root: Node }`、`Node { name, kind, data: Vec<u8>, children: Vec<Node> }` |
| `file.rs` | fd 表（`open` 的落点） | `FdTable`、`open/read/write/close/reset`、`O_RDONLY/O_WRONLY/O_RDWR/O_CREAT` |

`FileSystem` trait（v0.7 的全部语义）：

```rust
pub trait FileSystem {
    fn name(&self) -> &'static str;
    fn stat(&self, path: &str) -> Result<Metadata, i64>;
    fn read<'a>(&'a self, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, i64>;
    fn read_all<'a>(&'a self, path: &str) -> Result<&'a [u8], i64>;
    fn write(&mut self, path: &str, offset: u64, data: &[u8]) -> Result<usize, i64>;
    fn create(&mut self, path: &str) -> Result<(), i64>;
    fn mkdir(&mut self, path: &str) -> Result<(), i64>;
    fn list(&self, path: &str) -> Result<Vec<DirEntry>, i64>;
    fn usage(&self) -> (usize, usize) { (0, 0) } // 默认实现：磁盘后端不适用
}
```

约定两条：

- 方法收的是**挂载点内的相对路径**（挂载点前缀的剥离由 `MountTable::resolve` 完成）；
- 错误一律是**负数 errno**（`-ENOENT`/`-EISDIR`/`-ENOTDIR`/`-EEXIST`/`-EINVAL`/`-ENOSPC`），
  syscall handler 因此可以把文件系统错误直接返回用户态，中间不需要翻译层。

### 2.2 挂载表：路由是「可观测行为」而不是隐含逻辑

`MountTable::resolve(path)` 做**最长前缀匹配**，返回 `(后端, 挂载点内相对路径)`。
它存在的意义不只是「支持多个 FS」：**它是唯一把用户看到的路径变成某个后端内相对路径的地方**，
`vfs::stat/read/write/...` 每个入口都经过它，因此「请求落到了哪个文件系统」可以用串口断言验证：

```text
fs: mount point=/ fs=ramfs
fs: mount point=/mnt fs=ramfs-scratch
fs: route /mnt/note.txt fs=ramfs-scratch      ← 路由到 /mnt 的后端，而不是根后端
fs: readback /mnt/note.txt len=7 data=v0.7-ok ← 读回的确实是被路由过去的后端里的数据
```

后端用 `Box<dyn FileSystem>` **由挂载表独占持有**（不是 `static mut`）：所有访问都经
`resolve()` 取一次 `&mut`，不存在两个 `&mut` 同时指向同一后端的情形。

### 2.3 路径语义（刻意收窄）

| 输入 | 结果 |
| ---- | ---- |
| `/etc/motd`、`etc/motd` | 都规范化为 `/etc/motd`（允许省略前导 `/`） |
| `//a//b/` | `/a/b`（折叠重复与结尾斜杠） |
| `.` / `..` / 空串 | `EINVAL`（没有 cwd、没有父目录链接 —— 明确拒绝而不是猜一个含义） |
| `/etc/motd/deeper`（路径中间是文件） | `ENOTDIR` |
| `/nope` | `ENOENT` |

v0.7 **没有**：软/硬链接、权限位（`perms()` 返回固定串 `-rw-r--r--` / `drwxr-xr-x`，权限在 v1.0 的
`security/` 里做）、`rename`/`unlink`/`truncate`、稀疏文件（`write` 的 `offset > len` 返回 `EINVAL`）、
页缓存/写回（数据直接在内核堆上）。

### 2.4 RAMFS

- 数据模型就是 `Node { name, kind, data: Vec<u8>, children: Vec<Node> }`，**没有 inode 号、没有块分配、
  没有日志**；文件内容直接放在内核堆里，因此 `fs::init()` 必须在 `memory::init`（堆就绪）之后调用；
- 单文件上限 `MAX_FILE_BYTES` = 256 KiB（挡住用户态一次 `write` 把内核堆写爆的路径，超出返回 `ENOSPC`）；
- 目录列举按**插入顺序**返回（不排序），`ls -l` 风格的权限串来自 `Metadata::perms()`。

### 2.5 初始文件树（由 `fs::init` 建立）

```text
/            ramfs
├── init     用户程序 ELF（内核内嵌，见 syscall.md 3.1）
├── etc/
│   └── motd 文本文件（23 字节，用户程序用 open/read 读它）
└── tmp/     用户程序写文件的落点
```

启动日志（`make test-fs` 的断言来源）：

```text
fs: mount point=/ fs=ramfs
fs: create /init
fs: write /init bytes=<ELF 大小>
fs: mkdir /etc
fs: create /etc/motd
fs: write /etc/motd bytes=23
fs: mkdir /tmp
fs: ramfs files=2 bytes=<ELF 大小 + 23>
```

### 2.6 fd 表与 syscall 对接

fd 编号（与 Linux 一致的形状，见 syscall.md 2.7）：

| fd | 含义 | v0.7 行为 |
| ---- | ---- | ---- |
| 0 | stdin | **未接通**：`read` 返回 0（EOF），不让用户程序卡住 |
| 1 / 2 | stdout / stderr | 写串口（v0.6 行为不变，`user: write fd=1 len=N`） |
| ≥ 3 | RAMFS 文件 | 由 `fs::file` 的 fd 表管理（`FileHandle { path, offset, mode }`） |

- `open` 取**最小空闲槽位**，所以 `close(3)` 之后 `open` 会再次拿到 fd 3（实测：
  `user: close fd=3` → `user: open path=/tmp/hello.txt flags=0x41 fd=3`）；
- 偏移量由 `read`/`write` 推进，这是 v0.7 唯一的位置状态（没有 `lseek`、没有 `O_APPEND`）；
- 每次进入用户态前 `fs::file::reset()` 清空动态槽位（一次只跑一个用户程序，
  「每进程 fd 表」等 v0.8 的进程对象）。

### 2.7 验收（v0.7，`make test-fs` 19 项串口断言 PASS）

- [x] 挂载与初始文件树：`fs: mount point=/ fs=ramfs` / `fs: create /init` / `fs: write /etc/motd bytes=23` / `fs: mkdir /tmp`
- [x] 元数据与内容：`fs: stat /etc/motd kind=file size=23 perms=-rw-r--r--`、`fs: read /etc/motd len=23 data=motd: hello from ramfs`
- [x] **挂载点路由**：`fs: mount point=/mnt fs=ramfs-scratch` + `fs: route /mnt/note.txt fs=ramfs-scratch` + `fs: readback /mnt/note.txt len=7 data=v0.7-ok`
- [x] 目录列举（`ls` 语义）：`fs: ls / entries=3` + 每项 `name/kind/size/perms`
- [x] 负例 errno：`fs: negative cases PASS (ENOENT/ENOTDIR/EISDIR/EEXIST/EINVAL)`
- [x] 文件 syscall 全链路（随 `make test-user`，32 项断言）：`user: open path=/etc/motd flags=0x0 fd=3`
      → `user: read fd=3 want=64 got=23` → 内容经 `write` 打到串口 → `user: read fd=3 want=64 got=0`（EOF）
      → `user: close fd=3` → `user: open path=/tmp/hello.txt flags=0x41 fd=3` → `user: write fd=3 len=24`
      → 内核读回 `fs: readback /tmp/hello.txt len=24` + `fs: user data: written by user program`
- [ ] Shell 里 `cd`/`ls`/`cat` RAMFS 内文件 → 属 v0.9（需要 `init`/`sh` 与 `argc/argv`；v0.7 在**内核侧**
      与**一个用户程序**里验证了同样的语义，见 `user/hello`）
- [ ] 磁盘后端（FAT32）+ `mkfs`（H-03）→ v0.8 **剩余部分**（块设备那一半已完成：`make test-driver`）

## 3. 后续文件系统后端（v0.8 起）

### 3.1 FAT32 on virtio-blk

- v0.8 的块设备驱动（`driver/block.rs` + `driver/virtio_blk.rs`，2026-10-11 已就位）为 FAT32 提供了
  「按 512 字节扇区读写」的底座：FAT32 将作为第二个 `FileSystem` 实现挂到 VFS 上；
  syscall 层不需要改动 —— 这正是 2.1 把语义收进 trait 的目的；
- 届时 `read_all` 的零拷贝借用需要重新考虑（磁盘后端可能要在读时填充缓冲）。

### 3.2 mkfs（H-03）

- `tools/mkfs/mkfs.py` 仍是占位。v0.7 的 RAMFS 是**内核内建**的文件系统，不需要镜像生成；
  磁盘 FS 才需要 `mkfs`，因此它随 v0.8 落地。

## 4. 非目标 / 后续

- 页缓存/写回/一致性策略、`fsync`：非第一版目标；
- 完整权限与挂载语义（只读挂载、挂载选项、UID/GID）：v1.0 的 `security/`；
- 超大文件、高并发 IO、高性能目录索引：非第一版目标。

## 5. 修订记录

| 日期 | 变更 |
| ---- | ---- |
| 2026-10-06 | 第一阶段创建占位 |
| 2026-10-07 | 补记路线细化：明确 VFS 先行/RAMFS 后端/文件描述符表与 syscall 对接顺序、补充 ELF Loader 对接关系、补充 FAT32/mkfs 后续位置 |
| 2026-10-08 | 本次 docs 迭代统一文档顶部元信息（阶段 / 最后更新 / 状态）与修订记录，保持与 DEVELOPMENT.md 与索引文档描述一致 |
| 2026-10-11 | 备注：v0.8 的块设备层已落地（`block.rs` + `virtio_blk.rs`，`make test-driver`），FAT32 后端与 `mkfs` 仍未开始，因此本节仍按计划书写 |
| 2026-10-11 | **v0.7 核心落地**：从占位改为实现记录 —— 模块划分与 `FileSystem` trait、挂载表最长前缀路由（含路由证据日志）、路径语义收窄项、RAMFS 数据模型与单文件上限、初始文件树与启动日志、fd 表与 syscall 对接（fd 0/1/2 与 ≥3 的语义、最小空闲槽位、偏移量推进）、`make test-fs` 19 项验收；明确 Shell `cd/ls/cat` 属 v0.9、磁盘后端与 `mkfs` 属 v0.8 |
