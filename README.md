# Cereals-Kernel-Cerlesse

> 代号 **Cerlesse OS** — 使用 Rust 从零开发的类 Linux 高性能宏内核。
> 内核核心：**Rust**；主机侧工具：**Python**（构建/测试）+ **Go**（编排/日志服务）。
> 支持 Linux / Windows 主机交叉开发，目标平台 x86_64（UEFI + QEMU 优先）。

`v0.9 进行中` · `v0.1 – v0.8 已完成` · `13 个集成测试目标全绿`

---

## 目录

- [项目简介](#项目简介)
- [架构总览](#架构总览)
- [当前状态](#当前状态)
- [已实现能力](#已实现能力)
- [快速开始](#快速开始)
- [运行示例](#运行示例)
- [项目结构](#项目结构)
- [常用命令](#常用命令)
- [文档入口](#文档入口)
- [参与贡献](#参与贡献)
- [许可](#许可)

## 项目简介

Cerlesse 不做「完整 Linux 替代品」，而是按 **Boot → 内存 → 中断 → 进程 → 调度 →
系统调用 → 文件系统 → 驱动 → 用户态 → Shell → 网络** 逐层构建，最终得到一个
可一键启动到交互式 Shell 的小型操作系统。

设计原则：

- **宏内核（monolithic）+ `no_std` Rust**：所有进入内核运行时的代码都是 Rust；
  Python / Go 只出现在开发主机侧（构建、镜像、测试编排、串口分析）。
- **文档即规格**：设计先写进文档再落代码；每个版本任务清单的「验收」都必须落成自动化测试。
- **串口是唯一权威日志通道**：所有集成测试只断言串口输出，可在 CI/QEMU 中无人值守复现。

## 架构总览

```text
                    User Applications
                           │
                           ↓
                    System Call API          ← syscall/sysret，ring3 隔离，Linux 号段语义
                           │
                           ↓
┌───────────────────────────────────────────────┐
│                 Rust Kernel (no_std)           │
│                                               │
│  Process   Memory    Scheduler   IPC (pipe)   │
│  VFS       Drivers   Syscall     ELF Loader   │
│  Network   Security  Time        Interrupt    │
├───────────────────────────────────────────────┤
│              Hardware Abstraction             │
├───────────────────────────────────────────────┤
│                 Hardware (x86_64)             │
└───────────────────────────────────────────────┘
```

语言分工与 ADR 见 [docs/architecture.md](docs/architecture.md) 第 2、6 节。

## 当前状态

**v0.1 – v0.8 已全部完成并通过自动化验收；v0.9 已落地三步，剩余动态链接与网络。**

| 版本 | 状态 | 交付内容摘要 |
| ---- | ---- | ---- |
| v0.1 | ✅ | UEFI 引导 + Rust 内核入口 + 串口输出 + 镜像/QEMU 测试（H-01/H-02） |
| v0.2 | ✅ | GDT/IDT + 32 个异常向量 + 异常诊断 |
| v0.3 | ✅ | 位图帧分配器 + 4 级页表 Mapper + 内核堆 |
| v0.4 | ✅ | PIC + PIT 100Hz + PS/2 键盘 + 上下文切换原语 + H-04/H-05 Go 工具 |
| v0.5 | ✅ | 进程/线程模型 + Round Robin 调度 + IPC（pipe 骨架） |
| v0.6 | ✅ | `syscall`/`sysret` 入口 + ring3 隔离 + 用户态脚手架（`libc`） |
| v0.7 | ✅ | VFS + RAMFS + fd 表 + 文件 syscall + ELF Loader |
| v0.8 | ✅ | 驱动注册框架 + PCI 枚举 + 块设备 + legacy virtio-blk + FAT12/16/32 只读 + `mkfs` |
| v0.9 | 🟡 | 用户堆 → `stdin` + 交互式 Shell → 进程对象 + `fork`/`execve`/`wait4`；**剩余：动态链接、网络** |
| v1.0 | ⬜ | 权限/凭证 + 完整启动链 + 发布 |

**v0.9 已落地的三步（外部程序真的能跑）**：

1. **用户堆**：`brk`/`mmap`/`munmap`（W^X、新页全零、`invlpg`）+ `libc` 的 `#[global_allocator]`，
   用户程序可以用 `Vec`/`String`/`Box`；
2. **`stdin` + 交互式 Shell**：键盘行缓冲（行编辑 + 终端式回显）→ `read(0)` 整行交付
   （3 秒无整行 `-EAGAIN`）→ `/bin/sh`（提示符 `root@cerlesse C:>`，内建 `help`/`echo`/`cat`/`pid`/`exit`）；
3. **进程对象 + `fork`/`execve`/`wait4` + `argc`/`argv`**：每进程独立地址空间 +
   每进程 64 KiB 内核栈（栈底哨兵）+ 停在 syscall 里的完整用户帧；Shell 的外部命令
   `/bin/hello` 由 `fork`+`execve`+`wait4` 真正跑起来（找不到的程序 → `ENOENT` → `exit(127)`）。

详细的实现记录、踩坑与验收证据见 [DEVELOPMENT.md](DEVELOPMENT.md) 第 5、9 节。

## 已实现能力

- **引导与基础**：UEFI 引导、BootInfo、GDT/IDT、异常诊断（#PF 带 CR2）、串口控制台。
- **内存**：位图帧分配器、4 级页表与 `AddressSpace`、内核堆 `#[global_allocator]`、
  `allocate_contiguous`（virtqueue/DMA 前置）。
- **中断与时钟**：PIC（8259A）与 IRQ 分发表、PIT 100Hz 心跳、PS/2 键盘行缓冲。
- **进程与调度**：进程对象（PCB：地址空间/用户帧/内核栈/状态机）、Round Robin 200ms 时间片、pipe。
- **系统调用与用户态**：`syscall`/`sysret` 快速路径、独立用户 PML4（每级 `USER`、代码 RO+X、栈 RW+NX）、
  ring3 异常终止而不 panic；`read`/`write`/`open`/`close`/`mmap`/`munmap`/`brk`/`getpid`/`nanosleep`/
  `fork`/`execve`/`exit`/`wait4`（`mprotect` 待实现）。
- **文件系统**：VFS（`FileSystem` trait + 最长前缀挂载路由）、RAMFS、fd 表、
  磁盘 FAT12/16/32 **只读**后端（8.3 + NT 标志 + LFN、任意偏移读、`EROFS`/`EUCLEAN`）。
- **驱动**：驱动注册框架（`Driver` trait + `probe_all` + `ProbeStats`）、PCI 枚举（机制 #1 + BAR 探测）、
  块设备抽象、legacy virtio-blk 0.9.5（virtqueue + 轮询 + bounce buffer）。
- **用户程序**：`libc` 最小运行时（syscall 封装 / `_start` / `print!` / panic handler / 堆 / `argc`-`argv`）；
  `user/hello`（`/init`）、`user/shell`（`/bin/sh`）、`user/hello-bin`（外部程序 `/bin/hello`）。
- **主机侧工具**：镜像构建、ELF 段布局检查、`mkfs`（FAT 镜像，mtools 独立复核）、
  CKC 包容器 `.ckc`（H-06 P0a）、Go 测试编排器 `testorch` 与串口分析 `serialmon`。

## 快速开始

### 前置条件

| 组件 | 要求 |
| ---- | ---- |
| Rust（stable，`rustup`） | 目标 `x86_64-unknown-none` 与 `x86_64-unknown-uefi` |
| QEMU | `qemu-system-x86_64`（机器 `q35`，512M） |
| OVMF | `OVMF_CODE.fd` / `OVMF_VARS.fd`（默认 `/usr/share/OVMF/`） |
| GNU Make + Python 3 | 构建入口与主机侧工具（仅标准库） |
| 可选：mtools（`mdir`/`mtype`） | FAT 镜像独立复核（`make test-fat`） |
| 可选：Go | `testorch` / `serialmon`（`make go-test` / `make test-orch`） |

```bash
rustup target add x86_64-unknown-none x86_64-unknown-uefi
# Debian/Ubuntu 例：sudo apt-get install qemu-system-x86 ovmf mtools make python3 golang
```

需要自定义路径时通过 Makefile 变量覆盖：

```bash
make run OVMF_CODE=/path/OVMF_CODE.fd OVMF_VARS=/path/OVMF_VARS.fd QEMU=/path/qemu-system-x86_64
```

### 构建与运行

```bash
make build      # 构建 boot + kernel + 磁盘镜像（build/disk.img）
make run        # QEMU + OVMF 启动（串口输出到终端）
make test       # 冒烟测试：断言串口出现 "Kernel started!"
make test-all   # 全部 13 个集成测试目标
```

## 运行示例

启动后在串口依次可观察到（真实日志片段）：

```text
Kernel started!
driver: match 00:03.0 -> virtio-blk-pci
driver: attached 00:03.0 -> virtio-blk-pci
block: device=virtio-blk0 sector_size=512 sectors=131072 bytes=67108864
block: chunked read PASS (512/4096/4608 bytes consistent at lba 0)
fs: mount point=/ fs=ramfs
fs: mount point=/disk fs=fat
fs: read /etc/motd len=23 data=motd: hello from ramfs
user: program=/init source=elf
hello from user space
...
IRQ0_heartbeat tick=100
root@cerlesse C:> help
root@cerlesse C:> hello world
hello-bin: Hello from an external program!
hello-bin: pid=3 argc=2
hello-bin: argv[0]=hello
hello-bin: argv[1]=world
sh: hello: exit code=1
root@cerlesse C:>
```

## 项目结构

```text
boot/      UEFI 引导（v0.1 ✅）        kernel/   Rust 内核主体（v0.8 ✅，含内存/中断/调度/
libc/      最小用户态运行时（v0.9 ✅）            syscall/fs/驱动/进程）
shared/    boot/kernel 共享结构         user/     用户程序（hello ✅ / shell ✅ / hello-bin ✅）
tools/     Python 主机工具              tools-go/ Go 主机服务
tests/     QEMU 集成测试                docs/     全部开发文档
```

驱动与文件系统的分层：

```text
pci.rs ──枚举──▶ driver.rs ──匹配/挂接──▶ virtio_blk.rs
（总线上有什么）   （谁来管它）              （实现 Driver + BlockDevice）
                        │
                        └──▶ block.rs（按扇区读写的统一接口 + 设备注册表）
                                  │
磁盘上的文件：                     ▼
fs/fat.rs（FAT12/16/32 只读）──▶ fs/vfs.rs + 挂载表 ──▶ open/read/close（syscall 层零改动）
```

完整目录树与模块职责见 [docs/project-structure.md](docs/project-structure.md)。

## 常用命令

| 命令 | 说明 |
| ---- | ---- |
| `make build` / `make run` | 构建镜像 / QEMU 启动 |
| `make test` | 冒烟测试（断言 `Kernel started!`） |
| `make test-exception` | 注入 #DE，断言异常诊断 |
| `make test-memory` | 帧 / 内核堆 / 页表 + 上下文切换自测 |
| `make test-pagefault` | 访问未映射地址，断言 #PF 诊断 + CR2 |
| `make test-keyboard` | 经 QEMU monitor 注入按键，断言 IRQ1 行缓冲 + PIT 心跳 |
| `make test-scheduler` | Round Robin 轮转 |
| `make test-ipc` | pipe 创建/写入/读取 |
| `make test-fs` | VFS 挂载路由 + RAMFS 读/写/ls + 负例 errno（20 项） |
| `make test-user` | ELF 装载 + 文件 syscall + 用户堆 + 隔离 + Shell（61 项） |
| `make test-shell` | 注入按键跑交互式 Shell（11 项） |
| `make test-proc` | `fork`/`execve`/`wait4` 全链路（8 项 + 4 正则） |
| `make test-driver` | PCI 枚举 + 驱动挂接 + virtio-blk 扇区读写/负例（22 项） |
| `make fat-image` / `make test-fat` | 生成 FAT32 测试盘 / 双盘 FAT16·32 读测（21 项） |
| `make user` | 单独构建并校验三个用户程序（不启 QEMU） |
| `make test-pkg` | CKC `.ckc` 包容器规则（37 项，不启 QEMU） |
| `make test-all` | 全部集成测试 |
| `make go-test` / `make test-orch` | Go 单元测试 / 编排器并行跑全部场景 |

每个目标断言的内容与项数见 [docs/api.md](docs/api.md) 第 7 节。

## 文档入口

| 文档 | 内容 |
| ---- | ---- |
| [docs/README.md](docs/README.md) | 文档中心与阶段状态 |
| [DEVELOPMENT.md](DEVELOPMENT.md) | 主开发文档：需求、依赖、版本任务清单、测试/QEMU/Issue 规划 |
| [docs/api.md](docs/api.md) | **API 参考**：系统调用 ABI、errno、`libc` API、内核 trait、工具 CLI、Make 目标 |
| [docs/software-development.md](docs/software-development.md) | **软件开发文档**：环境、构建/测试/调试、规范、协作、质量门禁、扩展步骤 |
| [docs/architecture.md](docs/architecture.md) | 内核架构、语言分工、ADR |
| [docs/roadmap.md](docs/roadmap.md) | 开发路线与 v0.1–v1.0 里程碑 |
| [docs/project-structure.md](docs/project-structure.md) | 目录结构与模块职责 |
| [docs/boot.md](docs/boot.md) · [memory.md](docs/memory.md) · [drivers.md](docs/drivers.md) · [process.md](docs/process.md) · [syscall.md](docs/syscall.md) · [filesystem.md](docs/filesystem.md) | 各主题阶段文档（实现记录 + 设计取舍 + 踩坑） |
| [docs/package-manager.md](docs/package-manager.md) | H-06 包管理器设计提案与 `.ckc` 容器规格 |

## 参与贡献

1. 阅读 [docs/software-development.md](docs/software-development.md)（环境、规范、协作流程）与
   [docs/architecture.md](docs/architecture.md)（设计决策）。
2. 从 [DEVELOPMENT.md](DEVELOPMENT.md) 第 5 节的版本任务清单领取条目，按
   [docs/software-development.md](docs/software-development.md) 第 10 节的方式扩展。
3. 提交前确保对应 `make test-*` 目标通过（质量门禁见软件开发文档第 9 节），并同步相关文档。

## 许可

本仓库当前**尚未包含 LICENSE 文件**；在补充明确的许可协议前，默认保留所有权利。
如需开源，请由项目作者添加 `LICENSE` 并在本节注明。

---

下一步：按 [DEVELOPMENT.md](DEVELOPMENT.md) 第 5 节顺序继续 —— 完成 v0.9（动态链接 + 网络）。
