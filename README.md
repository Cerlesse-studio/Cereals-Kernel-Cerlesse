# Cereals-Kernel-Cerlesse

> 代号 **Cerlesse OS** — 使用 Rust 从零开发的类 Linux 高性能宏内核。
> 内核核心：**Rust**；主机侧工具：**Python**（构建/测试）+ **Go**（编排/日志服务）。
> 支持 Linux / Windows 主机交叉开发，目标平台 x86_64（UEFI + QEMU 优先）。

## 当前进度

**H-06 包管理器 P0a 已落地（2026-10-11）**：内核包有了**独有后缀 `.ckc`**
（**CKC** = Cerlesse Kernel Container）—— 64 字节头（magic `CKC\x01` / 版本 / 头长 / flags /
payload_len / payload_off / 32 字节 payload_sha256 / 保留位，小端）+ **确定性 ustar** payload
（归档名 = 安装目标路径去 `/`、`PKG.toml` 第一、其余按字节序、mtime/uid/gid 全 0、仅常规文件、不压缩）。
**身份 = payload 的 SHA-256**（换算成 `rev`），因此同一棵目录树在任何机器上打包结果逐字节相同。
实现：`tools/pkgmgr/ckc.py`（纯标准库 `pack`/`verify`/`info`，退出码 0/1/2/4）；
验收：`make test-pkg`（`tests/test_ckc.py`，**37 项**，含用系统 `tar` 读同一段 payload 的
**独立复核**）；规格见 docs/package-manager.md 3.2。

**v0.9 已开局（2026-10-11，运行时前置条件第一步：用户堆）**：用户态终于**有堆**了。
内核侧 `brk`（256 KiB 窗口，Linux 语义：失败返回原值）/ `mmap`（匿名私有映射，256 KiB 窗口，first-fit，
新页**保证全零**）/ `munmap`（整段归还；清页表项时 `invlpg`，不留幽灵映射），
拒绝 `PROT_WRITE|PROT_EXEC`（W^X）、文件映射（无 offset 参数）、`PROT_NONE`（需要 `mprotect`）；
`libc` 侧注册 `#[global_allocator]`（bump over `mmap`），因此用户程序可以用 `Vec`/`String`/`Box`。
`make test-user` 断言由 32 项增至 **59 项**（零页、模式读回、**堆缓冲经 `write` 交给内核**、
6 个具体 errno 负例、`Vec`/`format!`，以及「用户态结束后页帧全部归还」的泄漏检查
`user: frames free before=119159 after=119159` → `user: heap frame reclamation PASS`）。
关联：docs/syscall.md 3.5、libc/README.md。

**v0.8 已完成（2026-10-11）**：驱动注册框架 + PCI 枚举 + 块设备（legacy virtio-blk）+ **磁盘文件系统（FAT12/16/32 只读，挂到 VFS）** + H-03 `mkfs`。
**v0.7 已落地（2026-10-11）**：VFS + RAMFS + ELF Loader、文件 syscall（`open`/`read`/`close`），详情见文末。

**v0.4 已完成（2026-10-08）**：中断控制器 + Timer + 键盘 + 上下文切换原语 + 主机侧 Go 工具（H-04/H-05）。
PIC（8259A）初始化与 IRQ→向量（32..47）映射 → IRQ 分发注册表 → PIT 通道 0 以 **100Hz（10ms/tick）**
产生 IRQ0 并维护 tick 计数（每 100 tick 打印一次 `IRQ0_heartbeat tick=<n>`）→
PS/2 键盘：8042 初始化（排空输出缓冲、打开 IRQ1 使能位）→ IRQ1 扫描码读取 →
set 1 解码（字母/数字/Shift/扩展前缀）→ 串口回显 `KB <字符>` →
内核态上下文切换原语（callee-saved + 栈切换，含内核自测 `context: switch PASS`）。

主机侧（H-04 testorch / H-05 serialmon）：`make go-test`（`go vet` + `go test`）PASS；
`make test-orch`（Go 编排器并行跑 6 个集成测试场景 + 串口二次复核）PASS（6 passed / 0 failed）。

