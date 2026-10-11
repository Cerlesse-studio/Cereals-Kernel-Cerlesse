# API 参考（系统调用 / 用户态 / 内核内部 / 主机工具）

- 阶段：第八阶段（v0.9 进行中）
- 最后更新：2026-10-11
- 状态：🟡 持续更新 —— 本文汇总**当前代码实际提供**的对外接口：
  系统调用 ABI、用户态 `libc` API、内核内部 trait、主机侧工具 CLI 与 Makefile 目标。
  接口随版本演进，**以本文与源码注释为准**；设计背景见 [syscall.md](./syscall.md)、
  [process.md](./process.md)、[filesystem.md](./filesystem.md)、[drivers.md](./drivers.md)。

> 本文档顶部保留阶段 / 最后更新 / 状态字段，与项目文档维护约定对齐。
> 约定：**负数返回值 = `-errno`**（`-2` = `-ENOENT`）。Rust 侧错误一律用负数，
> 中间层不做翻译，`errno` 常量见第 3 节。

## 1. 系统调用 ABI（x86_64）

### 1.1 寄存器约定

| 项 | 约定 |
| ---- | ---- |
| 指令 | `syscall`（`EFER.SCE` + `IA32_STAR` / `IA32_LSTAR` / `IA32_FMASK`），返回 `sysretq` |
| 调用号 | `rax` |
| 参数 | `rdi, rsi, rdx, r10, r8`（最多 5 个，与 Linux x86_64 一致） |
| 返回值 | `rax`；**负数 = `-errno`** |
| 被破坏的寄存器 | 仅 `rax`、`rcx`、`r11`；其余寄存器（含 `rdi/rsi/rdx/r10/r8/r9`）**保持进入前的值**（入口汇编保存/恢复） |
| 入口栈 | 内核入口切到**专用系统调用栈**（`syscall` 指令不切栈）；返回前恢复 |

内核入口与分发见 `kernel/src/syscall/trampoline.rs`，号表见 `kernel/src/syscall/syscall_table.rs`。
用户态封装见 `libc/src/syscall.rs`。

### 1.2 调用号总表

编号沿用 **Linux x86_64** 语义，便于将来直接运行现成用户态程序。
号段上限 `SYS_MAX = 256`；**号越界**与**号合法但未实现**是两种可区分的失败
（前者 `parse` 返回 `None`，后者落在公共 ENOSYS 桩，均返回 `-ENOSYS`）。

| 号 | 名称 | 状态 | 说明 |
| ---- | ---- | ---- | ---- |
| 0 | `read` | ✅ | 从 fd 读入缓冲；fd 0 为行缓冲 stdin |
| 1 | `write` | ✅ | fd 1/2 写串口，fd ≥ 3 写文件 |
| 2 | `open` | ✅ | 打开/创建文件，返回 fd（≥ 3） |
| 3 | `close` | ✅ | 关闭动态 fd |
| 9 | `mmap` | ✅ | 匿名私有映射（无 offset） |
| 10 | `mprotect` | ⛔ | 号已定义但未实现（落 ENOSYS 桩） |
| 11 | `munmap` | ✅ | 整段归还匿名映射 |
| 12 | `brk` | ✅ | 调整用户堆顶（Linux 语义） |
| 35 | `nanosleep` | ✅ | 睡眠 N 个 PIT tick（100Hz） |
| 39 | `getpid` | ✅ | 当前进程 PID |
| 57 | `fork` | ✅ | 复制当前进程 |
| 59 | `execve` | ✅ | 换映像（成功后不返回） |
| 60 | `exit` | ✅ | 结束进程并交回退出码 |
| 61 | `wait4` | ✅ | 运行/回收直接子进程，返回退出码 |
| 其他 | — | ENOSYS | 号合法但未实现：查表命中 ENOSYS 桩；常见者会打印 `syscall: ENOSYS nr=… name=…` |

## 2. 系统调用逐条说明

参数顺序即寄存器顺序 `rdi, rsi, rdx, r10, r8`。

### 2.1 `read`（nr=0）

```
isize read(i32 fd, *mut u8 buf, usize count)   // libc: read(fd: i32, buf: &mut [u8]) -> isize
```

