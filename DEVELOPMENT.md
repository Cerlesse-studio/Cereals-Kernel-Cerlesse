# Cerlesse OS 主开发文档（DEVELOPMENT）

- 阶段：第一阶段
- 最后更新：2026-10-11
- 状态：v0.1–v0.5 全部完成（v0.4：中断/Timer/键盘/上下文切换 + H-04/H-05 Go 工具，`make go-test` / `make test-orch` 通过；v0.5：线程模型 + Round Robin 时间片调度 + IPC pipe 骨架，`make test-scheduler` / `make test-ipc` 通过）；
  v0.6 核心已落地并通过 `make test-user` 验收（syscall/sysret 入口 + ring3 + 用户地址空间隔离），
  并且**用户态脚手架已可用**（`libc/` 最小运行时 + `user/hello` 真实 Rust 用户程序 + `kernel/build.rs` 构建期摊平内嵌，`make user` 可单独验证）；
  **v0.7 已落地并通过 `make test-fs`（20 项）+ `make test-user`（32 项）验收**：
  VFS（trait + 挂载表最长前缀路由）、RAMFS、fd 表、`read`/`write`/`open`/`close`，
  以及 **ELF Loader**（用户程序不再是摊平映像，而是 RAMFS 上的 `/init` 经 `elf.rs` 按段权限装载，
  `RW` 段对 ring3 可写）；`make user` 追加 `tools/check_user_elf.py` 独立校验段布局；
  **v0.8 已完成并通过 `make test-driver`（22 项）+ `make test-fat`（21 项）验收**：驱动注册框架（`Driver` trait + 注册表 + `probe_all`）、
  PCI 枚举（配置机制 #1 + BAR 探测 + 窗口大小）、块设备抽象（按 512 字节扇区读写）、
  **legacy virtio-blk 0.9.5**（virtqueue + 同步轮询 + bounce buffer）、`frame::allocate_contiguous`（DMA 前置条件）、
  **FAT12/16/32 只读后端挂到 VFS**（`/disk` = 启动盘、`/mnt/fat` = 测试盘，syscall 层零改动）与 H-03 `mkfs`
  （`tools/mkfs/mkfs.py`，mtools 独立复核）；启动盘已改为 virtio-blk（`disable-modern=on`）；
  **v0.9 已开局**（2026-10-11，运行时前置条件第一步：**用户堆**）—— `brk`/`mmap`/`munmap`
  （W^X、全零页、`invlpg`、Linux 式 `brk` 语义）+ `libc` 的 `#[global_allocator]`（`Vec` 可用）；
  同日**第二步：交互式 Shell 与 `stdin`** —— 键盘行缓冲（行编辑 + 终端式回显）→
  `read(0)` 整行交付 → `user/shell`（提示符 `root@cerlesse C:>`、内建 `help`/`echo`/`cat`/`pid`/`exit`），
  内嵌为 RAMFS 上的 `/bin/sh`，新增 `make test-shell`（11 项）；
  同日**第三步：进程对象 + `fork`/`execve`/`wait4`** —— 每进程地址空间 + 每进程 64 KiB 内核栈
  （栈底哨兵）+ 停在 syscall 里的完整用户帧；Shell 的外部命令真的跑起来了
  （`/bin/hello`，新增 `make test-proc`，8 项 + 4 条正则）；
  `make test-user` 由 32 项增至 **61 项**；`argc/argv` 一并可用；动态链接/网络仍为占位
- 关联文档：[docs/architecture.md](docs/architecture.md) · [docs/roadmap.md](docs/roadmap.md) · [docs/project-structure.md](docs/project-structure.md) · [docs/drivers.md](docs/drivers.md) · [docs/syscall.md](docs/syscall.md) · [docs/filesystem.md](docs/filesystem.md) · [docs/process.md](docs/process.md)

---

## 1. 功能需求

### 1.1 核心需求（Rust 内核）

| 编号 | 需求 | 版本 |
| ---- | ---- | ---- |
| R-01 | UEFI 引导进入 Rust 内核入口，串口输出 | v0.1 |
| R-02 | GDT/IDT 与异常处理（#PF/#GP/#DF 等） | v0.2 |
| R-03 | 物理帧分配 + 虚拟内存映射 + 内核堆分配器 | v0.3 |
| R-04 | 中断控制器（PIC/APIC）、Timer、键盘输入 | v0.4 |
| R-05 | 进程/线程模型、上下文切换、Round Robin 调度 | v0.5 |
| R-06 | 系统调用入口与首批 10 个 syscall、用户态隔离 | v0.6 |
| R-07 | VFS + RAMFS + ELF Loader | v0.7 |
| R-08 | 驱动框架、PCI 枚举、磁盘驱动 | v0.8 |
| R-09 | Shell、基础用户程序、简化网络栈 | v0.9 |
| R-10 | 权限/凭证、可完整启动的类 Linux 小型 OS | v1.0 |

### 1.2 主机侧需求（Python / Go）

| 编号 | 需求 | 语言 | 版本 |
| ---- | ---- | ---- | ---- |
| H-01 | 镜像构建脚本（组装 boot + kernel + FS 镜像） | Python | v0.1 |
| H-02 | QEMU 自动化：启动、超时、串口日志断言 | Python | v0.1 |
| H-03 | mkfs 工具（FAT12/16/32 镜像生成，`tools/mkfs/mkfs.py`，mtools 复核） | Python | v0.8 ✅ |
| H-04 | 测试编排服务（并行多场景跑 QEMU 测试） | Go | v0.4 |
| H-05 | 串口日志实时收集与分析 | Go | v0.4 |
| H-06 | 包管理器设计与主机侧原型（高效/安全/轻量、Windows 式灵活性参考） | Python（纯标准库，见 docs/package-manager.md 第 11 节） | 设计阶段（**提案 v1 ✅** + **P0a 已落地**：`.ckc` 包容器 `tools/pkgmgr/ckc.py`，`make test-pkg` 37 项 PASS；事务/catalog 属 P0b） |

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

## 3. Cargo Workspace 规划（v0.1 初始化）

```toml
# 根 Cargo.toml（v0.1 落地；v0.6 加入 libc 与 user/*，并新增 user-release profile）
[workspace]
resolver = "2"
members = ["shared", "boot", "kernel", "libc", "user/hello"]
# 用户程序只能为目标 x86_64-unknown-none 构建（链接在 2^39），
# 因此 `cargo build`（不带 --target）仍只构建前三者。
default-members = ["shared", "boot", "kernel"]

[profile.dev]
panic = "abort"

[profile.release]
opt-level = 2
lto = true
panic = "abort"

# 用户程序专用：large 代码模型与 LTO 不兼容（见 libc/README.md 第 3 节）
[profile.user-release]
inherits = "dev"
opt-level = 2
lto = false
```

约定：

- `shared`：boot 与 kernel 共享的 `BootInfo` 等纯数据结构（no_std）。
- `kernel`：`#![no_std]`，目标 `x86_64-unknown-none`，链接脚本 `kernel/link.ld`（32MiB）。
- `boot`：UEFI 引导 crate，目标 `x86_64-unknown-uefi`；与内核边界调用必须 `extern "sysv64"`（ADR-006）。
- `libc`：最小用户态运行时（syscall 封装 + `_start` + `print!/println!`，v0.6 起加入 workspace）。
- `user/*`：`no_std` 用户态程序，产出静态 ELF（v0.6 起加入 workspace）；
  当前 `user/hello`（→ RAMFS `/init`）与 `user/shell`（→ RAMFS `/bin/sh`）为可运行的真实程序，
  `init/ls/cat/echo/ps` 仍是 v0.9 占位（`cat` 先以 Shell 内建命令形式落地）。