自动化验证（内核侧）：`make test` 回归 PASS；`make test-exception`（#DE）PASS；
`make test-memory`（堆/帧/页表 + 上下文切换自测）PASS；`make test-pagefault`（#PF 诊断 + CR2）PASS；
`make test-keyboard`（经 QEMU monitor 注入真实按键，断言 `KB a` / `KBIRQ ENTRY` / `KBIRQ EXIT` / 心跳）PASS。

**v0.5 已完成（2026-10-08）**：内核线程 + Round Robin 时间片调度 + IPC（pipe 骨架）。
线程控制块与状态机（`process/thread.rs`）→ PID 分配（`process/pid.rs`）→ 定长环形就绪队列（`scheduler/queue.rs`）
→ Round Robin 策略（`scheduler/round_robin.rs`，200ms 时间片）→ PIT 100Hz/IRQ0 驱动的抢占式调度
（`scheduler/scheduler.rs`）：pid 1/2/3 三个内核任务严格轮转并绕回 pid 0（`sched: round-robin wrap PASS`）。
→ IPC 单向管道骨架（`ipc/pipe.rs`）：创建/写入/读取 + 串口摘要日志（`pipe: create/write/read`），
自测在 Round Robin 巡回之后执行（`pipe: selftest PASS`），验证不给调度器引入新崩溃面。

自动化验证：`make test-scheduler`（调度器轮转）PASS；`make test-ipc`（IPC 骨架验收）PASS；
全部 7 个集成测试目标（`test-all` 的组成项）逐项 PASS。channel / shared_memory 仍为占位（按计划 pipe 之后再做）。

v0.4/v0.5 期间修复的缺陷：异常向量 stub 之外的 IRQ 向量曾误入异常分发而 panic（B-08）、
IRQ 路径未保存通用寄存器导致被打断的代码偶发 #GP（B-09）、
上下文切换未保存 RFLAGS 导致切出中断上下文后 IF 永久为 0（B-10）、
IRQ 的 EOI 时序错误导致 PIC 停摆（B-11）。

**v0.6 核心已落地（2026-10-10）**：syscall 入口 + 用户态（ring3）+ 用户地址空间隔离。
选定 **`syscall` / `sysret`** 快速路径（`EFER.SCE` + `IA32_STAR/LSTAR/FMASK`，GDT 按 STAR 布局重排），
入口汇编自行切到**专用系统调用栈**（`syscall` 不切栈），返回用 `sysretq`；
用户地址空间为**独立 PML4**（用户映射固定在下标 1 = `0x8000000000`，每级页表均置 `USER`，
代码页只读可执行、栈页可写不可执行），`Drop` 时整棵释放。
首批 syscall：`write`（串口，先校验用户缓冲指针）/ `getpid` / `nanosleep`（PIT tick 粒度）/ `exit`。
来自 ring3 的异常按 `cs & 3` 识别后**终止用户程序而非 panic**（内核继续存活），
这就是隔离的验收形态：越权读内核页 → `vector=0xe error=0x5 cr2=0x2000000` + `user: isolation PASS`。
进入用户态前挂起调度器并将 `TSS.RSP0` 切到专用陷阱栈（否则 PIT 会在「用户页表 + 用户栈」的中间状态切到内核任务）。

**v0.6 用户态脚手架已可用（2026-10-11）**：用户程序不再需要手写机器码。
`libc/`（最小用户态运行时）提供 syscall 封装（`write`/`getpid`/`nanosleep`/`exit`）、
`_start` → 用户 `main` → `exit(main())` 的启动代码、`print!`/`println!`（一次宏调用 = 一次 `write`）
与 `#[panic_handler]`；`user/hello` 是第一个跑在 ring3 的**真实 Rust 用户程序**（不再内嵌汇编）。
当时构建链：`kernel/build.rs` 用嵌套 cargo 构建用户程序（large 代码模型 + `user-release` profile），
解析 ELF、摊平成「从 `USER_VIRT_BASE` 开始的连续映像」并 `include_bytes!` 内嵌；
`UserSpace::load_program` 按映像大小逐页映射（1..=8 页，`user/hello` 实测 2 页）。
（**v0.7 已把「摊平内嵌」换成 ELF Loader**，见下一节；`make user` 仍可单独验证脚手架，
编写新用户程序的约定见 [libc/README.md](libc/README.md)。）

