# Cerlesse OS 文档中心

- 阶段：第一阶段 / 文档中心
- 最后更新：2026-10-11
- 状态：🟡 持续更新（2026-10-11 同步 v0.8 收官（驱动/PCI/块设备 + **磁盘 FAT 后端** + `mkfs`）实现记录）

> 本文档顶部保留阶段/最后更新/状态字段，与项目文档维护约定对齐。

> 项目代号：**Cerlesse**（仓库：Cereals-Kernel-Cerlesse）
> 定位：使用 Rust 从零开发的类 Linux 高性能宏内核，支持 Linux / Windows 主机交叉开发（x86_64 优先）。
> 语言策略：**Rust 编写内核核心**，Python 用于构建/测试工具链，Go 用于主机侧辅助服务。

## 文档索引

| 文档 | 内容 | 状态 |
| ---- | ---- | ---- |
| [architecture.md](./architecture.md) | 第一阶段：内核架构、语言分工、目标与非目标 | ✅ 已完成（会随开发反复修订） |
| [lang-selection.md](./lang-selection.md) | 按功能领域汇总的性能最佳语言对照（辅助选型参考） | ✅ 新增（2026-10-07） |
| [package-manager.md](./package-manager.md) | 面向内核/系统生态的高效、安全、轻量、灵活包管理器设计提案 | 设计中 |
| [project-structure.md](./project-structure.md) | 目录结构与模块职责划分 | ✅ 已完成 |
| [../DEVELOPMENT.md](../DEVELOPMENT.md) | 主开发文档：需求、依赖、任务清单、测试、QEMU、Issue 规划 | ✅ 已完成 |
| [boot.md](./boot.md) | 阶段：Bootloader / UEFI / Kernel Entry / GDT / IDT | ✅ v0.1 + v0.2 已完成 |
| [memory.md](./memory.md) | 阶段：物理/虚拟内存 / Heap / Allocator | ✅ v0.3 已完成 |
| [drivers.md](./drivers.md) | 阶段：驱动框架 / PCI / 磁盘（含 v0.4 中断/时钟/键盘/上下文切换） | ✅ v0.8 已完成（驱动注册框架 + PCI 枚举 + 块设备抽象 + **legacy virtio-blk**，`make test-driver` 22 项 PASS；启动盘已改为 virtio-blk）；v0.4（PIC + PIT 100Hz + PS/2 键盘 + 上下文切换原语）已完成 |
| [process.md](./process.md) | 阶段：进程/线程 / 上下文切换 / 调度器 / IPC | ✅ v0.5 已完成（线程模型 + 上下文切换 + Round Robin 时间片调度 `make test-scheduler`；IPC pipe 骨架 `make test-ipc`）；channel / shared_memory 仍为占位；v0.6 追加了 `scheduler::suspend()`（用户态执行期挂起调度） |
| [syscall.md](./syscall.md) | 阶段：系统调用 / 用户空间 / ELF | 🟡 v0.7 核心已完成（`syscall`/`sysret` 入口 + ring3 隔离 + `read/write/open/close`+ `write/exit/getpid/sleep` + **ELF Loader** 从 RAMFS `/init` 按段权限装载）；`make test-user` 32 项 PASS；用户堆/`argc/argv`/`fork` 系未做（见 3.4） |
| [filesystem.md](./filesystem.md) | 阶段：VFS / RAMFS / 磁盘文件系统 | ✅ v0.7 + **v0.8 磁盘后端**已完成（VFS trait + 挂载表最长前缀路由 + RAMFS + fd 表；**FAT12/16/32 只读后端** + `tools/mkfs/mkfs.py`，`make test-fs` 19 项 / `make test-fat` 21 项 PASS） |

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
- **后续**：每进入一个新阶段就编写/修订对应的主题文档；阶段内已落地部分也要及时补记当前状态（例如 v0.4 中断控制器）。
- 本套文档**会在开发过程中被反复修改**——修改前请在对应文档顶部记录修订日期与变更点。
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