- `tools/`（Python）与 `tools-go/`（Go）**不进 Cargo workspace**，各自独立管理。
- 交叉编译在 Linux 与 Windows 主机上均通过 `rustup target add` 相同目标完成（见 docs/boot.md）。

---

## 4. 目录结构

见 [docs/project-structure.md](docs/project-structure.md)（已同步创建占位文件框架）。

---

## 5. 版本任务清单（v0.1 – v1.0）

### v0.1 — Bootloader + Rust Kernel ✅（2026-10-06 完成）
- [x] 初始化 Cargo Workspace 与 target 配置（shared/boot/kernel；uefi + none 双目标）
- [x] UEFI Bootloader：解析内存映射、页分配、加载 ELF、BootInfo、ExitBootServices
- [x] `kernel_main` 入口 + Serial Console 输出（自建栈，sysv64 边界调用）
- [x] Python 镜像构建脚本（H-01，mtools 组装 64MiB FAT）与 QEMU 启动测试（H-02）
- [x] 验收：QEMU 打印 "Kernel started!"（`make test` 断言通过）
- [x] 文档完成：[docs/boot.md](docs/boot.md)（v0.1 实现记录、踩坑记录 ADR-006/007/008）

### v0.2 — GDT + IDT + Exception ✅（2026-10-06 完成）
- [x] 自建 GDT（null/kcode/kdata）+ TSS（IST1 专用双错栈、RSP0 预留）+ ltr
- [x] IDT 32 个异常向量，汇编 stub（区分有/无错误码）+ 统一保存/恢复（`exception_dispatch`）
- [x] 异常 handler 诊断输出：向量名、错误码、RIP/CS/RFLAGS/RSP/SS、通用寄存器、#PF 时 CR2
- [x] 验收（自动化）：`make test` 回归 PASS；`make test-exception` PASS（串口含 `EXCEPTION: divide error` + 完整寄存器转储 + `KERNEL PANIC: unhandled exception`）
- [x] 文档完成：[docs/boot.md](docs/boot.md)（v0.1/v0.2 实现记录、B-01/B-05/B-06 修复）

### v0.1 缺陷复查与修复（2026-10-06，随 v0.2 一并落地）

| # | 级别 | 问题 | 修复措施 |
| ---- | ---- | ---- | ---- |
| B-01 | 高 | `_start` 到 `kernel_main` 内 `cli` 之间存在中断窗口：ExitBootServices 后 IF 仍为 1，而 IDT 还是固件遗留的，中断可能跳进已失效的固件代码 | `_start` 汇编第一行 `cli`，内核入口即刻关中断（v0.4 重新开启） |
| B-02 | 中 | 内核无自有 GDT/IDT，全程依赖固件遗留状态；任何异常直接 triple fault，不可诊断 | v0.2 本体：自建 GDT/IDT + 异常诊断 |
| B-03 | 低 | 异常处理无自动化回归（v0.1 仅验证成功路径） | 新增 `make test-exception`（feature 门控触发 #DE） |
| B-04 | 低 | 启动日志含一次性低地址内存诊断输出（10 行） | 保留：仅打印 <64MiB 区域，用于装载地址冲突诊断；后续改 QEMU 调试开关 |
| B-05 | **高** | 内核目标默认链接为 **PIE（ELF DYN）**：函数地址表等绝对数据依赖 `.rela.dyn` 的 `R_X86_64_RELATIVE` 加载时重定位，而 bootloader 只按程序头拷贝段、不处理动态重定位 → v0.1 未暴露（全是 RIP 相对访问），v0.2 异常注入时 IDT handler 地址读到错误值，直接把 #DE 分发到垃圾地址 | `.cargo/config.toml` 对 `x86_64-unknown-none` 强制 `-C relocation-model=static`；重建后 ELF 为 EXEC、`There are no relocations`，测试通过 |

复查结论：v0.1 在其自身功能路径（ELF 装载、内存映射、ExitBootServices、ABI 边界）上无回归；
但 v0.2 的异常注入测试暴露出 B-05 这一高危潜伏缺陷（静态重定位未处理），已在 v0.2 根治。
异常防御缺失（B-02）同样由 v0.2 补齐并自动化验证。

### v0.3 — Physical/Virtual Memory + Heap ✅（2026-10-07 完成）
- [x] 解析 BootInfo MemoryMap（UEFI 描述符，仅 `EfiConventionalMemory` 可用）
- [x] FrameAllocator（位图，1 bit/帧，128KiB 静态位图覆盖 4GiB；保留内核映像/低 1MiB/BootInfo/堆区）
- [x] 页表 Mapper（4 级 translate / map_4k / unmap_4k / TLB 刷新 / AddressSpace / 恒等映射补齐）
      —— 内核高半区映射移至 v0.6（ADR-009，与用户态隔离一并完成）
- [x] 内核堆 + `#[global_allocator]`（内核映像末尾预留 1MiB + 首次适配空闲链表，带合并）
- [x] 验收（自动化）：`make test-memory` PASS（`Box`/`Vec`/`String` + 1024 帧压力 + 页表映射自测）；
      `make test-pagefault` PASS（#PF 诊断 + CR2）；`make test` / `make test-exception` 回归 PASS
- [x] 文档完成：[docs/memory.md](docs/memory.md)（v0.3 实现记录、B-06/B-07、ADR-009/010/011 落地）

v0.3 发现并修复的缺陷（随本阶段一并落地）：

| # | 级别 | 问题 | 修复措施 |
| ---- | ---- | ---- | ---- |
| B-06 | 中 | v0.2 异常名表自向量 9 起错位一位（漏记 #9 coprocessor segment overrun）→ #PF 打印成 `reserved (#14)`、#GP 打印成 `page fault`；v0.2 的 #DE 测试刚好在错位之前 | 按 Intel SDM Table 6-1 重建 32 项名表；新增 `make test-pagefault` 覆盖 #PF 名称 + CR2 |
| B-07 | 中 | 堆分配器首版把对齐前导 padding 计入已分配块大小 → 块尾越界覆盖后继空闲块头（空闲链表自环、分配死循环） | 块大小改为 `want - pad`；压力测试增加填充模式校验（抓重叠/越界），已回归 |

