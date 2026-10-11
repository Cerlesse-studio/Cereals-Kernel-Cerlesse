# Cerlesse OS 文档中心

- 阶段：第一阶段 / 文档中心
- 最后更新：2026-10-11
- 状态：🟡 持续更新（2026-10-11 同步 v0.9 第二步（**`stdin` + 交互式 Shell `/bin/sh`**）实现记录；同日已同步 v0.8 收官、v0.9 第一步用户堆与 H-06 包管理器 `.ckc` 提案 P0a）

> 本文档顶部保留阶段/最后更新/状态字段，与项目文档维护约定对齐。

> 项目代号：**Cerlesse**（仓库：Cereals-Kernel-Cerlesse）
> 定位：使用 Rust 从零开发的类 Linux 高性能宏内核，支持 Linux / Windows 主机交叉开发（x86_64 优先）。
> 语言策略：**Rust 编写内核核心**，Python 用于构建/测试工具链，Go 用于主机侧辅助服务。

## 文档索引

| 文档 | 内容 | 状态 |
| ---- | ---- | ---- |
| [architecture.md](./architecture.md) | 第一阶段：内核架构、语言分工、目标与非目标 | ✅ 已完成（会随开发反复修订） |
| [lang-selection.md](./lang-selection.md) | 按功能领域汇总的性能最佳语言对照（辅助选型参考） | ✅ 新增（2026-10-07） |
| [package-manager.md](./package-manager.md) | 面向内核/系统生态的高效、安全、轻量、灵活包管理器设计提案（H-06） | 🟡 提案 v1 + **P0a 已落地**：包容器后缀 **`.ckc`**（CKC = Cerlesse Kernel Container，64 字节头 + 确定性 ustar，身份 = payload 哈希），实现 `tools/pkgmgr/ckc.py`，验收 `make test-pkg` 37 项（含 `tar` 独立复核）；事务/catalog（P0b）与内核侧接入（P1）待做，见该文档 11.1 |
| [project-structure.md](./project-structure.md) | 目录结构与模块职责划分 | ✅ 已完成 |
| [../DEVELOPMENT.md](../DEVELOPMENT.md) | 主开发文档：需求、依赖、任务清单、测试、QEMU、Issue 规划 | ✅ 已完成 |
| [boot.md](./boot.md) | 阶段：Bootloader / UEFI / Kernel Entry / GDT / IDT | ✅ v0.1 + v0.2 已完成 |
| [memory.md](./memory.md) | 阶段：物理/虚拟内存 / Heap / Allocator | ✅ v0.3 已完成 |
| [drivers.md](./drivers.md) | 阶段：驱动框架 / PCI / 磁盘（含 v0.4 中断/时钟/键盘/上下文切换） | ✅ v0.8 已完成（驱动注册框架 + PCI 枚举 + 块设备抽象 + **legacy virtio-blk**，`make test-driver` 22 项 PASS；启动盘已改为 virtio-blk）；v0.4（PIC + PIT 100Hz + PS/2 键盘 + 上下文切换原语）已完成；v0.9 键盘加**行缓冲 + 终端式回显**（`make test-keyboard` 断言同步） |
| [process.md](./process.md) | 阶段：进程/线程 / 上下文切换 / 调度器 / IPC | ✅ v0.5 已完成（线程模型 + 上下文切换 + Round Robin 时间片调度 `make test-scheduler`；IPC pipe 骨架 `make test-ipc`）；channel / shared_memory 仍为占位；v0.6 追加了 `scheduler::suspend()`；**v0.9 第三步：进程对象与 `fork`/`execve`/`wait4` 已落地**（每进程地址空间 + 每进程 64 KiB 内核栈 + 完整用户帧，`make test-proc`，见 1.4） |
| [syscall.md](./syscall.md) | 阶段：系统调用 / 用户空间 / ELF | 🟡 v0.7 核心已完成（`syscall`/`sysret` 入口 + ring3 隔离 + `read/write/open/close`+ `write/exit/getpid/sleep` + **ELF Loader** 从 RAMFS `/init` 按段权限装载），v0.9 已落地三步：**用户堆**（3.5）、**`stdin` + 交互式 Shell**（3.6）、**`fork`/`execve`/`wait4` + `argc`/`argv`**（3.7，外部程序 `/bin/hello` 真的能跑）；`make test-user` 61 项 / `make test-shell` 11 项 / `make test-proc` 8 项 + 4 正则 PASS；只剩 `mprotect`/环境变量/动态链接未做（见 3.4） |
| [filesystem.md](./filesystem.md) | 阶段：VFS / RAMFS / 磁盘文件系统 | ✅ v0.7 + **v0.8 磁盘后端**已完成（VFS trait + 挂载表最长前缀路由 + RAMFS + fd 表；**FAT12/16/32 只读后端** + `tools/mkfs/mkfs.py`，`make test-fs` 20 项 / `make test-fat` 21 项 PASS） |