自动化验证（当时）：`make test-user` 19 项串口断言（v0.7 增至 **32 项**，v0.9 第一步增至 **59 项**）PASS；
`tests/orchestrate.json` 的 `user` 场景同步断言脚手架证据（当时共 7 场景，v0.7 增至 8）。

**v0.7 已落地（2026-10-11）**：VFS + RAMFS + ELF Loader —— 用户程序不再需要手工摊平。

- **VFS**（`kernel/src/fs/`）：`FileSystem` trait（stat/read/read_all/write/create/mkdir/list/usage）+ 路径工具，
  错误一律负数 errno；**挂载表按最长前缀路由**（`/mnt/*` 落到 `/mnt` 的后端，日志 `fs: route ... fs=ramfs-scratch`）；
- **RAMFS**（第一个后端）：`Node { name, kind, data: Vec<u8>, children }`，单文件上限 256 KiB；
  `fs::init` 建立初始文件树 `/init`（用户程序 ELF）+ `/etc/motd` + `/tmp`；
  自测 `make test-fs` 断言 stat/内容读回/路由/`ls` 语义/负例 errno（19 项）；
- **fd 表与文件 syscall**（`fs/file.rs` + `syscall/file.rs`）：0/1/2 保留（stdin 未接通、串口输出），
  动态 fd ≥ 3 取最小空闲槽位；新增 `read`/`open`/`close`，`write` 支持文件 fd；
  用户缓冲按页校验（`-EFAULT`），单次 I/O 上限 4096 字节；
- **ELF Loader**（`syscall/elf.rs`）：ELF64/`ET_EXEC`/`EM_X86_64` → 按**段权限**逐页映射
  （`W` → 可写、非 `X` → `NX`）→ 拷 `p_filesz`、`p_memsz` 余下置 0 → 入口取 `e_entry`；
  用户地址布局固定为「映像窗口 64 KiB + 栈 4 页」；用户程序从 RAMFS 的 `/init` 读取装载，
  因此 `user/hello` 的 `RW` 段（`.data`/`.bss`）现在对 ring3 **可写**（实测 `hello: static counter=1`）；
- 构建链：`kernel/build.rs` 校验 ELF（基址/段页对齐/`W^X`/映像窗口/无重定位）后**原样内嵌**；
  `user.ld` 四个段分别 `ALIGN(0x1000)`；`make user` 跑 `tools/check_user_elf.py`（不启 QEMU 验段布局）。

自动化验证：`make test-user`（**32 项**断言，v0.9 第一步增至 **59 项**：`source=elf` + `segments/image_pages/writable_pages` +
`open/read/close/write` 全链路 + 内核读回用户写入的文件 + `static counter=1` + `EFAULT` + 退出码/隔离）PASS；
`make test-fs`（19 项）PASS；`tests/orchestrate.json` 增至 **8 场景**（新增 `fs`）全 PASS。

v0.7 仍**未做**（后续版本补齐）：`argc/argv`、`fork`/`execve`/`wait4` 与进程对象、动态链接、
内核高半区迁移（用户堆已于 v0.9 第一步补齐）；磁盘 FS（FAT32）与 `mkfs`（H-03）属 v0.8，
Shell 与 `ls`/`cat` 属 v0.9。

**v0.8 已落地（2026-10-11）**：驱动注册框架 + PCI 枚举 + 块设备（virtio-blk legacy）+ 磁盘文件系统（FAT12/16/32）。

- **PCI 枚举**（`kernel/src/driver/pci.rs`）：配置机制 #1（`0xCF8`/`0xCFC`），全总线扫描（用头部类型的
  多功能位把 256 个 function 的探测砍到 1 个），**BAR 探测**（写全 1 读掩码）并算出窗口大小；
  类别/子类/编程接口一并解码，日志用 `00:03.0` 形式便于对照 `lspci`;