### v0.4 — Interrupt + Timer + Keyboard ✅（2026-10-08 完成）
- [x] 中断控制器骨架与 PIC 第一版（初始化、IRQ→向量映射、mask/EOI 原语）
- [x] `mod interrupt` 绑定到 `controller / exception / irq` 子模块
- [x] `kernel_main` 中接入 PIC 初始化（默认全屏蔽，不开启外设 IRQ）
- [x] 这次额外纳入的外部流程文档：`kernel/src/interrupt/doc CONTRIBUTING_CHAIN.md`（当前作为本模块集成上下文快照，不覆盖 DEVELOPMENT.md 本身的阶段定义）
- [x] PIC 选型策略与当前状态记录（`kernel/src/interrupt/pic_policy.md`、`kernel/src/interrupt/known_state.md`）
- [x] PIT Timer tick + 系统计时雏形（`kernel/src/time/timer.rs` 设置通道 0 重装载值、`kernel/src/time/clock.rs` 提供 tick 计数）
- [x] IRQ 分发入口与最小 IRQ0 注册表骨架（`kernel/src/interrupt/irq.rs`），串口心跳防重入保护具备
- [x] IRQ0（PIT）回调已注册，在串口观测心跳（每 10 个 tick 打印一次 `tick=<n>`，经回归测试环境确认可观察）
- [x] PIT 通道 0 频率校正为 **100Hz（10ms/tick）**（`time::timer::TICK_RELOAD_100HZ`，心跳节流每 100 tick = 1 秒）
- [x] IRQ 汇编 stub 修正：向量 32..47 全部走 `irq_common` 分发（原先只有 IRQ0 特例，其余 IRQ 会落入异常分发而 panic）
- [x] IRQ 统一入口 `irq_common` 保存/恢复全部通用寄存器（见 B-09）
- [x] `kernel/src/driver/keyboard.rs`：PS/2 键盘控制器初始化（排空输出缓冲、打开 IRQ1 使能位）
- [x] PS/2 键盘中断输入：IRQ1 扫描码接收 → set 1 解码（字母/数字/Shift/扩展前缀）→ 串口回显 `KB <字符>`
      （v0.9 起改为**行缓冲 + 终端式原样回显**：`Enter` 交付整行并打印 `kbd: line ready len=N`，
      旧格式废弃，`make test-keyboard` 断言同步——见 docs/drivers.md 1.6）
- [x] 上下文切换原语（为 v0.5 铺路）：`arch::x86_64::context` 的 `switch_to` / `init_context` + 内核自测 `context: switch PASS`
- [x] 并行共享磁盘镜像：QEMU 改用 `snapshot=on`（临时写时复制），多场景并发不抢镜像写锁且不污染原镜像
- [x] H-04 测试编排器（Go）：`tools-go/cmd/testorch` + `internal/orchestrator`（场景配置校验、并发信号量、场景超时、串口日志二次复核）
- [x] H-05 串口日志分析（Go）：`tools-go/cmd/serialmon` + `internal/serialparse`（行分类、心跳递增检测、`-expect` 核对、`-allow-panic`）
- [x] 验收（自动化）：`make go-test`（`go vet` + `go test`）PASS；
      `make test-orch`（编排器并行跑 6 个集成测试场景 + 串口二次复核）PASS（6 passed / 0 failed）

v0.4 发现并修复的缺陷（随本阶段一并落地）：

| # | 级别 | 问题 | 修复措施 |
| ---- | ---- | ---- | ---- |
| B-08 | **高** | IRQ 汇编 stub 只把 IRQ0（`isr_32`）改为调用 `irq_dispatch`，向量 33..47 仍 `jmp isr_common` → 走向 `exception_dispatch`；IRQ1 一触发就 `EXCEPTION: irq(1)` + `KERNEL PANIC` | 16 个 IRQ stub 统一 `push 0/pushN/jmp irq_common`；`make test-keyboard` 覆盖 IRQ1 派发路径 |
| B-09 | **高** | IRQ 路径直接 `call irq_dispatch` 而不保存通用寄存器，`iretq` 后被中断的代码带着被破坏的 caller-saved 寄存器继续执行 → 随机 #GP（表现为内存初始化的 `bit_used` 解引用垃圾指针） | `irq_common` 参照 `isr_common` 保存/恢复 15 个通用寄存器，并在 `call` 前强制 16 字节栈对齐 |

### v0.5 — Process + Thread + Scheduler + IPC ✅（2026-10-08 完成）
- [x] Task/Thread 结构与状态机（`process/thread.rs`：`Thread` + `ThreadState`；`process/pid.rs` PID 分配）
- [x] 内核级上下文切换接入调度流程（`process/context.rs` 转发 `arch/x86_64/context.rs`，含 RFLAGS 保存）
- [x] 就绪队列与调度策略抽象（`scheduler/queue.rs` 定长环形队列；`scheduler/round_robin.rs` 200ms 时间片）
- [x] Round Robin 调度器 + PIT 时间片抢占（`scheduler/scheduler.rs`，IRQ0 → `on_tick` → `switch`）
- [x] 基础 IPC（pipe）—— v0.5 最后一块，单向 pipe 骨架已落地
    - `kernel/src/ipc/pipe.rs`：固定容量（64B）环形缓冲单向管道，非阻塞语义
      （写满短写、空读返回 0），创建/写入/读取均打印串口摘要日志
- [x] IPC 模块占位（pipe 优先；channel / shared_memory 后续）：
    - `kernel/src/ipc/pipe.rs`（已实现，含 `selftest`）
    - `kernel/src/ipc/channel.rs`（尚未编写，保留位置；已更新占位说明）
    - `kernel/src/ipc/shared_memory.rs`（尚未编写，保留位置；已更新占位说明）
- [x] 验收（自动化）：`make test-scheduler`（pid 1/2/3 三个内核任务依次运行 + `sched: switch pid=3 -> 0` + `sched: round-robin wrap PASS`）；
      `context: switch PASS` 随 `make test-memory` 断言
- [x] IPC 验收（v0.5 余项，2026-10-08 通过，新增 `make test-ipc`）：
    - [x] 能创建单向 pipe（`pipe: create id=1 cap=64`）
    - [x] 写端可写入、读端可读取已写字节（`pipe: write id=1 len=10` / `pipe: read id=1 len=10` + 读回比对）
    - [x] 串口可观测到 pipe 创建/读写摘要日志
    - [x] 调度器巡回不崩溃（pipe 自测在 Round Robin 巡回后执行，`pipe: selftest PASS`；调度回归仍通过）
- [x] v0.5 收官标记（2026-10-08）：IPC 验收全部通过，v0.5 标记为整体完成；
      channel / shared_memory 仍为后续占位，不在本轮范围

### v0.4/v0.5 当前完成判断（2026-10-08 复核后更新）：
- v0.4 整体标记为**已完成**：内核侧（PIC / PIT 100Hz / PS/2 键盘 / 上下文切换原语）已验收，
  主机侧 H-04/H-05（testorch / serialmon）已落地，`make go-test` 与 `make test-orch`（6 场景并行）均实测通过。
- v0.5 整体标记为**已完成**：线程模型 + 上下文切换 + Round Robin 时间片调度已验收，
  IPC（pipe 骨架）已落代码并通过 `make test-ipc` 验收；channel / shared_memory 按计划保留为后续占位。
- 下一开发顺序按 DEVELOPMENT.md 第 5 节版本顺序继续：v0.6 核心与用户态脚手架均已落地，继续 v0.7（ELF Loader / VFS 文件语义 / 更多 syscall）。

v0.5 踩坑记录（已修复，建议保留）：

| # | 级别 | 问题 | 修复措施 |
| ---- | ---- | ---- | ---- |
| B-10 | **高** | 上下文切换未保存 `RFLAGS`。调度切换发生在 IRQ0 中断上下文内（中断门已清 IF），切出后 IF 永久为 0 → 后续 IRQ 不再递交，调度与心跳同时停摆 | `context_switch` 加入 `pushfq`/`popfq`；`init_context` 的新上下文默认 `RFLAGS=0x202`（IF=1） |
| B-11 | **高** | IRQ 的 EOI 原先在回调**之后**发出；IRQ0 回调会触发任务切换，本调用栈很晚才恢复 → PIC 不再递交 IRQ0 | `irq_dispatch` 改为「先 EOI，后分发」（中断门已清 IF，提前 EOI 无嵌套风险） |
| B-12 | 中 | 启动上下文切出后没有回到就绪队列，Round Robin 永远绕不回 pid 0，`start()` 无法返回 | `start()` 在切出前把 pid 0 压回队尾 |