## 阶段说明

- **第一阶段（已完成）**：确定架构方向、总体路线、里程碑、目录结构，并创建代码文件框架占位。
- **第二阶段（v0.1–v0.2）**：Boot ✅、GDT/IDT/异常 ✅（见 boot.md 实现记录）。
- **第三阶段（v0.3）**：物理/虚拟内存 + 内核堆 ✅（见 memory.md）。
- **第四阶段（v0.4）**：中断控制器 + Timer + 键盘 + 上下文切换原语 + H-04/H-05 Go 工具 ✅（见 drivers.md 第 1 节）。
- **第五阶段（v0.5）**：进程/线程 + Round Robin 调度 + IPC（pipe 骨架）✅（见 process.md）。
- **第六阶段（v0.6–v0.7）**🟡：v0.6（syscall 入口 + ring3 隔离 + 用户态脚手架）与
  v0.7（VFS/RAMFS/fd 表 + 文件 syscall + ELF Loader）均已实现并验收，见 syscall.md 与 filesystem.md。
- **第七阶段（v0.8）**✅：驱动注册框架 + PCI 枚举 + 块设备（legacy virtio-blk）已实现并验收（`make test-driver` 22 项），
  见 drivers.md；磁盘文件系统（**FAT12/16/32 只读后端**）已挂到现有 VFS，H-03 `mkfs` 同步落地（`make test-fat` 21 项），
  见 filesystem.md。之后的 v0.9：Shell + 用户程序 + 网络。
- **第八阶段（v0.9 开局）**🟡：第一步 **用户堆**（`brk`/`mmap`/`munmap` + `libc` 的
  `#[global_allocator]`）已落地并验收（见 syscall.md 3.5）；第二步 **`stdin` + 交互式 Shell**
  已落地并验收 —— 键盘行缓冲 → `read(0)` 整行 → `/bin/sh`（`make test-shell` 11 项，见 syscall.md 3.6）；
  第三步 **进程对象 + `fork`/`execve`/`wait4` + `argc`/`argv`** 已落地并验收 ——
  Shell 能真的跑起外部程序（`/bin/hello`，`make test-proc`，见 syscall.md 3.7 与 process.md 1.4）；
  网络仍为占位。
- **后续**：每进入一个新阶段就编写/修订对应的主题文档；阶段内已落地部分也要及时补记当前状态（例如 v0.4 中断控制器）。
- 本套文档**会在开发过程中被反复修改**——修改前请在对应文档顶部记录修订日期与变更点。
-  本次（2026-10-11，第八轮）修订重点：**v0.9 第三步（进程对象 + `fork`/`execve`/`wait4`）落地** ——
  进程表从「最小登记」换成真正的 PCB（每进程地址空间 + 每进程 64 KiB 内核栈 + 栈底哨兵
  + 停在 syscall 里的完整用户帧）、新增 `syscall/process.rs`（fork 复制地址空间与当前帧、
  execve 换映像并重建 argv 用户栈、wait4 延迟调度 + 回收）、`trampoline` 改为按进程切换；
  Shell 的外部命令改走 `fork`+`execve`+`wait4`，新增外部程序 `/bin/hello`（`user/hello-bin`）；
  新增 `make test-proc`（8 项 + 4 条正则，含 PID 一致性反向引用）与编排 `proc` 场景（11 → **12 场景**）；
  同步 syscall.md（新增 3.7、更新 3.2/3.3/3.4/3.6/4）、process.md（新增 1.4，1.3 阻塞点关闭）、
  libc/README（新增进程调用与 `argc`/`argv` 一节）、project-structure/roadmap/README/DEVELOPMENT。
-  本次（2026-10-11，第七轮）修订重点：**v0.9 第二步（`stdin` + 交互式 Shell）落地** ——
  键盘行缓冲 + 终端式回显、`read(0)` 整行交付（3 秒 `-EAGAIN`）、`user/shell` → RAMFS `/bin/sh`
  （提示符 `root@cerlesse C:>`、内建命令、外部命令真调 `fork()` 暴露 ENOSYS）、新增 `make test-shell`（12 项）
  与编排 `shell` 场景（11 场景）；同步 syscall.md（新增 3.6、3.2/3.3/3.4）、process.md（1.3 阻塞点）、
  drivers.md（键盘行缓冲）、project-structure/roadmap/README/DEVELOPMENT。