- **驱动注册框架**（`kernel/src/driver/driver.rs`）：`Driver` trait（`name`/`matches`/`attach`）+ 固定容量
  注册表 + `probe_all()`：按注册顺序匹配、一个设备最多被一个驱动接管、`attach` 失败不阻断其它设备，
  统计以 `ProbeStats { devices, matched, attached, failed }` 返回（验收断言直接看这组数字）；
- **块设备抽象**（`kernel/src/driver/block.rs`）：`BlockDevice`（512 字节扇区、`lba` 绝对寻址），
  长度非整扇区**报错而不是静默取整**；设备注册表让上层只认 trait、不认具体驱动；
- **legacy virtio-blk（0.9.5）驱动**（`kernel/src/driver/virtio_blk.rs`）：状态机
  `ACKNOWLEDGE → DRIVER → FEATURES_OK → DRIVER_OK`，virtqueue（描述符表 + avail + 4 KiB 对齐的 used，
  三段必须**物理连续** → `frame::allocate_contiguous`），描述符链「请求头 → 数据 → 状态字节」，
  notify 后**轮询** used ring（超时用 PIT tick）；数据搬运走页对齐 bounce buffer，
  因此不要求上层缓冲对齐或连续；遇到 modern（0x1042）设备会打印原因并报错；
- **启动盘换成 virtio-blk**：`make run` 与全部 QEMU 测试都改为
  `-device virtio-blk-pci,drive=blk0,disable-modern=on` —— 「镜像能启动」与「内核驱动能读写这块盘」
  因此是**同一台设备上的同一条路径**。
- **磁盘文件系统（FAT12/16/32，只读）挂到 VFS**（`kernel/src/fs/fat.rs`）：类型按 BPB 的**簇数**判定
  （不靠扩展名或配置），支持 FAT12/16 的**固定根目录区**与 FAT32/子目录的**簇链**、
  8.3 短名（应用 NT 大小写标志）与 **LFN 长文件名**（带校验和验证）、任意偏移读、空文件（首簇 0）
  正确返回 0 字节；写操作一律 `EROFS`，簇链断/越界 → `EUCLEAN`（不静默返回半截数据）；
  `fs::mount_disks()` 把启动盘挂到 `/disk`、`mkfs` 生成的测试盘挂到 `/mnt/fat`（顺带验证最长前缀路由），
  **syscall 层一行未改** —— 这正是 VFS 收口的目的；`FileSystem` trait 随之收窄为 `&mut self` 且
  `read_all` 返回 `Vec<u8>`（磁盘文件内容不在内存里，原来那种「借用后端缓冲」的形状不再成立）；
- **H-03 `mkfs`**（`tools/mkfs/mkfs.py`）：自己生成 FAT12/16/32 镜像（引导扇区/FSInfo/FAT 副本/
  LFN + 8.3 目录项/簇链），写完回读校验；正确性由 **mtools 独立复核**（`mdir`/`mtype` 读同一张盘）——
  两套独立的 FAT 实现都认同一张盘，才能说明「镜像本身是对的」。

自动化验证：`make test-driver` **22 项**断言 PASS（`vendor=0x1af4 device_id=0x1001`、`bar0=io:`、
`queue0 size=256 region=... (3 pages contiguous)`、`capacity=131072 sectors (64 MiB)`、
`boot sector sig=0x55aa fstype=FAT16`、512/4096/4608 字节分块读一致性、最后扇区写回读+还原、三类负例）；
`make test-fat` **21 项**断言 PASS（**双盘启动**：从启动盘读回**内核自己的映像**
`fs: read /disk/KERNEL.ELF magic=0x7f454c46 size=3005280` 与 `BOOTX64.EFI magic=0x4d5a`；
测试盘上跨簇文件 3000 字节**逐字节对公式**、偏移 1024 读、空文件、LFN 长文件名、二级子目录，
以及 `fs: disk negative cases PASS (ENOENT/ENOTDIR/EISDIR/EROFS)`）；
`tests/orchestrate.json` 增至 **10 场景**（新增 `driver` 与 `fat`）全 PASS。
另用非 `snapshot=on` 方式启动后比对镜像 md5，确认写测试真的把最后一个扇区恢复了原样。