### v0.6 — Syscall + User Space（核心 + 用户态脚手架已实现并通过 `make test-user` 验收，2026-10-10 / 收尾 2026-10-11）
- [x] syscall 指令入口：**选定 `syscall` / `sysret`**（`int 0x80` 方案不保留）
    - `IA32_LSTAR` → `syscall_entry`；`IA32_STAR` 决定 GDT 用户段布局（udata 必须紧邻 ucode 之前）；
      `IA32_FMASK` = IF\|DF\|TF，内核在关中断下运行 syscall handler
    - 寄存器约定：号 `rax`；参数 `rdi/rsi/rdx/r10/r8`；返回值 `rax`；负数 = `-errno`
- [x] `syscall/mod.rs` 分发层 + `syscall_table.rs` 全号段分发表（未实现号落在 `-ENOSYS` 桩）
- [x] 已实现的 syscall（v0.6 最小集）
    - [x] `write`（fd=1/2 写串口；用户缓冲先用当前用户地址空间校验，越界 `-EFAULT`）
    - [x] `exit`（记录退出码并结束当前用户态执行）
    - [x] `getpid`（返回当前用户程序 PID）
    - [x] `sleep`（PIT tick 粒度等待；等待期间 `sti; hlt`）
- [x] 用户地址空间隔离（`syscall/address_space.rs`）
    - [x] 独立 PML4 + 用户映射固定在下标 1（0x8000000000），Drop 整棵释放
    - [x] 内核子树 `USER` 位惰性清理（只在该位确为 1 时写回）
    - [x] 代码页只读可执行、栈页可写不可执行；每级中间表均置 `USER`
- [x] ring3 异常终止语义：`exception_dispatch` 用 `cs & 3` 区分来源，用户态异常不再 panic
- [x] 用户态执行期收敛：`scheduler::suspend()` + `TSS.RSP0` 切到专用陷阱栈
- [x] 用户态脚手架（2026-10-11 收尾，`make user` + `make test-user` 实测通过）：
    - [x] `libc/`：syscall 封装（`write`/`getpid`/`nanosleep`/`exit`）、`_start`（`.text.start` → 用户 `main`）、
      `print!`/`println!`（一次宏调用 = 一次 `write`）、`#[panic_handler]`
    - [x] `user/hello`：真实 Rust 用户程序（不再是内嵌机器码），进入 workspace 与构建链
    - [x] `kernel/build.rs`：构建期构建用户程序 → 解析 ELF → 摊平成连续映像 → `include_bytes!` 内嵌
      （同时断言装载基址 = `USER_VIRT_BASE`、入口在映像第 0 字节、无可装载重定位）
    - [x] `UserSpace::load_program` 支持多代码页（1..=8 页，实测 `user/hello` 占 2 页），
      代码区/栈区间固定布局
    - [x] `make user` 单独验证脚手架（不启 QEMU）；`make test-user` 断言 `user: program=hello source=libc`
- [x] v0.6 验收（`make test-user`，19 项串口断言实测 PASS）：
    - [x] 用户态隔离下 syscall 入口/返回可观察（`user: write fd=1 len=22` / `user: getpid -> 1`）
    - [x] `exit` 能回收退出码（`user: exit code=42` → `user: hello PASS`）
    - [x] `getpid`/`sleep` 可用（`user: sleep done ticks=2`，计时真实推进）
    - [x] 隔离硬证据：probe 读内核页被拦下（`vector=0xe error=0x5 cr2=0x2000000`）+ 内核继续运行
- [x] 推进到 v0.7 的部分（v0.6 时明确未做；2026-10-11 已在 v0.7 完成的部分）：
    - [x] 用户程序通过 **ELF Loader** 从文件加载运行（RAMFS 上的 `/init`）
    - [x] `read` / `open` / `close`（与 VFS 对接）
    - [x] 用户静态数据可写（`RW` 段按段权限映射，实测 `hello: static counter=1`）
- [ ] 仍未做（属于 v0.8+，不应被读成已完成）：
    - [x] ~~`fork` / `exec` / `wait`~~ → **v0.9 第三步已做**（2026-10-11，见下方 v0.9 小节与 docs/syscall.md 3.7）
    - [x] ~~进程/线程对象~~ → **v0.9 第三步已做**（真正的 PCB：每进程地址空间 + 每进程内核栈 + `Ready/Running/Zombie`）
    - [ ] 高半区迁移（隔离当前靠页表 `USER` 位而非地址空间划分）
    - [x] ~~用户堆/`brk`/`mmap`~~ → v0.9 第一步已做；~~`argc/argv` 传递~~ → **v0.9 第三步已做**

v0.6 踩坑记录（已修复，建议保留）：

| # | 级别 | 问题 | 修复措施 |
| ---- | ---- | ---- | ---- |
| B-13 | **高** | 创建用户地址空间时无条件重写整棵内核页表来清 `USER` 位。OVMF/EDK2 把自己建立的页表页标为**只读** → 启动路径上直接 #PF（实测 `CR2=0x1fc03000`，`error=P\|W`） | `scrub_user_bits` 改为**惰性**：只在 `USER` 确为 1 时才写回，并打印清位数（实测 OVMF 全为 0） |
| B-14 | **高** | `syscall` 返回路径栈算术写成 `add rsp, 48`。8 次 push 是从栈顶**向下**增长，该写法把 rsp 移到栈顶之上，`pop r11/rcx` 取到垃圾 → `sysretq` 跳飞到 `RIP=0x3` | 改为 `sub rsp, 16` 后 `pop r11` / `pop rcx`（r11/rcx 位于 top-16 / top-8） |

### v0.7 — VFS + RAMFS + ELF（核心已实现并通过验收，2026-10-11）
- [x] VFS 抽象（`kernel/src/fs/vfs.rs`）：`FileSystem` trait（stat/read/read_all/write/create/mkdir/list/usage）
      + 路径工具（`normalize`/`components`/`within`）+ 系统级入口（经挂载表路由，错误一律负数 errno）
- [x] 挂载管理（`fs/mount.rs`）：挂载点 → 后端，**最长前缀路由**（`/mnt/*` 落到 `/mnt` 的后端）
- [x] Inode/元数据（`fs/inode.rs`）：`NodeKind`、`Metadata`（含 `ls -l` 风格权限串）、`DirEntry`
- [x] RAMFS（`fs/ramfs.rs`）：`Node { name, kind, data: Vec<u8>, children }`，单文件上限 256 KiB；
      `fs::init` 建立初始文件树 `/init` + `/etc/motd` + `/tmp`
- [x] fd 表与文件 syscall（`fs/file.rs` + `syscall/file.rs`）：0/1/2 保留、动态 fd ≥ 3（最小空闲槽位）、
      `open`（指针 + 长度、`O_CREAT`）/`read`/`write`/`close`，用户缓冲按页校验（`-EFAULT`）
- [x] **ELF Loader**（`syscall/elf.rs`）：ELF64/`ET_EXEC`/`EM_X86_64` 解析 → 按段权限逐页映射
      （`W` → 可写、非 `X` → `NX`）→ 拷贝 `p_filesz`、`p_memsz` 余下置 0 → 入口取 `e_entry`；
      拒绝畸形 ELF 时打印 `user: ELF rejected: <reason>`
- [x] 用户程序改为从文件加载：`kernel/build.rs` 内嵌 ELF → RAMFS `/init` → `vfs::read_all` → `elf::load`
      （v0.6 的「构建期摊平」已移除）；`user.ld` 四个段分别 `ALIGN(0x1000)`，`W^X` 不退化