-  本次（2026-10-11，第六轮）修订重点：**H-06 P0a（`.ckc` 包容器）落地** —— 内核包后缀定名为
  `.ckc`（CKC = Cerlesse Kernel Container）：64 字节头（magic/版本/头长/flags/长度/偏移/
  32 字节 payload_sha256/保留位）+ 确定性 ustar（归档名 = 目标路径去 `/`、`PKG.toml` 第一、
  按字节序、mtime/uid/gid 全 0、仅常规文件）；身份 = payload 的 SHA-256；
  实现 `tools/pkgmgr/ckc.py`（`pack`/`verify`/`info`）、新增 `make test-pkg`（37 项，含系统 `tar` 独立复核）；
  同步 package-manager.md 3.2/11.1/12.1、DEVELOPMENT.md（H-06 行 + 测试方案）、tools/README.md、
  project-structure.md、README.md。
-  本次（2026-10-11，第五轮）修订重点：**v0.9 第一步（用户堆）落地** ——
  `kernel/src/syscall/memory.rs`（`brk`/`mmap`/`munmap`，W^X + 全零页 + `invlpg`）、
  `address_space.rs` 的堆窗口与匿名 mmap 窗口、`libc/heap.rs`（`#[global_allocator]`）、
  `user/hello` 堆自测与 `user.rs` 页帧泄漏检查；`make test-user` 断言 32 → **59 项**，
  同步 syscall.md（新增 3.5、2.7 表、地址布局）、roadmap/DEVELOPMENT/README/project-structure/libc README。
-  本次（2026-10-11，第四轮）修订重点：**H-06 包管理器提案 v1 完成** ——
  package-manager.md 从「设想」补成可实施规格（七个制品格式、内容哈希 + 三级校验、
  **纯文件安装**的安全模型与威胁对策、事务阶段与回滚、scope 路径白名单与到内核现状的映射、
  CLI 与退出码、MVP 三期、可脚本断言的验收条目），索引状态由「设计中」改为「提案 v1（待原型）」；
  DEVELOPMENT.md 的 H-06 行同步（仍为设计阶段，验收与实现属下一轮）。
-  本次（2026-10-11，第三轮）修订重点：**v0.8 收官** —— FAT12/16/32 只读后端（`fs/fat.rs`）挂到 VFS
  与 H-03 `mkfs`（`tools/mkfs/mkfs.py`）落地并通过 `make test-fat`（21 项）验收后，
  更新 filesystem.md（改为 v0.8 实现记录）、drivers.md（限制项移除）、roadmap/project-structure/DEVELOPMENT
  与索引状态（v0.8 标 ✅，编排场景 9 → 10）。
-  本次（2026-10-11，第二轮）修订重点：v0.8 前半（驱动框架 + PCI 枚举 + 块设备 + virtio-blk legacy）
  验收通过后，把 drivers.md 从「v0.8 尚未编写」更新为实现记录（设计取舍、日志证据、限制）
  并在索引/阶段说明里标明「v0.8 前半完成」。
-  本次（2026-10-11，第一轮）修订重点：v0.6（ring3 + syscall 入口 + 用户态脚手架）与
  v0.7（VFS/RAMFS/文件 syscall/ELF Loader）验收通过后，把 syscall.md 与 filesystem.md 从占位
  更新为实现记录并在索引里标明状态。
  更早（2026-10-08）修订重点：v0.4 与 v0.5 验收全部通过后，把索引状态更新为已完成——
  v0.4（内核侧 PIC / PIT 100Hz / PS/2 键盘 / 上下文切换原语 + 主机侧 H-04/H-05 Go 工具）、
  v0.5（线程模型 + Round Robin 时间片调度 + IPC pipe 骨架），与 DEVELOPMENT.md 任务清单一致。

## 文档维护约定

1. 每份文档顶部保留 `最后更新` / `阶段` / `状态` 三行，便于快速辨识（已有文档逐步对齐）。
2. 设计发生变更时，先改文档再改代码（文档即规格）。
3. 未开始的阶段文档保留占位大纲，不提前编写细节；但阶段内已落地部分应及时补记当前状态，避免文档与代码库状态脱节。
4. 本项目文档会在开发过程中被反复修改，修改前请在对应文档顶部记录修订日期与变更点。