- `buf` 必须落在**当前用户地址空间**内且对 ring3 **可写**，否则 `-EFAULT`。
- `fd = 0`（stdin）：**行缓冲（cooked）** —— 交付**一整行**（行尾统一 `\n`），
  无整行时等待，超过 `STDIN_IDLE_TICKS = 300`（PIT 100Hz → **3 秒**）返回 `-EAGAIN`(-11)。
  交互程序应把 `EAGAIN` 当作「暂无输入」而非错误。
- `fd ≥ 3`：从文件读，返回字节数，**0 = EOF**。
- 错误：`EBADF`（fd 无效/模式不符）、`EFAULT`、`EAGAIN`（仅 stdin）、`EISDIR`、`EINVAL`。

### 2.2 `write`（nr=1）

```
isize write(i32 fd, const u8 *buf, usize count) // libc: write(fd: i32, buf: &[u8]) -> isize
```

- `buf` 必须可读（按页校验）。
- `fd = 1` / `2`：写串口（stdout/stderr 均接串口），返回写入字节数。
- `fd ≥ 3`：写文件。
- 错误：`EBADF`、`EFAULT`、`EINVAL`、`ENOSPC`（超过 RAMFS 单文件上限）。

### 2.3 `open`（nr=2）

```
isize open(const u8 *path, usize path_len, i32 flags)  // libc: open(path: &str, flags: i32) -> isize
```

- 路径用**指针 + 长度**传递（内核已校验区间，不在不可信内存上找 NUL）。
- `flags` = `O_RDONLY`(0) / `O_WRONLY`(1) / `O_RDWR`(2) 与可选的 `O_CREAT`(0o100) 组合；
  未知位返回 `EINVAL`。
- 返回 fd **≥ 3**（取最小空闲槽位），或负 errno。
- 错误：`ENOENT`、`ENOTDIR`、`EISDIR`、`EINVAL`、`ENOSPC`（fd 用尽，`FD_MAX = 16`）、`EROFS`（只读后端）。

### 2.4 `close`（nr=3）

```
isize close(i32 fd)   // libc: close(fd: i32) -> isize
```

- 只接受 `fd ≥ 3`；0/1/2 是保留位，关闭它们返回 `-EBADF`。
- 成功返回 0。

### 2.5 `mmap`（nr=9）

```
isize mmap(usize len, i32 prot, i32 flags)   // libc: mmap(len: usize, prot: i32, flags: i32) -> isize
```

- **只支持私有匿名映射**：`flags` 必须含 `MAP_PRIVATE|MAP_ANONYMOUS`，`fd` 固定 `-1`，**没有 offset 参数**（ABI 5 参数）。
- `prot` 允许 `PROT_READ`(0x1) / `PROT_WRITE`(0x2) / `PROT_EXEC`(0x4)；拒绝 `PROT_WRITE|PROT_EXEC`（W^X）与 `PROT_NONE`。
- 返回页对齐地址（> 0）或负 errno；**新页保证全零**；窗口 256 KiB，first-fit。
- 错误：`EINVAL`（位非法/文件映射/PROT_NONE）、`ENOMEM`（超窗口/无空闲帧）。

### 2.6 `munmap`（nr=11）

```
isize munmap(u64 addr, usize len)   // libc: munmap(addr: u64, len: usize) -> isize
```

- 整段归还（要求页对齐且全段已映射）；清页表项并 `invlpg`，不留幽灵映射。
- 错误：`EINVAL`。

### 2.7 `brk`（nr=12）

```
u64 brk(u64 addr)   // libc: brk(addr: u64) -> u64
```

- 沿用 **Linux 语义**：`addr = 0` 只查询当前堆顶；否则页对齐增长/收缩（窗口 256 KiB）。
- **不返回错误**：失败时返回**原值**，调用方用 `ret >= addr` 判断成功。

### 2.8 `nanosleep`（nr=35）

```
isize nanosleep(u64 ticks)   // libc: nanosleep(ticks: u64) -> isize
```

- 睡眠 `ticks` 个 PIT tick（100Hz → 1 tick = 10ms）；等待期间 `sti; hlt`，tick 真实推进。

### 2.9 `getpid`（nr=39）

```
u64 getpid()   // libc: getpid() -> u64
```

- 当前进程 PID（内核分配的顺序号）。

### 2.10 `fork`（nr=57）

```
isize fork()   // libc: fork() -> isize
```

- **父进程**返回子进程 PID（> 0）；**子进程**返回 0，并从**同一条用户指令**继续
  （内核恢复 `fork` 时刻的完整寄存器现场，只把返回值改成 0）。