- [x] 隔离探针保留为 `map_raw` 路径（探针不依赖被测的 ELF Loader）
- [x] 验收：`make test-fs` 19 项 PASS（挂载/初始文件树/stat/内容读回/挂载点路由/`ls`/负例 errno）；
      `make test-user` 32 项 PASS（`source=elf` + 段数/页数/可写页 + `open/read/close/write` 全链路 +
      用户写入被内核读回 + `static counter=1`（`RW` 段可写）+ `EFAULT` + 退出码/隔离回归）
- [x] `make user` 追加 `tools/check_user_elf.py`：不启 QEMU 即可校验基址/段页对齐/`W^X`/映像窗口/无重定位
- [x] Shell 里 `cat` 已可用（v0.9 第二步，内建命令走同一套文件 syscall）；`ls`/`cd` 等
      **外部程序**在 v0.9 第三步打通（`fork`/`execve`/`wait4`）—— 见下方 v0.9 小节
- [x] 磁盘文件系统后端（FAT）+ Python mkfs（H-03）→ **v0.8 已落地**（见 v0.8 小节的最后两条）
- [x] 用户堆 / `brk` / `mmap`（v0.9 第一步）、`argc/argv` 与 `fork`/`execve`/`wait4`（v0.9 第三步）；
      **剩余**：动态链接、`mprotect`（见 docs/syscall.md 3.4）

### v0.8 — Driver + PCI + Disk（已实现并通过验收，2026-10-11）
- [x] Driver Manager 与驱动注册框架（`kernel/src/driver/driver.rs`）：`Driver` trait（`name`/`matches`/`attach`）、
      固定容量注册表、`probe_all()` 按注册顺序匹配 + 挂接、`ProbeStats { devices, matched, attached, failed }`
- [x] PCI 枚举 + Vendor/Device 匹配（`kernel/src/driver/pci.rs`）：配置机制 #1（`0xCF8`/`0xCFC`）、
      全总线扫描（多功能位修剪）、BAR 探测（写全 1 读掩码）与**窗口大小**计算、类别三元组解码、`print_bdf`
- [x] 块设备抽象 + 注册表（`kernel/src/driver/block.rs`）：`BlockDevice`（`read`/`write`/`sectors`/`sector_size`）、
      512 字节扇区粒度、长度非整扇区直接报错（不静默取整）
- [x] 块设备驱动：**legacy virtio-blk（0.9.5）**（`kernel/src/driver/virtio_blk.rs`）：
      virtqueue（描述符表 + avail + 4 KiB 对齐 used）→ notify → 轮询 used ring（PIT tick 超时），
      描述符链「请求头 → 数据 → 状态字节」，页对齐 bounce buffer 分块搬运（不要求调用方缓冲对齐/连续）；
      status 机 `ACKNOWLEDGE → DRIVER → FEATURES_OK → DRIVER_OK`，modern（0x1042）明确拒绝并给出原因
- [x] 基础设施：`arch/x86_64/port.rs`（`inb/inw/inl`+`outb/outw/outl`）、`frame::allocate_contiguous` / `free_contiguous`
      （virtqueue 必须物理连续，legacy 的队列地址只装一个 PFN）
- [x] 硬件路线改变：QEMU 启动盘由 IDE/AHCI 改为 **virtio-blk（legacy）** —— 「镜像能启动」与
      「内核驱动能读写这块盘」变成同一台设备上的同一条路径（Makefile `run` / `tests/test_boot.py` / `tests/test_keyboard.py`）
- [x] 验收：`make test-driver` 22 项 PASS（`vendor=0x1af4 device_id=0x1001`、`bar0=io:`、
      `queue0 size=256 ... 3 pages contiguous`、`capacity=131072 sectors (64 MiB)`、
      `boot sector sig=0x55aa fstype=FAT16`、分块读一致性 512/4096/4608、写回读 + 还原、三类负例）；
      编排场景由 8 个增至 **9 个**（`driver`），`make test-orch` 9 passed / 0 failed（127.2s）
- [x] 磁盘 FS 后端（**FAT12/16/32，只读**）挂载到 VFS（`kernel/src/fs/fat.rs` + `fs::mount_disks`）：
      类型由 BPB 的**簇数**判定（不靠扩展名）、12/16 的固定根目录区与 32 的子目录簇链、
      8.3 短名 + NT 大小写标志 + **LFN 长文件名**（含校验和）、任意偏移读、空文件（首簇 0）正确返回 0；
      写操作一律 `EROFS`；簇链断/越界 → `EUCLEAN`（不静默返回半截数据）。
      `FileSystem` trait 随之收窄为 `&mut self` + `read_all -> Vec<u8>`（磁盘文件内容不在内存里，
      原来那种「借用后端缓冲」的形状不再成立）；**syscall 层一行未改** —— 这正是 VFS 收口的目的
- [x] Python `mkfs` 工具（H-03，`tools/mkfs/mkfs.py`）：自己实现 FAT12/16/32 镜像生成（引导扇区/FSInfo/
      FAT 副本/LFN + 8.3 目录项/簇链），写完回读校验；正确性由 **mtools 独立复核**（`mdir`/`mtype`
      读同一张盘，见 `tests/fatroot/README.md`）—— 两套独立实现互相印证，而不是自己验证自己
- [x] 验收：`make test-fat` **21 项** PASS（双盘启动）：`/disk` 是固件写的 FAT16 启动盘，
      从它读回**内核自己的映像**（`/disk/KERNEL.ELF` 前 4 字节 = ELF 魔数 `0x7f454c46`）与
      引导程序（`BOOTX64.EFI` = `MZ`）；`/mnt/fat` 是 `mkfs` 生成的 FAT32 测试盘，
      跨簇文件（3000 字节 = 6 个簇）**逐字节对公式**、偏移 1024 的 8 字节读、空文件、LFN 长文件名、
      二级子目录、最长前缀路由（`/mnt/note.txt` → RAMFS、`/mnt/fat/*` → FAT）、负例 `ENOENT/ENOTDIR/EISDIR/EROFS`；
      编排场景由 9 个增至 **10 个**（`fat`）

#### v0.8 缺陷与设计记录

| 编号 | 等级 | 现象 | 处理 |
| ---- | ---- | ---- | ---- |
| B-16 | 中 | BAR 窗口大小算错：低字掩码（u32）被当成 u64 取反 → 类型正确的 I/O BAR 报出 `0xFFFFFFFF00000080` 这种尺寸（真实值 0x80） | 大小改为在该字**自己的位宽**里求 `~mask + 1`（`0x1_0000_0000 - (mask & !0xF)`）；另补两条边界：掩码全 1 = BAR 未实现，64 位 BAR 低字掩码为 0 → 大小由高字决定 |
| B-17 | 高 | `lba + sectors` 用 `u64` 加法：`lba = u64::MAX - 1` 时环绕后的和为 0，「看起来」在容量内 → 越界请求会发给设备 | 改用 `checked_add`，溢出即报错；负例测试加入 `lba=u64::MAX-1` |

设计取舍（与 ADR 同级，理由见 docs/drivers.md 1.2）：

- PCI 走配置机制 #1（不依赖 ACPI）而不是 ECAM；
- 只实现 legacy virtio，modern 遇到就报错（QEMU 用 `disable-modern=on` 提供 legacy）；
- 块 I/O 同步轮询（自旋 + PIT tick 超时），不配置 MSI-X/INTx；
- 驱动注册表容量固定、启动期写入、之后只读（`main` 只看 `ProbeStats`）。

