# Cerlesse OS 主开发文档（DEVELOPMENT）

- 阶段：第八阶段（v0.9 进行中）
- 最后更新：2026-10-11
- 状态：**v0.1 – v0.8 全部完成并通过自动化验收；v0.9 已落地三步（用户堆 / `stdin` + 交互式 Shell /
  进程对象 + `fork`·`execve`·`wait4`），剩余动态链接与网络。**
- 关联文档：[README.md](README.md) · [docs/README.md](docs/README.md) · [docs/api.md](docs/api.md) ·
  [docs/software-development.md](docs/software-development.md) · [docs/architecture.md](docs/architecture.md) ·
  [docs/roadmap.md](docs/roadmap.md) · [docs/project-structure.md](docs/project-structure.md)

> 本文是**需求与版本任务清单**的主开发文档。接口细节见 [docs/api.md](docs/api.md)，
> 开发流程与规范见 [docs/software-development.md](docs/software-development.md)。

---

## 1. 功能需求

### 1.1 核心需求（Rust 内核）

| 编号 | 需求 | 版本 | 状态 |
| ---- | ---- | ---- | ---- |
| R-01 | UEFI 引导进入 Rust 内核入口，串口输出 | v0.1 | ✅ |
| R-02 | GDT/IDT 与异常处理（#PF/#GP/#DF 等） | v0.2 | ✅ |
| R-03 | 物理帧分配 + 虚拟内存映射 + 内核堆分配器 | v0.3 | ✅ |
| R-04 | 中断控制器（PIC/APIC）、Timer、键盘输入 | v0.4 | ✅（APIC 未做） |
| R-05 | 进程/线程模型、上下文切换、Round Robin 调度 | v0.5 | ✅ |
| R-06 | 系统调用入口、首批 syscall、用户态隔离 | v0.6 | ✅ |
| R-07 | VFS + RAMFS + ELF Loader | v0.7 | ✅ |
| R-08 | 驱动框架、PCI 枚举、磁盘驱动 | v0.8 | ✅ |
| R-09 | Shell、基础用户程序、简化网络栈 | v0.9 | 🟡（Shell/用户程序 ✅，网络未做） |
| R-10 | 权限/凭证、可完整启动的类 Linux 小型 OS | v1.0 | ⬜ |

### 1.2 主机侧需求（Python / Go）

| 编号 | 需求 | 语言 | 状态 |
| ---- | ---- | ---- | ---- |
| H-01 | 镜像构建脚本（组装 boot + kernel + FS 镜像） | Python | ✅ v0.1 |
| H-02 | QEMU 自动化：启动、超时、串口日志断言 | Python | ✅ v0.1 |
| H-03 | `mkfs` 工具（FAT12/16/32 镜像生成） | Python | ✅ v0.8（`tools/mkfs/mkfs.py`，mtools 复核） |
| H-04 | 测试编排服务（并行多场景跑 QEMU 测试） | Go | ✅ v0.4（`tools-go/cmd/testorch`） |
| H-05 | 串口日志实时收集与分析 | Go | ✅ v0.4（`tools-go/cmd/serialmon`） |
| H-06 | 包管理器设计与主机侧原型（高效/安全/轻量、Windows 式灵活性参考） | Python（纯标准库） | 🟡 提案 v1 ✅ + **P0a 已落地**（`.ckc` 容器 `tools/pkgmgr/ckc.py`，`make test-pkg` 37 项）；事务/catalog 属 P0b |

### 1.3 非目标（见 architecture.md 第 5 节）

SMP、USB、Wi-Fi、GPU、完整 POSIX、完整动态链接 ELF、完整 TCP/IP、多架构。

---

## 2. 模块依赖关系

```text
boot ──→ arch(x86_64) ──→ interrupt ──→ time
              │               │
              ↓               ↓
           memory ──────→ process ──→ scheduler
              │               │
              ↓               ↓
            driver ─────→ syscall ──→ fs ──→ user(shell/apps)
                              │
                              ↓
                          network（最后）
```