- 失败：`-EAGAIN`（进程表满）、`-ENOMEM`（地址空间复制失败）。
- **调度差异**：单 CPU、用户态无抢占，子进程**不会与父进程并发运行**，要等父进程 `wait4`
  才被恢复 —— 标准写法 `if pid == 0 { execve(..) } else { wait4(pid) }` 原样可用。

### 2.11 `execve`（nr=59）

```
isize execve(const u8 *path, usize path_len, const u64 *argv_pairs, usize argc, u64 _)
// libc: execve(path: &str, argv: &[&str]) -> isize
```

- **成功后不返回**（内核换 CR3 并恢复新入口帧）；失败返回负 errno，调用方通常立刻 `exit`。
- `argv` 用「指针 + 长度」对数组传递：`argv_pairs[2i]` = 字符串指针、`argv_pairs[2i+1]` = 长度；
  内核逐段校验后写进**新进程的用户栈**。上限 `ARG_MAX = 8` 项、单项 `ARG_MAX_LEN = 256` 字节。
- 错误：`ENOENT`（映像不存在）、`E2BIG`（argv 超限）、`EFAULT`、`EINVAL`（ELF 被拒）。

### 2.12 `exit`（nr=60）

```
! exit(i32 code)   // libc: exit(code: i32) -> !
```

- 结束当前进程并把退出码交回内核。顶层程序（`/init`、`/bin/sh`）结束后控制权回到内核；
  子进程结束后回到父进程的 `wait4`。**不返回**。

### 2.13 `wait4`（nr=61）

```
isize wait4(i64 pid)   // libc: wait4(pid: i64) -> isize
```

- 运行并**回收**直接子进程（地址空间整棵归还），返回它的退出码（可为负）。
- 非直接子进程 / 已回收：`-ECHILD`(-10)。

## 3. errno 常量

| 数值 | 名称 | 含义 | 用户态 `libc` 导出 |
| ---- | ---- | ---- | ---- |
| -2 | `ENOENT` | 路径不存在 | ✅ |
| -5 | `EIO` | 块设备读写失败（内核内部） | — |
| -8 | `ENOEXEC` | ELF 格式错误（内核内部，execve 折算为 EINVAL） | — |
| -9 | `EBADF` | fd 无效 / 未打开 / 模式不符 | ✅ |
| -10 | `ECHILD` | `wait4` 找不到直接子进程 | ✅ |
| -11 | `EAGAIN` | 暂无数据（非阻塞语义，**不是错误**） | ✅ |
| -12 | `ENOMEM` | 内存不足 | ✅ |
| -14 | `EFAULT` | 用户地址区间非法 | ✅ |
| -17 | `EEXIST` | 目标已存在（内核内部） | — |
| -20 | `ENOTDIR` | 路径中间分量不是目录 | — |
| -21 | `EISDIR` | 目标是目录，但被当文件用 | ✅ |
| -22 | `EINVAL` | 参数非法 | ✅ |
| -28 | `ENOSPC` | 空间/句柄不足 | ✅ |
| -30 | `EROFS` | 只读文件系统 | — |
| -38 | `ENOSYS` | 号合法但未实现（v0.9 起只剩 `mprotect`） | ✅ |
| -117 | `EUCLEAN` | 磁盘结构损坏（簇链断/越界/目录项非法） | — |

## 4. 用户态 `libc` API

`libc/` 是最小用户态运行时（`no_std`）。普通用户程序**只用这一节**；内核细节见第 5 节。
完整用法与新程序模板见 [../libc/README.md](../libc/README.md)。

### 4.1 系统调用封装（`libc::syscall`，全部 re-export 到 crate 根）