### v0.9 — Shell + User Programs + Network
- [x] **运行时前置条件第一步：用户堆**（2026-10-11 落地）
    - [x] `brk`（256 KiB 窗口，页对齐、增长全成或全不成；“失败返回原值”的 Linux 语义）
    - [x] `mmap`（只收 `MAP_PRIVATE|MAP_ANONYMOUS`，5 参数无 offset；窗口 256 KiB、first-fit；**新页全零**；`PROT_WRITE|PROT_EXEC` → `EINVAL`（W^X））
    - [x] `munmap`（整段归还；清页表项 + `invlpg`；未对齐/未映射 → `EINVAL`）
    - [x] `libc::heap`：`#[global_allocator]`（bump over `mmap`，64 KiB 块，`dealloc` 不回收 —— 由 `UserSpace::Drop` 在运行结束时整棵释放）
    - [x] 参考用户程序加入堆自测（零页、模式读回、堆缓冲 `write`、6 个负例、`Vec`/`format!`）
    - [x] 验收：`make test-user` 由 32 项增至 **59 项**（含 `user: heap frame reclamation PASS`）；
          `orchestrate.json` 的 `user` 场景同步；`mprotect` 列入「号已定义但未实现」
- [x] **运行时前置条件第二步：`stdin` + 交互式 Shell**（2026-10-11 落地）
    - [x] 键盘行缓冲（`driver/keyboard.rs`）：128 字节单行缓冲、行编辑（退格/回显）、`Enter` 交付整行 + `kbd: line ready len=N`
    - [x] `read(0)` 行缓冲语义（`syscall/file.rs::read_stdin`）：整行交付（行尾 `\n`）、3 秒无整行 → `-EAGAIN`(-11)（无进程模型时的权宜之计）
    - [x] `user/shell`（→ `/bin/sh`）：提示符 `root@cerlesse C:>`（用户名@主机名 盘符>）、参数切分、内建 `help`/`echo`/`cat`/`pid`/`exit`
    - [x] 外部命令路径：真的调 `fork()` → 当时内核回 `syscall: ENOSYS nr=57 name=fork`（依赖关系的实测证据；
          **v0.9 第三步已实现**，该 ENOSYS 留痕自然消失，`make test-shell` 的断言同时改为外部程序路径）
    - [x] `kernel/build.rs` 由「单个用户程序」改为「按列表构建+校验+内嵌」（`/init` + `/bin/sh` 同路数）
    - [x] 验收：新增 `make test-shell`（**11 项**，QEMU monitor 注入真实按键）+ 编排 `shell` 场景（**11 场景**，后续第三步又加为 12）；
          `make test-user` 由 59 项增至 **61 项**；`make test-fs` 根目录条目数 3 → 4（新增 `/bin`）
- [x] **运行时前置条件第三步：进程对象 + `fork`/`execve`/`wait4`**（2026-10-11 落地）
    - [x] 进程对象（`process/process.rs`）：真正的 PCB（地址空间 / 用户帧 / 每进程 64 KiB 内核栈
          + 栈底哨兵 / `return_to` / `Ready|Running|Zombie`）；旧入口假设「一次只跑一个用户程序」的问题
          在 docs/syscall.md 3.6 / process.md 1.4 记下了根因
    - [x] `syscall/process.rs`：`fork`（复制地址空间 + 克隆当前用户帧）、`execve`（换映像 + 重建 argv 用户栈，
          先切 CR3 再 Drop 旧地址空间）、`wait4`（延迟调度 + 回收）
    - [x] `trampoline` 改为按进程切换：CR3 / `TSS.RSP0` / syscall 栈顶 / 当前进程下标；`suspend`/`resume` 语义
    - [x] `argc/argv` 传递（`libc/src/args.rs`，入口寄存器 `rdi`/`rsi`）
    - [x] 外部程序 `user/hello-bin` → RAMFS `/bin/hello`；Shell 的 `cmd_external` 改走 `fork`+`execve`+`wait4`
          （`ls` → `/bin/ls` 不存在 → `execve` 返 `-ENOENT` → 子进程 `exit(127)`）
    - [x] 验收：新增 `make test-proc`（**8 项 + 4 条正则**；`--expect-re` 用反向引用断言
          fork/execve/wait4 三处 PID 相同、并断言 `frames free before=N after=N` 无页帧泄漏）；
          编排新增 `proc` 场景（11 → **12 场景**）；`make test-shell` 12 → 11 项
- [ ] 动态链接（`PT_INTERP`/`ld.so`）与 `mprotect`（W^X 转换）；文件映射（`mmap` 加 offset）
- [ ] 其余用户程序（ls/cp/mv/rm/mkdir/ps/kill）与 Shell 增强（`cd`、管道、重定向）
- [ ] 简化网络：virtio-net → Ethernet → ARP → IPv4 → UDP（TCP 尽力）
- [ ] Socket syscall 最小集
- [x] 验收：Shell 能启动外部程序（`make test-proc`，2026-10-11）；ping/HTTP 简例仍待网络落地

### v1.0 — 完整可启动类 Linux 小型 OS
- [ ] 用户/组/权限（`-rwxr-xr-x`、UID/GID）
- [ ] 进程隔离与 syscall 参数校验
- [ ] 启动链：Kernel → init → Shell → User Programs
- [ ] 文档全量修订、发布 v1.0 tag
- [ ] 验收：QEMU 一键启动到可交互 Shell

---

## 6. 测试方案

| 层级 | 内容 | 工具 | 开始版本 |
| ---- | ---- | ---- | ---- |
| 单元测试 | 纯逻辑模块（分配器算法、ELF 解析、路径解析）在 host 上 `cargo test` | Rust | v0.4（v0.3 暂以内核内自测覆盖，见 docs/memory.md 第 5 节） |
| 集成测试 | QEMU 启动 → 断言串口日志包含预期输出 | Python (H-02) | v0.1 |
| 压力测试 | 帧分配、任务轮转、fork 风暴 | Python + Go 编排 | v0.3/v0.5 |
| 回归测试 | 每个里程碑的验收标准固化为测试用例 | Go 编排服务 | v0.4 |
| 异常测试 | 故意触发 #DE/#PF，校验诊断输出（`make test-exception` / `make test-pagefault`） | Python | v0.2（#PF 于 v0.3 加入） |
| 驱动/块设备测试 | PCI 枚举 → 驱动挂接 → 扇区读写/分块一致性/负例（`make test-driver`） | Python | v0.8 |
| 磁盘文件系统测试 | 双盘启动 → FAT16 启动盘读内核映像 / FAT32 测试盘读·LFN·跨簇公式·偏移读·负例（`make test-fat`） | Python | v0.8 |
| 包容器测试（主机侧，不启 QEMU） | `.ckc` 容器：确定性、头部逐字段、payload 篡改、ustar 结构规则、打包期规则（超长路径/越界 target/setuid/hooks）+ 系统 `tar` 独立复核（`make test-pkg`） | Python | H-06 P0a |
| 键盘/交互输入测试 | QEMU monitor `sendkey` 注入真实按键 → 断言 IRQ1 标记、行缓冲整行交付（`make test-keyboard`）；交互式 Shell 全流程（`sh: prompt` → `help` → `cat /etc/motd` → `exit`，`make test-shell`）；**进程调用全流程**（`hello world` → fork/execve/argv → 退出码回收，`ls` → `-ENOENT` → `exit(127)`，`make test-proc`；`--expect-re` 支持正则 + 反向引用） | Python | v0.4（v0.9 起含 Shell / 进程） |