依赖规则：

- 上游模块未验收，下游模块不得开工。
- `network` 依赖 `driver` + `syscall`，排在最后。
- `security` 依赖 `process` + `fs`，v1.0 才实现。

---

## 3. Cargo Workspace 规划

根 `Cargo.toml` 的 `members` 包含 `shared`/`boot`/`kernel`/`libc`/`user/*`，
`default-members` 保持 `shared`/`boot`/`kernel`，因此 `cargo build`（不带 `--target`）
不会连带构建用户程序（用户程序只能为目标 `x86_64-unknown-none` 构建，链接在 2^39）。

约定：

- `shared`：boot 与 kernel 共享的 `BootInfo` 等纯数据结构（`no_std`）。
- `kernel`：`#![no_std]`，目标 `x86_64-unknown-none`，链接脚本 `kernel/link.ld`（装载 32MiB）。
- `boot`：UEFI 引导 crate，目标 `x86_64-unknown-uefi`；与内核边界调用必须 `extern "sysv64"`（ADR-006）。
- `libc`：最小用户态运行时（syscall 封装 + `_start` + `print!/println!` + 堆 + `args`）。
- `user/*`：`no_std` 用户态程序，产出静态 ELF；当前可运行的有
  `user/hello`（→ RAMFS `/init`）、`user/shell`（→ `/bin/sh`）、`user/hello-bin`（→ `/bin/hello`，外部程序）；
  `init/ls/cat/echo/ps` 仍为占位（`cat` 先以 Shell 内建命令形式落地）。
- `tools/`（Python）与 `tools-go/`（Go）**不进 Cargo workspace**，各自独立管理（仅标准库）。
- 交叉编译在 Linux 与 Windows 主机上均通过 `rustup target add` 相同目标完成（见 docs/boot.md）。

用户程序专用 profile（workspace 根定义，large 代码模型与 LTO 不兼容）：

```toml
[profile.user-release]
inherits = "dev"
opt-level = 2
lto = false
debug = false
debug-assertions = false
overflow-checks = false
strip = true
panic = "abort"
```

---

## 4. 目录结构

见 [docs/project-structure.md](docs/project-structure.md)（完整目录树与模块职责表）。

---

## 5. 版本任务清单（v0.1 – v1.0）

### 5.1 里程碑总览

| 版本 | 目标 | 状态 |
| ---- | ---- | ---- |
| v0.1 | Bootloader + Rust Kernel | ✅ 2026-10-06 |
| v0.2 | GDT + IDT + Exception | ✅ 2026-10-06 |
| v0.3 | Physical/Virtual Memory + Heap | ✅ 2026-10-07 |
| v0.4 | Interrupt + Timer + Keyboard + H-04/H-05 | ✅ 2026-10-08 |
| v0.5 | Process + Thread + Scheduler + IPC（pipe） | ✅ 2026-10-08 |
| v0.6 | Syscall + User Space + 用户态脚手架 | ✅ 2026-10-10 |
| v0.7 | VFS + RAMFS + ELF Loader | ✅ 2026-10-11 |
| v0.8 | Driver + PCI + Disk（FAT 只读 + mkfs） | ✅ 2026-10-11 |
| v0.9 | Shell + User Programs + Network | 🟡 已落地三步 |
| v1.0 | 完整可启动类 Linux 小型 OS | ⬜ |

### 5.2 v0.1 – v0.5 完成记录（要点）

- **v0.1**：Cargo Workspace + 双目标（uefi/none）；UEFI 引导解析内存映射、装载内核 ELF、
  传 `BootInfo`、`ExitBootServices`；`kernel_main` + 串口；H-01 镜像构建 + H-02 QEMU 测试。
  验收：串口打印 `Kernel started!`。