| 函数 | 签名 | 备注 |
| ---- | ---- | ---- |
| `raw` | `unsafe fn raw(nr, a1..a5) -> isize` | 裸入口；不要用魔法数调用 |
| `write` | `fn write(fd: i32, buf: &[u8]) -> isize` | |
| `read` | `fn read(fd: i32, buf: &mut [u8]) -> isize` | stdin 行缓冲（见 2.1） |
| `open` | `fn open(path: &str, flags: i32) -> isize` | 返回 fd ≥ 3 或负 errno |
| `close` | `fn close(fd: i32) -> isize` | |
| `brk` | `fn brk(addr: u64) -> u64` | Linux 语义，失败返回原值 |
| `mmap` | `fn mmap(len: usize, prot: i32, flags: i32) -> isize` | 匿名私有，无 offset |
| `munmap` | `fn munmap(addr: u64, len: usize) -> isize` | |
| `getpid` | `fn getpid() -> u64` | |
| `nanosleep` | `fn nanosleep(ticks: u64) -> isize` | 1 tick = 10ms |
| `fork` | `fn fork() -> isize` | 父 = 子 PID，子 = 0 |
| `execve` | `fn execve(path: &str, argv: &[&str]) -> isize` | 成功不返回 |
| `wait4` | `fn wait4(pid: i64) -> isize` | 返回退出码或 `ECHILD` |
| `exit` | `fn exit(code: i32) -> !` | |

### 4.2 常量

- fd：`STDIN = 0`、`STDOUT = 1`、`STDERR = 2`、`FD_FIRST_DYNAMIC = 3`。
- `open` 标志：`O_RDONLY = 0`、`O_WRONLY = 1`、`O_RDWR = 2`、`O_CREAT = 0o100`。
- `mmap` 保护位：`PROT_READ = 0x1`、`PROT_WRITE = 0x2`、`PROT_EXEC = 0x4`。
- `mmap` 标志：`MAP_PRIVATE = 0x2`、`MAP_ANONYMOUS = 0x20`。
- errno：`ENOENT/EBADF/ECHILD/EAGAIN/ENOMEM/EFAULT/EISDIR/EINVAL/ENOSPC/ENOSYS`（数值见第 3 节）。
- 进程：`ARG_MAX = 8`。

### 4.3 宏与运行时支撑

| 项 | 说明 |
| ---- | ---- |
| `print!` / `println!` | 固定 256 字节缓冲；**一次宏调用 = 一次 `write`**（日志里可见 `len=`） |
| `_start` | `global_asm!` 放在 `.text.start`：`_start` → 用户 `main` → `exit(main())` |
| `#[panic_handler]` | 打 stderr 后 `exit(101)` |
| `heap` | `#[global_allocator]`：bump over `mmap`（64 KiB 块）；`dealloc` 是空操作，长驻程序内存会单调上升 |
| `args` | `argc()` / `arg(i)` / `arg_str(i)` / `Args`：入口寄存器 `rdi = argc`、`rsi = argv` |

### 4.4 用户程序构建约束（硬约束，改错会在链接期或 ring3 立刻炸）

| 约束 | 取值 | 原因 |
| ---- | ---- | ---- |
| 目标 | `x86_64-unknown-none` | 裸机用户态 |
| 代码模型 | `-C code-model=large` | 链接在 2^39，超出 small 模型 |
| 重定位 | `-C relocation-model=static` | 内核不做加载时重定位（只接受 `ET_EXEC`） |
| profile | `user-release`（`lto = false`） | large 模型与 LTO 不兼容 |
| 链接基址 | `USER_VIRT_BASE = 0x0000_0080_0000_0000` | 链接地址 = 运行地址，无需重定位 |
| 段对齐 | 每段 `ALIGN(0x1000)` | 装载器按页设权限，同页 W+X 会让 W^X 退化被拒 |
| 静态变量 | 归入 `.bss`（含 `.lbss.*`） | 否则成孤儿段，RW 段不再页对齐 |
| 内存窗口 | 映像 ≤ 64 KiB、栈 16 KiB、brk 堆 256 KiB、匿名 mmap 256 KiB | 固定布局，超窗失败 |

## 5. 内核内部接口（crate 内 trait / 系统级入口）

> 这些接口只在内核编译单元内可见，不是用户 ABI；列在此处便于二次开发与评审。

### 5.1 文件系统：`FileSystem` trait（`kernel/src/fs/vfs.rs`）

后端实现**挂载点内的相对路径**（`/a/b`；挂载点本身是 `/`），挂载点前缀剥离由挂载表完成。
所有路径操作收 `&mut self`（挂载表独占持有后端）。错误一律负数 `i64`。