约定：

- 每个版本任务清单中的"验收"必须落成自动化测试。
- 串口是唯一权威日志通道，测试断言只依赖串口输出。

## 7. QEMU 调试方案

启动（计划的 Makefile 目标）：

```bash
# 构建镜像后启动（v0.1 起提供；v0.8 起启动盘挂在 virtio-blk 上）
make run
# 底层等价于：
qemu-system-x86_64 -machine q35 -m 512M -serial stdio -display none \
  -drive if=none,id=blk0,format=raw,file=build/disk.img \
  -device virtio-blk-pci,drive=blk0,disable-modern=on
```

注：`disable-modern=on` 是必需的 —— 内核只实现 legacy virtio（见 docs/drivers.md 1.2）；
测试脚本会再加 `snapshot=on`（临时写时复制），使并行场景共享同一镜像且不污染原始文件。

调试流程：

1. **串口日志**：`-serial stdio`，所有 `println!` 重定向到串口，开发期唯一输出通道。
2. **GDB 联调**：QEMU 加 `-s -S`，主机 `gdb target remote :1234`，配合 `cargo run`/`gdb` 脚本单步内核。
3. **崩溃分析**：`-no-reboot -d int,cpu_reset`，配合异常 handler 的寄存器转储定位 triple fault。
4. **断点**：内核符号保留在 ELF 中，`add-symbol-file kernel.elf` 后可按函数名下断点。
5. **Windows 主机**：同一套 QEMU 命令（QEMU for Windows），GDB 用 `gdb-multiarch`/`gdb.exe`，Makefile 目标保持跨平台一致。

## 8. GitHub Issue / 里程碑规划

里程碑与版本一一对应（Milestone 标签 `v0.1` … `v1.0`），Issue 模板分类：

| 标签 | 用途 |
| ---- | ---- |
| `boot` / `memory` / `interrupt` / `process` / `syscall` / `fs` / `driver` / `network` / `security` | 模块归属 |
| `phase:bug` / `phase:feature` / `phase:refactor` / `phase:doc` | 类型 |
| `host:python` / `host:go` | 主机侧工具 |
| `blocked:upstream` | 被依赖项阻塞 |

规划规则：

1. 每个里程碑开工时，把第 5 节对应版本的 checklist 拆成 Issues（一个 checklist 项 ≈ 1 个 Issue，粒度过大的再拆）。
2. Issue 标题格式：`[v0.x] 模块: 简述`，例如 `[v0.3] memory: 实现 bitmap FrameAllocator`。
3. 里程碑关闭条件：该版本任务清单全部勾选 + 验收测试通过 + 对应阶段文档更新。
4. 第一阶段只建 Milestone `v0.1` 及其 Issues，后续版本的 Issues 在开工前一个版本创建。

---

## 9. 修订记录