- **v0.2**：自建 GDT + TSS（IST1 双错栈 / `RSP0`）+ ltr；IDT 32 个异常向量 + 汇编 stub
  （区分有/无错误码）+ 统一保存/恢复；异常诊断输出（向量名/错误码/RIP/CS/RFLAGS/RSP/SS/通用寄存器，
  #PF 带 CR2）。验收：`make test` 回归 + `make test-exception`（#DE）。
- **v0.3**：位图帧分配器（1 bit/帧，128KiB 覆盖 4GiB）、4 级页表（translate/map_4k/unmap_4k/TLB/`AddressSpace`）、
  内核堆（映像末尾预留 1MiB + 首次适配空闲链表 + 合并）。
  验收：`make test-memory`（含 `Box`/`Vec`/`String`、1024 帧压力、页表自测）+ `make test-pagefault`（#PF + CR2）。
- **v0.4**：PIC（8259A，IRQ→向量 32..47）+ IRQ 分发注册表；PIT 通道 0 频率 **100Hz（10ms/tick）** +
  心跳；PS/2 键盘（8042 初始化 + IRQ1 + set 1 解码）；上下文切换原语（callee-saved + 栈切换 + RFLAGS）；
  H-04/H-05 Go 工具（`go-test` / `test-orch`）。
  验收：`make test-keyboard`（QEMU monitor 注入真实按键 + PIT 心跳）。
- **v0.5**：线程控制块与状态机、PID 分配、定长环形就绪队列、Round Robin（200ms 时间片）、
  PIT/IRQ0 抢占式调度（pid 1/2/3 严格轮转并绕回 pid 0）；IPC 单向 pipe 骨架（64B 环形缓冲，非阻塞语义）。
  验收：`make test-scheduler` / `make test-ipc`。

### 5.3 v0.6 — Syscall + User Space ✅（2026-10-10 核心，2026-10-11 脚手架收尾）

- [x] 入口选型：`syscall`/`sysret` 快速路径（`EFER.SCE` + `IA32_STAR/LSTAR/FMASK`，GDT 按 STAR 布局）；入口切专用 syscall 栈
- [x] 寄存器约定：号 `rax`；参数 `rdi/rsi/rdx/r10/r8`；返回 `rax`；负数 = `-errno`
- [x] `syscall/mod.rs` 分发层 + `syscall_table.rs` 全号段表（未实现号落 `-ENOSYS` 桩）
- [x] 首批 syscall：`write`（串口，先校验用户缓冲指针）/ `getpid` / `nanosleep` / `exit`
- [x] 用户地址空间隔离：独立 PML4（用户映射固定下标 1 = `0x8000000000`，每级 `USER`），
      代码页 RO+X、栈页 RW+NX，`Drop` 整棵释放
- [x] ring3 异常终止语义：`exception_dispatch` 按 `cs & 3` 识别，用户态异常不 panic
- [x] 用户态执行期收敛：`scheduler::suspend()` + `TSS.RSP0` 切专用陷阱栈
- [x] 用户态脚手架：`libc/`（syscall 封装 + `_start` + `print!/println!` + `#[panic_handler]`）
      + `user/hello` 真实 Rust 用户程序；`kernel/build.rs` 构建期构建/校验/内嵌其 ELF；
      `UserSpace::load_program` 支持多代码页（1..=8）
- [x] 验收：`make test-user`（19 项 → v0.7 增至 32 → v0.9 增至 61）；`make user` 单独验证脚手架

### 5.4 v0.7 — VFS + RAMFS + ELF ✅（2026-10-11）

- [x] VFS（`fs/vfs.rs`）：`FileSystem` trait + 路径工具（`normalize`/`components`/`within`）+ 系统级入口
- [x] 挂载管理（`fs/mount.rs`）：**最长前缀路由**（`/mnt/*` 落到 `/mnt` 后端），日志 `fs: route ... fs=...`
- [x] Inode/元数据（`fs/inode.rs`）：`NodeKind`、`Metadata`、`DirEntry`
- [x] RAMFS（`fs/ramfs.rs`）：`Node { name, kind, data, children }`，单文件 ≤ 256 KiB；
      `fs::init` 建初始树 `/init` + `/etc/motd` + `/tmp`（v0.9 加 `/bin`）