v0.8 仍**未做**（明确不算完成）：FAT 后端**只读**（写盘/簇分配器、扇区缓存属后续）、不解析分区表、
modern（virtio 1.0）、MSI-X/INTx 中断、超过两块盘、BAR 重分配、ECAM/ACPI。

下一步：按 DEVELOPMENT.md 第 5 节顺序继续 —— v0.9（Shell + 用户程序 + 网络）。

## 文档入口

- [docs/README.md](docs/README.md) — 文档中心与阶段状态
- [docs/architecture.md](docs/architecture.md) — 内核架构与语言分工
- [docs/roadmap.md](docs/roadmap.md) — 开发路线与 v0.1–v1.0 里程碑
- [docs/project-structure.md](docs/project-structure.md) — 目录结构与模块职责
- [DEVELOPMENT.md](DEVELOPMENT.md) — 主开发文档（需求/任务清单/测试/QEMU/Issue 规划）

## 项目结构（概览）

```text
boot/      UEFI 引导（v0.1 ✅）   kernel/    Rust 内核主体（v0.8 ✅，含内存/中断/调度/syscall/fs/驱动）
libc/      最小用户态运行时（v0.7 ✅，含 open/read/close）
shared/    boot/kernel 共享结构    user/      用户态程序（v0.7：hello ✅，其 ELF 即 RAMFS 上的 /init）
tools/     Python 主机工具        tools-go/  Go 主机服务
tests/     QEMU 集成测试（v0.1 ✅） docs/     全部开发文档
```

驱动分层（v0.8，详见 [docs/drivers.md](docs/drivers.md)）：

```text
pci.rs ──枚举──▶ driver.rs ──匹配/挂接──▶ virtio_blk.rs
（总线上有什么）   （谁来管它）              （实现 Driver + BlockDevice）
                        │
                        └──▶ block.rs（按扇区读写的统一接口 + 设备注册表）
                                  │
磁盘上的文件：                     ▼
fs/fat.rs（FAT12/16/32 只读后端）──▶ fs/vfs.rs + 挂载表 ──▶ open/read/close（syscall 层零改动）
```

## 常用命令

```bash
make build           # 构建 boot + kernel + 磁盘镜像
make run             # QEMU + OVMF 启动（串口输出到终端）
make test            # 集成测试：断言串口出现 "Kernel started!"
make test-exception  # v0.2：注入 #DE，断言异常诊断输出
make test-memory     # v0.3/v0.4：断言帧分配器 / 内核堆 / 页表 Mapper / 上下文切换自测
make test-pagefault  # v0.3：访问未映射地址，断言 #PF 诊断 + CR2
make test-keyboard   # v0.4：经 QEMU monitor 注入按键，断言 IRQ1 回显 + PIT 心跳
make test-scheduler  # v0.5：断言 ≥3 个内核任务在 PIT 时间片下轮转
make test-ipc        # v0.5：断言 pipe 创建/写入/读取摘要日志 + 自测 PASS
make test-fs         # v0.7：断言 VFS 挂载路由 + RAMFS 读/写/ls + 负例 errno
make test-user       # v0.7/v0.9：ELF 从 RAMFS 装载 + 文件 syscall + 用户堆（brk/mmap/munmap + Vec）+ 隔离（59 项）
make test-driver     # v0.8：断言 PCI 枚举 + 驱动挂接 + virtio-blk 扇区读/写/负例
make fat-image       # v0.8：用 tools/mkfs/mkfs.py 生成 FAT32 测试盘（内容见 tests/fatroot/）
make test-fat        # v0.8：双盘启动，断言 FAT16/32 挂到 VFS 后的读/ls/LFN/跨簇/偏移读/EROFS
make user            # v0.7：单独构建并校验用户态脚手架（libc + user/hello → 段页对齐的 ELF）
make test-pkg        # H-06 P0a：CKC 包容器（.ckc）与打包/校验规则（主机侧，不启 QEMU）
make test-all        # 全部集成测试
make go-test         # v0.4 H-04/H-05：Go 单元测试（串口分析 + 并行编排器）
make test-orch       # v0.4 H-04：Go 编排器并行跑 10 个集成测试场景（tests/orchestrate.json）
```