```rust
pub trait FileSystem {
    fn name(&self) -> &'static str;
    fn stat(&mut self, path: &str) -> Result<Metadata, i64>;
    fn read(&mut self, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, i64>;
    fn read_all(&mut self, path: &str) -> Result<Vec<u8>, i64>;
    fn write(&mut self, path: &str, offset: u64, data: &[u8]) -> Result<usize, i64>;
    fn create(&mut self, path: &str) -> Result<(), i64>;
    fn mkdir(&mut self, path: &str) -> Result<(), i64>;
    fn list(&mut self, path: &str) -> Result<Vec<DirEntry>, i64>;
    fn usage(&self) -> (usize, usize) { (0, 0) }   // (文件数, 占用字节数)
}
```

现有实现：`ramfs::RamFs`（内存，可读写，单文件 ≤ 256 KiB）、`fat::FatFs`（FAT12/16/32 **只读**，写操作 `EROFS`）。

**VFS 系统级入口**（经挂载表最长前缀路由；`kernel/src/fs/vfs.rs`）：
`stat` / `read` / `read_all` / `write` / `create` / `mkdir` / `list` / `usage` /
`is_mount_point` / `backend_name`。
路径工具：`normalize`（拒绝 `.`/`..`）、`components`、`within`。

挂载表（`fs/mount.rs`）：`resolve(path)` → `(&mut dyn FileSystem, String)`；`is_mount_point`；
启动时 `fs::init` 建立初始树（`/init`、`/etc/motd`、`/tmp`、`/bin`），`fs::mount_disks` 挂
`/disk`（启动盘）与 `/mnt/fat`（测试盘）。

### 5.2 驱动：`Driver` trait（`kernel/src/driver/driver.rs`）

```rust
pub trait Driver {
    fn name(&self) -> &'static str;
    fn matches(&self, dev: &PciDevice) -> bool;   // 无副作用
    fn attach(&self, dev: &PciDevice) -> Result<(), &'static str>;
}
```

注册表 API：`register(driver) -> Result<usize, &str>`、`count()`、`get(i)`、
`register_builtin()`、`report()`、`probe_all() -> ProbeStats`、`driver_for(dev)`。
匹配语义：按注册顺序，首个 `matches` 为真的驱动认领设备；一个设备最多挂接一次；
`attach` 失败不阻断其它设备，计入 `ProbeStats.failed`。

```rust
pub struct ProbeStats { pub devices: usize, pub matched: usize,
                        pub attached: usize, pub failed: usize }
```

### 5.3 块设备：`BlockDevice` trait（`kernel/src/driver/block.rs`）

```rust
pub const SECTOR_SIZE: u32 = 512;
pub trait BlockDevice {
    fn name(&self) -> &'static str;
    fn sector_size(&self) -> u32;                 // v0.8 恒为 512
    fn sectors(&self) -> u64;
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<usize, &'static str>;
    fn write(&mut self, lba: u64, data: &[u8]) -> Result<usize, &'static str>;
    fn capacity_bytes(&self) -> u64 { self.sectors() * self.sector_size() as u64 }
}
```

- `lba` = 绝对逻辑块地址（v0.8 不解析分区表）；长度必须是扇区整数倍，否则报错（不静默取整）。
- 注册表：`register` / `count` / `get(i)` / `first`；`is_sector_aligned(len)`。
- 现有实现：`virtio_blk::VIRTIO_BLK`（legacy virtio-blk 0.9.5，同步轮询 + 页对齐 bounce buffer）。

### 5.4 内存与调度（速览）

| 模块 | 关键接口 |
| ---- | ---- |
| `memory::frame` | 位图帧分配器；`allocate` / `free` / `allocate_contiguous` / `free_contiguous`（DMA/virtqueue 用） |
| `arch::x86_64::paging` | 4 级页表：`translate` / `map_4k` / `unmap_4k` / TLB 刷新；`AddressSpace` |
| `memory::heap` | `#[global_allocator]`（内核堆，映像末尾 1MiB + 首次适配空闲链表） |
| `scheduler` | Round Robin（200ms 时间片）；`suspend` / `resume` / `spawn` |
| `process` | `Process`（PCB：地址空间 / 用户帧 / 每进程 64 KiB 内核栈 / `Ready\|Running\|Zombie`） |
| `shared` | `BootInfo`、`BOOT_MAGIC`（boot 与 kernel 共享的纯数据结构，`no_std`） |

### 5.5 构建期接口（`kernel/build.rs`）