- [x] fd 表与文件 syscall（`fs/file.rs` + `syscall/file.rs`）：0/1/2 保留、动态 fd ≥ 3（最小空闲槽位）、
      `read`/`open`/`close`，`write` 支持文件 fd；用户缓冲按页校验（`-EFAULT`），单次 I/O 上限 4096 字节
- [x] **ELF Loader**（`syscall/elf.rs`）：ELF64/`ET_EXEC`/`EM_X86_64` → 按段权限逐页映射
      （`W` → 可写、非 `X` → `NX`）→ 拷 `p_filesz`、余下置 0 → 入口 `e_entry`；畸形 ELF 打印拒绝原因
- [x] 用户程序改为从文件加载：`kernel/build.rs` 原样内嵌 ELF → RAMFS `/init` → `read_all` → `elf::load`
- [x] `user.ld` 四段分别 `ALIGN(0x1000)`（`W^X` 不退化）；`make user` 跑 `tools/check_user_elf.py`
- [x] 验收：`make test-fs`（19 项）+ `make test-user`（32 项）；编排新增 `fs` 场景（8 场景）

### 5.5 v0.8 — Driver + PCI + Disk ✅（2026-10-11）

- [x] 驱动注册框架（`driver/driver.rs`）：`Driver` trait（`name`/`matches`/`attach`）+ 定长注册表 +
      `probe_all()`（按注册顺序匹配、一设备一驱动、`attach` 失败不阻断）+ `ProbeStats`
- [x] PCI 枚举（`driver/pci.rs`）：配置机制 #1（`0xCF8`/`0xCFC`）、全总线扫描（多功能位修剪）、
      BAR 探测（写全 1 读掩码）与窗口大小、类别三元组解码
- [x] 块设备抽象（`driver/block.rs`）：`BlockDevice`（512B 扇区、`lba` 绝对寻址、长度非整扇区报错）+ 设备注册表
- [x] legacy virtio-blk 0.9.5（`driver/virtio_blk.rs`）：状态机 `ACKNOWLEDGE→DRIVER→FEATURES_OK→DRIVER_OK`、
      virtqueue（描述符表 + avail + 4KiB 对齐 used，三段物理连续）、描述符链「请求头→数据→状态字节」、
      notify 后轮询 used（PIT tick 超时）、页对齐 bounce buffer；modern（0x1042）明确拒绝
- [x] 启动盘改为 virtio-blk（`disable-modern=on`）：`make run` 与全部 QEMU 测试走同一条路径
- [x] 磁盘 FS：**FAT12/16/32 只读后端**（`fs/fat.rs`）按 BPB 簇数判类型、固定根目录区 / 簇链目录、
      8.3 + NT 大小写标志 + LFN（含校验和）、任意偏移读、空文件正确返回 0；写一律 `EROFS`；
      损坏结构 `EUCLEAN`；`fs::mount_disks` 挂 `/disk`（启动盘）与 `/mnt/fat`（测试盘）——**syscall 层零改动**
- [x] `FileSystem` trait 收窄为 `&mut self` 且 `read_all` 返回 `Vec<u8>`
- [x] H-03 `mkfs`（`tools/mkfs/mkfs.py`）：自己生成 FAT12/16/32 镜像（引导扇区/FSInfo/FAT 副本/LFN+8.3/簇链），
      回读校验；**mtools 独立复核**
- [x] 验收：`make test-driver`（22 项）+ `make test-fat`（21 项，双盘启动）；编排 9 → **10 场景**
- [x] 非 snapshot 启动后比对镜像 md5，确认写测试真的还原了最后一个扇区