| 日期 | 变更 |
| ---- | ---- |
| 2026-10-06 | 第一阶段初稿：需求、依赖、workspace 规划、v0.1–v1.0 任务清单、测试/QEMU/Issue 规划 |
| 2026-10-07 | v0.3 完成：内存管理（帧分配器/页表 Mapper/内核堆）；新增 test-memory、test-pagefault 验收目标；补记 B-06/B-07 |
| 2026-10-08 | v0.4 完成：PIC/PIT 100Hz/PS-2 键盘/上下文切换原语；新增 test-keyboard 验收目标；补记 B-08/B-09；H-04/H-05 主机侧 Go 工具仍待开工 |
| 2026-10-08 | v0.5 内核侧落地：线程模型/PID/就绪队列/Round Robin 时间片调度接入 PIT/IRQ0；新增 test-scheduler；补记 B-10/B-11/B-12；IPC 仍待做 |
| 2026-10-08 | 本次 docs 迭代统一文档与代码库状态不一致点：v0.4 收官前，部分文档一度记载 H-04/H-05 仍待编写；但工具代码已实际落地；随后复核确认 `make go-test`（vet + test）与 `make test-orch`（6 场景并行，6 passed / 0 failed）均通过，v0.4 最终标记为整体完成 |
| 2026-10-08 | 继续 v0.5 后续开发前，统一当前未完成项表述：v0.5 仅剩 IPC(pipe/channel/shared_memory) 未落地；syscall.md 与 filesystem.md 仍为占位文档，尚未进入 v0.6/v0.7 编码 |
| 2026-10-08 | 按开发流程复查后更新：v0.4 与 v0.5 都尚未在 DEVELOPMENT 里标记为整体“以完成”，因为当前尚未完成项尚未通过验收/尚未实现 |
| 2026-10-08 | 本次 docs 迭代统一各阶段文档顶部元信息（阶段 / 最后更新 / 状态）、修订记录与本文件描述一致，保持索引与主开发文档一致 |
| 2026-10-10 | **v0.6 核心落地**：`syscall`/`sysret` 入口 + 真实用户页表（PML4 下标 1，每级 `USER`）+ ring3 进入/退出 + `write/exit/getpid/sleep` + ring3 异常终止（不再 panic）；新增 `make test-user` 18 项断言与 orchestrate.json `user` 场景（7 场景）；补记 B-13（固件页表只读 → 惰性清 `USER`）、B-14（`syscall` 返回栈算术）；同步 docs/syscall.md 从占位改为实现记录 |
| 2026-10-11 | **v0.6 用户态脚手架可用**：`libc/` 从占位变为实现（syscall 封装 + `_start` + `print!/println!` + `panic_handler`）；新增真实用户程序 `user/hello` 并入 workspace（含 `default-members` 与 `user-release` profile，large 代码模型 + `lto = false`）；`kernel/build.rs` 构建期构建用户程序并摊平成连续映像内嵌（断言装载基址 / 入口在映像第 0 字节 / 无可装载重定位）；`UserSpace::load_program` 支持多代码页（1..=8）；新增 `make user`，`make test-user` 增至 **19 项断言**（含 `user: program=hello source=libc`）；全量回归（test / test-memory / test-scheduler / test-ipc / test-exception / test-pagefault / test-keyboard / go-test / test-orch 7 场景）PASS |
| 2026-10-11 | **v0.7 落地（VFS + RAMFS + ELF Loader）**：`fs/` 从占位变为实现（`vfs` trait + 路径工具 + 系统级入口、`mount` 最长前缀路由、`inode` 元数据、`ramfs` 内存后端、`file` fd 表）；`read`/`write`/`open`/`close` 接入 `syscall_table`（fd 0/1/2 保留、动态 fd ≥ 3、用户缓冲按页校验）；新增 `syscall/elf.rs`：用户程序改为「RAMFS `/init` → ELF Loader 按段权限逐页映射」（`build.rs` 不再摊平，`user.ld` 四个段 `ALIGN(0x1000)`，`.lbss.*` 必须归入 `.bss`），`RW` 段对 ring3 可写；新增 `make test-fs`（19 项）与 `tools/check_user_elf.py`（`make user` 验段布局），`make test-user` 增至 **32 项断言**；修 B-15（syscall 入口未恢复 caller-saved 寄存器 → 用户态 `r9` 被破坏，#PF）；全量回归（9 个串口测试目标 + `go-test` + `test-orch` **8 场景**）PASS |
| 2026-10-11 | **v0.8 前半落地（驱动框架 + PCI + 块设备）**：新增 `driver/driver.rs`（`Driver` trait + 注册表 + `probe_all` + `ProbeStats`）、`driver/pci.rs`（配置机制 #1 + BAR 探测/窗口大小）、`driver/block.rs`（`BlockDevice` + 注册表 + 验收自测）、`driver/virtio_blk.rs`（legacy virtio 0.9.5：virtqueue/描述符链/轮询/bounce buffer）、`arch/x86_64/port.rs`（端口 I/O 32 位入口）、`frame::allocate_contiguous`（DMA/virtqueue 前置）；启动盘改为 **virtio-blk（`disable-modern=on`）**，`main.rs` 在 `Kernel started!` 后接入「枚举 → 注册 → 探测/挂接 → 块设备自测」；新增 `make test-driver`（22 项）与编排场景 `driver`（**9 场景**，127.2s 全 PASS），全量回归 PASS（含非 snapshot 启动后镜像 md5 不变，验证写测试真的还原了扇区）；修 B-16（BAR 大小 32→64 位宽错误 → I/O BAR 尺寸报错）、B-17（`lba + sectors` 环绕 → 越界请求漏拦）；同步 docs/drivers.md（v0.8 实现记录 + 设计取舍 + 限制） |
| 2026-10-11 | **v0.8 收官（磁盘文件系统 + `mkfs`）**：`fs/fat.rs`（FAT12/16/32 只读：BPB 按簇数定类型、固定根目录区/簇链目录、8.3 + NT 标志 + LFN 校验和、偏移读、`EROFS`/`EUCLEAN`）挂到 VFS（`fs::mount_disks` → `/disk` 启动盘、`/mnt/fat` 测试盘，验证最长前缀路由）；`FileSystem` trait 改为 `&mut self` + `read_all -> Vec<u8>`；新增 `vfs::is_mount_point` 并修「单盘启动误判 `/mnt/fat` 已挂 → 启动被 halt」；`tools/mkfs/mkfs.py`（H-03）自己生成 FAT12/16/32 镜像并由 mtools 独立复核，`tests/fatroot/` 提供测试盘内容与复核说明；`tests/test_boot.py` 新增 `--disk2`；新增 `make fat-image` / `make test-fat`（**21 项**，双盘启动）与编排场景 `fat`（**10 场景**，150.7s 全 PASS）；全量回归 PASS（`test-driver` 22 项 / `test-fs` 19 项 / `test-user` 32 项 / `make go-test`）；同步 filesystem.md（第 3 节由计划改写为实现记录）、drivers.md、roadmap/README/project-structure/tools README |
| 2026-10-11 | **H-06 P0a 落地：`.ckc` 包容器（CKC）** —— 内核包后缀定为 **`.ckc`**（Cerlesse Kernel Container），64 字节头 + 确定性 ustar payload（身份 = payload 的 SHA-256）；新增 `tools/pkgmgr/ckc.py`（纯标准库 `pack`/`verify`/`info`，含 `PKG.toml` 子集解析、scope 白名单、setuid/hooks/超长路径等打包期规则）与验收 `tests/test_ckc.py`（新增 `make test-pkg`，**37 项**，含系统 `tar` 独立复核）；docs/package-manager.md 3.2 由「`.cpk` 裸 tar 设想」改写为容器规格，11.1 拆成 P0a（已落地）/P0b（事务/catalog/receipt）；同步 docs/README.md 索引、tools/README.md、project-structure.md、README.md |
| 2026-10-11 | **v0.9 第三步：进程对象 + `fork`/`execve`/`wait4`** —— `process/process.rs` 换成真正的 PCB（地址空间 / 用户帧 / 每进程 64 KiB 内核栈 + 栈底哨兵 / `return_to` / `Ready\|Running\|Zombie`）；新增 `kernel/src/syscall/process.rs`（fork 复制地址空间与当前帧、execve 换映像并重建 argv 用户栈、wait4 延迟调度 + 回收）；`trampoline` 改为按进程切 CR3/`TSS.RSP0`/syscall 栈顶/当前进程下标； `libc` 新增 `args`（入口寄存器 ABI）并去掉 `fork`/`execve`/`wait4` 的 ENOSYS 说法；新增外部程序 `user/hello-bin` → RAMFS `/bin/hello`，Shell 的 `cmd_external` 改走 `fork`+`execve`+`wait4`；`tests/test_keyboard.py` 新增 `--expect-re`（正则 + 反向引用）；新增 `make test-proc`（8 项 + 4 正则）并加入 `test-all`，编排场景 11 → **12**（新增 `proc`）；`make test-shell` 12 → 11 项；记录两条踩坑（debug 构建下 16 KiB 内核栈静默溢出踩坏 `.bss` 帧分配器；子进程退出时必须先恢复父进程 CR3 再 Drop 地址空间）；同步 syscall/process/libc-README/project-structure/roadmap/README |
| 2026-10-11 | **v0.9 第二步：交互式 Shell 与 `stdin`** —— 键盘行缓冲（128 字节 + 行编辑，回显由 `KB <字符>` 改为终端式原样回显，`kbd: line ready len=N`）、`read(0)` 行缓冲语义（3 秒 `-EAGAIN`）、新增 `user/shell`（提示符 `root@cerlesse C:>`、内建 `help`/`echo`/`cat`/`pid`/`exit`、外部命令真调 `fork()`）、`kernel/build.rs` 改为多用户程序构建（`/init` + `/bin/sh`）、`syscall/mod.rs` 对已命名未实现的号打印 `syscall: ENOSYS nr=… name=…`；新增 `make test-shell`（12 项）与编排 `shell` 场景（11 场景）；`make test-user` 59 → **61 项**，`test-fs` 根目录 3 → 4；同步 syscall/drivers/process/project-structure/roadmap/README |
| 2026-10-11 | **v0.9 第一步：用户堆**（运行时前置条件）—— 新增 `kernel/src/syscall/memory.rs`（`mmap`/`munmap`/`brk`：W^X、`MAP_*`/`PROT_*` 白名单、5 参数无 offset 的取舍、逐条错误码）、`address_space.rs` 新增 brk 窗口（256 KiB）与匿名 mmap 窗口（256 KiB，first-fit，清页表项时 `invlpg`）、`libc/heap.rs`（`#[global_allocator]`，bump over mmap）与 `brk/mmap/munmap` 封装、`user/hello` 堆自测、`user.rs` 页帧泄漏检查；`make test-user` 断言 32 → **59 项**，`orchestrate.json` 的 `user` 场景同步；全量回归 PASS（`test-fs` 19 / `test-driver` 22 / `test-fat` 21 / `make go-test` / `make test-orch` 10 场景） |
| 2026-10-11 | **H-06 包管理器提案 v1 完成（设计阶段，不进内核主线）**：docs/package-manager.md 从「设想」补成可实施规格 —— 术语字段级定义、七个制品格式（`PKG.toml`/确定性 `.cpk`/`catalog.toml(+sig)`/`cpkg.toml`/receipt/journal/内容寻址 store）、内容哈希 + 三级校验（签名 → 绑定 → 落盘后逐文件）、安全模型（显式信任根、scope 域边界与路径白名单、**纯文件安装/无安装脚本**、setuid 拒绝、威胁—对策表）、事务阶段与原子性边界（文件级 + 可恢复）、效率预算（幂等零写入、文件级去重、`gc`）、CLI 与稳定退出码、MVP 三期（P0 主机侧 `tools/pkgmgr/` → P1 与镜像构建/基线对接 → P2 增量补丁等）、可脚本断言的验收条目（`make test-pkg`）；同步 docs/README.md 索引状态（设计中 → 提案 v1）与 H-06 行 |