- 用嵌套 `cargo` 构建 `USER_PROGRAMS` 列表中的用户程序（large 代码模型 + `user-release`）；
- 校验 ELF（装载基址 = `USER_VIRT_BASE`、段页对齐、`W^X`、映像窗口、无重定位）后**原样内嵌**；
- `kernel/src/syscall/user.rs` 以 `include_bytes!` 取回并写入 RAMFS 对应路径。

## 6. 主机侧工具 CLI

主机侧工具**不进入内核**，只在开发机上运行（Python / Go）。更多说明见
[../tools/README.md](../tools/README.md) 与 [../tools-go/README.md](../tools-go/README.md)。

| 工具 | 命令 | 说明 |
| ---- | ---- | ---- |
| 镜像构建 | `tools/image-builder/`（`make image`） | 组装 boot + kernel + 64 MiB FAT 磁盘镜像 |
| ELF 检查 | `python3 tools/check_user_elf.py <file.elf>` | 段布局/基址/页对齐/`W^X`/窗口/重定位（不启 QEMU） |
| mkfs | `python3 tools/mkfs/mkfs.py --out <img> --from <dir>` | 生成 FAT12/16/32 镜像；mtools 独立复核 |
| CKC 打包 | `python3 tools/pkgmgr/ckc.py pack <tree> -o <out.ckc> [--print-rev]` | 生成 `.ckc` 容器（确定性 ustar + 64 字节头） |
| CKC 校验 | `python3 tools/pkgmgr/ckc.py verify <file.ckc>` | 校验头字段与 payload 哈希 |
| CKC 信息 | `python3 tools/pkgmgr/ckc.py info <file.ckc>` | 打印头与条目信息 |
| 测试编排 (Go) | `go run ./cmd/testorch -config ../tests/orchestrate.json -root ..` | 并行跑场景 + 串口复核 |
| 串口分析 (Go) | `go run ./cmd/serialmon -file <log> [-once] [-expect S]... [-allow-panic]` | 跟随/快照分析串口日志 |

CKC 退出码：`0` 成功、`1` 用法/参数错误、`2` 校验失败、`4` I/O 错误。
serialmon 退出码：`0` 通过、`1` 异常/缺失、`2` 用法或 IO 错误。

## 7. 构建与测试目标（Makefile）

```bash
make build / make run            # 构建镜像 / QEMU + OVMF 启动
make test ... test-fat           # 各集成测试（断言串口日志）
make test-all                    # test + exception + memory + pagefault + keyboard
                                 #   + shell + proc + scheduler + ipc + fs + user + driver + fat
make user                        # 单独构建并校验三个用户程序（不启 QEMU）
make test-pkg                    # CKC 包容器测试（主机侧，不启 QEMU）
make go-test / make test-orch    # Go 单元测试 / 编排器并行跑全部场景
make fat-image                   # 生成 FAT32 测试盘
make clean
```

| 目标 | 断言内容 | 项数 |
| ---- | ---- | ---- |
| `test` | 串口出现 `Kernel started!` | — |
| `test-exception` | 注入 #DE，异常诊断 | — |
| `test-memory` | 帧/堆/页表自测 + 上下文切换 | — |
| `test-pagefault` | #PF 诊断 + CR2 | — |
| `test-keyboard` | IRQ1 行缓冲整行交付 + PIT 心跳 | — |
| `test-scheduler` | Round Robin 轮转 | — |
| `test-ipc` | pipe 创建/写/读 | — |
| `test-fs` | VFS 路由 + RAMFS 读/写/ls + 负例 | 20 |
| `test-user` | ELF 装载 + 文件 syscall + 用户堆 + 隔离 + Shell | 61 |
| `test-shell` | 交互式 Shell（注入按键） | 11 |
| `test-proc` | fork/execve/wait4 全链路 + PID 一致性 | 8 + 4 正则 |
| `test-driver` | PCI 枚举 + 驱动挂接 + 扇区读写/负例 | 22 |
| `test-fat` | 双盘 FAT16/32 读/LFN/跨簇/偏移/EROFS | 21 |
| `test-pkg` | `.ckc` 容器规则 + `tar` 独立复核 | 37 |

## 8. 修订记录

| 日期 | 变更 |
| ---- | ---- |
| 2026-10-11 | 创建本文档：汇总系统调用 ABI（寄存器约定/号表/逐条语义）、errno 表、`libc` API、
内核内部 trait（`FileSystem`/`Driver`/`BlockDevice`）、主机侧工具 CLI 与 Makefile 目标 |