**v0.8 明确未做**：FAT 后端只读（无写/簇分配器/扇区缓存）、不解析分区表、modern（virtio 1.0）、
MSI-X/INTx 中断、超过两块盘、BAR 重分配、ECAM/ACPI。

### 5.6 v0.9 — Shell + User Programs + Network 🟡（已落地三步）

#### 第一步：用户堆（2026-10-11）

- [x] `brk`（256 KiB 窗口，页对齐，全成或全不成，失败返回原值）
- [x] `mmap`（只收 `MAP_PRIVATE|MAP_ANONYMOUS`，5 参数无 offset，窗口 256 KiB，first-fit，新页全零；
      `PROT_WRITE|PROT_EXEC` → `EINVAL`（W^X）；`PROT_NONE`/文件映射 → `EINVAL`）
- [x] `munmap`（整段归还；清页表项 + `invlpg`；未对齐/未映射 → `EINVAL`）
- [x] `libc::heap`：`#[global_allocator]`（bump over `mmap`，64 KiB 块，`dealloc` 空操作）
- [x] 验收：`make test-user` 32 → **59 项**（零页、模式读回、堆缓冲 `write`、6 个 errno 负例、`Vec`/`format!`、
      页帧泄漏检查 `user: heap frame reclamation PASS`）

#### 第二步：`stdin` + 交互式 Shell（2026-10-11）

- [x] 键盘行缓冲（`driver/keyboard.rs`）：128 字节单行缓冲、行编辑（退格/回显）、`Enter` 交付整行 +
      `kbd: line ready len=N`（回显由 `KB <字符>` 改为终端式原样回显）
- [x] `read(0)` 行缓冲语义（`syscall/file.rs::read_stdin`）：整行交付（行尾 `\n`），
      `STDIN_IDLE_TICKS = 300`（3 秒）无整行 → `-EAGAIN`
- [x] `user/shell`（→ `/bin/sh`）：提示符 `root@cerlesse C:>`、参数切分、内建 `help`/`echo`/`cat`/`pid`/`exit`
- [x] `kernel/build.rs` 改为按列表构建/校验/内嵌多用户程序（`/init` + `/bin/sh`）
- [x] 验收：新增 `make test-shell`（QEMU monitor 注入真实按键，**11 项**）+ 编排 `shell` 场景；
      `make test-user` 59 → **61 项**；`make test-fs` 根目录条目 3 → 4

#### 第三步：进程对象 + `fork`/`execve`/`wait4`（2026-10-11）

- [x] 进程对象（`process/process.rs`）：真正的 PCB —— 地址空间 / 用户帧 / **每进程 64 KiB 内核栈**
      （栈底哨兵 `KERNEL STACK OVERFLOW`）/ `return_to` / `Ready|Running|Zombie`
- [x] `syscall/process.rs`：`fork`（复制地址空间 + 克隆当前用户帧）、`execve`（换映像 + 重建 argv 用户栈，
      先切 CR3 再 Drop 旧地址空间）、`wait4`（延迟调度 + 回收）
- [x] `trampoline` 改为按进程切换 CR3 / `TSS.RSP0` / syscall 栈顶 / 当前进程下标
- [x] `argc`/`argv`（`libc/src/args.rs`，入口寄存器 `rdi`/`rsi`；内核写在**新进程的用户栈**上）
- [x] 外部程序 `user/hello-bin` → RAMFS `/bin/hello`；Shell `cmd_external` 改走 `fork`+`execve`+`wait4`
      （`ls` → `/bin/ls` 不存在 → `execve` 返 `-ENOENT` → 子进程 `exit(127)`）
- [x] 验收：新增 `make test-proc`（**8 项 + 4 条正则**，含反向引用断言 fork/execve/wait4 三处 PID 相同、
      断言 `frames free before=N after=N` 无页帧泄漏）；编排新增 `proc` 场景（11 → **12 场景**）

#### v0.9 剩余

- [ ] 动态链接（`PT_INTERP`/`ld.so`）与 `mprotect`（W^X 转换）；文件映射（`mmap` 加 offset）
- [ ] 其余用户程序（ls/cp/mv/rm/mkdir/ps/kill）与 Shell 增强（`cd`、管道、重定向）
- [ ] 简化网络：virtio-net → Ethernet → ARP → IPv4 → UDP（TCP 尽力）
- [ ] Socket syscall 最小集

### 5.7 v1.0 — 完整可启动类 Linux 小型 OS

- [ ] 用户/组/权限（`-rwxr-xr-x`、UID/GID）
- [ ] 进程隔离与 syscall 参数校验强化
- [ ] 启动链：Kernel → init → Shell → User Programs
- [ ] 文档全量修订、发布 v1.0 tag
- [ ] 验收：QEMU 一键启动到可交互 Shell

### 5.8 缺陷与修复汇总

| 编号 | 版本 | 级别 | 问题 | 修复 |
| ---- | ---- | ---- | ---- | ---- |
| B-01 | v0.1 | 高 | `_start`→`kernel_main` 内 `cli` 前存在中断窗口（IDT 仍是固件遗留） | `_start` 第一行 `cli`，入口即刻关中断 |
| B-02 | v0.1 | 中 | 内核无自有 GDT/IDT，异常直接 triple fault | v0.2 自建 GDT/IDT + 异常诊断 |
| B-03 | v0.1 | 低 | 异常处理无自动化回归 | 新增 `make test-exception` |
| B-04 | v0.1 | 低 | 启动日志含一次性低地址内存诊断 | 保留（<64MiB，用于装载冲突诊断） |
| B-05 | v0.2 | 高 | 内核默认链接为 PIE，绝对数据依赖 `.rela.dyn`；bootloader 不处理动态重定位 → handler 地址错误 | 强制 `relocation-model=static`（ADR-008） |
| B-06 | v0.3 | 中 | 异常名表自向量 9 起错位一位 | 按 Intel SDM Table 6-1 重建 32 项名表 + `test-pagefault` |
| B-07 | v0.3 | 中 | 堆分配器把对齐前导 padding 计入块大小 → 越界覆盖空洞头 | 块大小改为 `want - pad` + 填充模式校验 |
| B-08 | v0.4 | 高 | IRQ 向量 33..47 走入异常分发 → IRQ1 触发即 panic | 16 个 IRQ stub 统一 `jmp irq_common` |
| B-09 | v0.4 | 高 | IRQ 路径未保存通用寄存器 → 被打断代码带破坏寄存器继续，随机 #GP | `irq_common` 保存/恢复 15 个通用寄存器 |
| B-10 | v0.5 | 高 | 上下文切换未保存 `RFLAGS`（切出后 IF 永久为 0） | `context_switch` 加 `pushfq`/`popfq`；新上下文 `RFLAGS=0x202` |
| B-11 | v0.5 | 高 | IRQ EOI 在回调之后（IRQ0 回调触发切换后 PIC 停摆） | 改为「先 EOI，后分发」 |
| B-12 | v0.5 | 中 | 启动上下文切出后未回就绪队列，Round Robin 绕不回 pid 0 | `start()` 切出前把 pid 0 压回队尾 |
| B-13 | v0.6 | 高 | 创建用户地址空间时无条件重写整棵内核页表清 `USER`，OVMF 页表页只读 → 启动 #PF | `scrub_user_bits` 惰性：仅当 `USER` 确为 1 时写回 |
| B-14 | v0.6 | 高 | `syscall` 返回路径栈算术错误（`add rsp,48`）→ `sysretq` 跳飞 `RIP=0x3` | 改为 `sub rsp,16` 后 `pop r11`/`pop rcx` |
| B-15 | v0.7 | 高 | syscall 入口未恢复 caller-saved 寄存器 → 用户态 `r9` 被破坏，#PF | 入口保存/恢复 `rdi/rsi/rdx/r10/r8/r9` |
| B-16 | v0.8 | 中 | BAR 窗口大小按 64 位取反，I/O BAR 报出错误尺寸 | 大小在该字**自身位宽**里求 `~mask + 1` |
| B-17 | v0.8 | 高 | `lba + sectors` 用 `u64` 加法，`lba = MAX-1` 环绕后被误判在容量内 | 改用 `checked_add`，溢出即报错 |

v0.9 记录的两条踩坑（设计记录，非编号缺陷）：debug 构建下 16 KiB 内核栈静默溢出踩坏 `.bss` 帧分配器；
子进程退出时必须**先恢复 CR3 再 Drop 地址空间**。详见 [docs/process.md](docs/process.md) 1.4 与
[docs/syscall.md](docs/syscall.md) 3.7。

### 5.9 关键设计决策（ADR）

见 [docs/architecture.md](docs/architecture.md) 第 6 节（ADR-001 … ADR-011）。新增重大决策时追加编号并同步本文。

---

## 6. 测试方案

| 层级 | 内容 | 工具 | 版本 |
| ---- | ---- | ---- | ---- |
| 单元测试 | 纯逻辑模块（分配器算法、ELF 解析、路径解析）host 上 `cargo test`（**计划**，暂以内核内自测覆盖） | Rust | v0.4 起规划 |
| 集成测试 | QEMU 启动 → 断言串口日志包含预期输出 | Python (H-02) | v0.1 |
| 压力测试 | 帧分配、任务轮转、fork 风暴 | Python + Go 编排 | v0.3/v0.5 |
| 回归测试 | 每个里程碑的验收标准固化为测试用例 | Go 编排服务 | v0.4 |
| 异常测试 | 故意触发 #DE/#PF，校验诊断输出 | Python | v0.2/v0.3 |
| 驱动/块设备 | PCI 枚举 → 驱动挂接 → 扇区读写/分块一致性/负例 | Python | v0.8 |
| 磁盘文件系统 | 双盘启动 → FAT16 启动盘读内核映像 / FAT32 测试盘读·LFN·跨簇·偏移·负例 | Python | v0.8 |
| 包容器（不启 QEMU） | `.ckc` 确定性、头部逐字段、payload 篡改、ustar 规则、打包期规则 + `tar` 独立复核 | Python | H-06 P0a |
| 键盘/交互/进程 | QEMU monitor `sendkey` 注入真实按键 → 行缓冲/Shell/进程调用全流程（`--expect-re` 支持正则 + 反向引用） | Python | v0.4，v0.9 扩展 |

约定：

- 每个版本任务清单里的「验收」必须落成自动化测试。
- 串口是唯一权威日志通道，测试断言只依赖串口输出。
- 目标与项数见 [docs/api.md](docs/api.md) 第 7 节。

---

## 7. QEMU 调试方案

```bash
make run
# 底层等价于：
qemu-system-x86_64 -machine q35 -m 512M -serial stdio -display none \
  -drive if=pflash,format=raw,readonly=on,file=$OVMF_CODE \
  -drive if=pflash,format=raw,file=build/OVMF_VARS.fd \
  -drive if=none,id=blk0,format=raw,file=build/disk.img \
  -device virtio-blk-pci,drive=blk0,disable-modern=on
```

- `disable-modern=on` 必需：内核只实现 legacy virtio（见 docs/drivers.md 1.2）。
- 测试脚本额外加 `snapshot=on`（临时写时复制），使并行场景共享镜像且不污染原文件。
- 调试：`-s -S` + `gdb target remote :1234`；崩溃用 `-no-reboot -d int,cpu_reset`；
  内核 ELF 保留符号，`add-symbol-file kernel.elf` 后可按函数名下断点。
- 更完整的开发/调试流程见 [docs/software-development.md](docs/software-development.md) 第 5 节。

---

## 8. GitHub Issue / 里程碑规划

里程碑与版本一一对应（Milestone 标签 `v0.1` … `v1.0`），Issue 标签分类：

| 标签 | 用途 |
| ---- | ---- |
| `boot` / `memory` / `interrupt` / `process` / `syscall` / `fs` / `driver` / `network` / `security` | 模块归属 |
| `phase:bug` / `phase:feature` / `phase:refactor` / `phase:doc` | 类型 |
| `host:python` / `host:go` | 主机侧工具 |
| `blocked:upstream` | 被依赖项阻塞 |

规划规则：

1. 每个里程碑开工时，把第 5 节对应版本的 checklist 拆成 Issues（一个 checklist 项 ≈ 1 个 Issue）。
2. Issue 标题格式：`[v0.x] 模块: 简述`，例如 `[v0.3] memory: 实现 bitmap FrameAllocator`。
3. 里程碑关闭条件：任务清单全部勾选 + 验收测试通过 + 对应阶段文档更新。

---

## 9. 文档维护约定

1. 每份文档顶部保留 `阶段` / `最后更新` / `状态` 三行。
2. **设计变更先改文档再改代码**（文档即规格）；未开始的阶段保留占位大纲，
   但阶段内已落地部分应及时补记，避免文档与代码库状态脱节。
3. 新增/调整文档时同步 [docs/README.md](docs/README.md) 索引与相关交叉链接；
   **对外接口变化必须同步 [docs/api.md](docs/api.md)**。
4. 文档会被反复修改：每次修订在该文档的「修订记录」补一行（日期 + 变更点）。
5. 面向开发者的流程/规范统一收敛到 [docs/software-development.md](docs/software-development.md)。

---

## 10. 修订记录

> 本表按日期从旧到新排列；每轮迭代的变更点在此追加（不删除历史）。

| 日期 | 变更 |
| ---- | ---- |
| 2026-10-06 | 第一阶段初稿：需求、依赖、workspace 规划、v0.1–v1.0 任务清单、测试/QEMU/Issue 规划 |
| 2026-10-07 | v0.3 完成：内存管理（帧分配器/页表 Mapper/内核堆） |
| 2026-10-08 | v0.4 完成：PIC/PIT 100Hz/PS-2 键盘/上下文切换原语；H-04/H-05 Go 工具 |
| 2026-10-08 | v0.5 完成：线程模型 + Round Robin 时间片调度 + IPC pipe 骨架 |
| 2026-10-10 | v0.6 核心落地：`syscall`/`sysret` 入口 + ring3 + 用户地址空间隔离 + 首批 syscall |
| 2026-10-11 | v0.6 用户态脚手架可用：`libc/` + `user/hello` + `kernel/build.rs` 内嵌；`make user` |
| 2026-10-11 | v0.7 落地：VFS + RAMFS + fd 表 + 文件 syscall + ELF Loader；`make test-fs` |
| 2026-10-11 | v0.8 前半：驱动框架 + PCI 枚举 + 块设备 + legacy virtio-blk；`make test-driver` |
| 2026-10-11 | v0.8 收官：FAT12/16/32 只读后端挂到 VFS + H-03 `mkfs`；`make test-fat` |
| 2026-10-11 | v0.9 第一步：用户堆（`brk`/`mmap`/`munmap` + `libc` 全局分配器） |
| 2026-10-11 | v0.9 第二步：`stdin` + 交互式 Shell（`/bin/sh`）；`make test-shell` |
| 2026-10-11 | v0.9 第三步：进程对象 + `fork`/`execve`/`wait4` + `argc`/`argv`；`make test-proc` |
| 2026-10-11 | H-06 P0a：`.ckc` 包容器（CKC）落地；`make test-pkg` 37 项 |
| 2026-10-11 | **文档重构**：README 改为项目入口页；本文按「需求/依赖/workspace/目录/版本任务/缺陷汇总/测试/QEMU/Issue/文档约定/修订」重组；新增 [docs/api.md](docs/api.md) 与 [docs/software-development.md](docs/software-development.md)，并同步 [docs/README.md](docs/README.md) 索引 |
