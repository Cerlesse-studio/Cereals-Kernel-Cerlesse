# 总体开发路线图（第一阶段）

- 阶段：第一阶段 / 路线规划
- 最后更新：2026-10-11
- 状态：🟡 持续更新（2026-10-08 补记 v0.4/v0.5；2026-10-11 补记 v0.6/v0.7 实际进度、v0.8 前半落地与 v0.8 收官）

> 本文档顶部保留阶段/最后更新/状态字段，与项目文档维护约定对齐。

## 1. 总体开发路线

```text
Boot / 启动（Bootloader → CPU初始化 → Kernel入口）
        ↓
Kernel 基础层（GDT / IDT / Exception / Panic）
        ↓
内存管理（Physical → Virtual → Heap → Allocator）
        ↓
中断与时钟（PIC/APIC → Timer → Keyboard）
        ↓
进程 / 线程（Task → Context → Scheduler → IPC）
        ↓
系统调用（syscall → Process → File → Memory）
        ↓
文件系统（VFS → FAT/EXT → File → Directory）
        ↓
驱动系统（PCI → Block → USB → Input → Display）
        ↓
用户空间（ELF → libc → init → shell）
        ↓
网络系统（Ethernet → IP → TCP/UDP → Socket）
        ↓
完整系统（Shell + Utilities + Services + Apps）
```

## 2. 实际开发顺序（严格按依赖关系）

```text
① Bootloader → ② Kernel Entry → ③ Serial Console → ④ GDT/IDT → ⑤ Exception
→ ⑥ Physical Memory → ⑦ Virtual Memory → ⑧ Kernel Heap → ⑨ Interrupt
→ ⑩ Timer → ⑪ Keyboard → ⑫ Task → ⑬ Context Switch → ⑭ Scheduler
→ ⑮ Process → ⑯ Syscall → ⑰ User Mode → ⑱ ELF Loader → ⑲ VFS
→ ⑳ RAMFS → ㉑ Disk Driver → ㉒ PCI → ㉓ Network → ㉔ Shell → ㉕ User Applications
```

**禁止提前做的内容**：GUI、网络、USB、GPU —— 这些都建立在前面的内核基础设施之上。

## 3. 里程碑（v0.1 – v1.0）

| 版本 | 目标 | 对应文档 |
| ---- | ---- | ---- |
| **v0.1 ✅** | Bootloader + Rust Kernel（2026-10-06 QEMU 验收通过） | boot.md |
| **v0.2 ✅** | GDT + IDT + Exception（2026-10-06 验收通过） | boot.md / memory.md |
| **v0.3 ✅** | Physical/Virtual Memory + Heap（2026-10-07 验收通过；高半区映射移至 v0.6，ADR-009） | memory.md |
| **v0.4 ✅** | Interrupt + Timer + Keyboard（2026-10-08 验收通过）；含上下文切换原语 + H-04/H-05 Go 工具（`make go-test` / `make test-orch` 通过） | drivers.md |
| **v0.5 ✅** | Process + Thread + Scheduler + IPC（2026-10-08 验收通过：线程模型 + 上下文切换 + Round Robin 时间片调度 + pipe 骨架，`make test-scheduler` / `make test-ipc` 通过） | process.md |
| **v0.6 🟡** | Syscall + User Space（2026-10-10 核心已验收：`syscall`/`sysret` 入口 + ring3 + 用户地址空间隔离 + `write/exit/getpid/sleep`；2026-10-11 用户态脚手架可用：`libc/` + `user/hello` 真实用户程序跑在 ring3，`make user` / `make test-user` PASS）；ELF Loader / VFS 文件语义 / 进程对象留到 v0.7 | syscall.md |
| **v0.7 🟡** | VFS + RAMFS + ELF（2026-10-11 核心已验收：`FileSystem` trait + 挂载表最长前缀路由 + RAMFS 后端 + fd 表 + `open/read/write/close` + **ELF Loader**（RAMFS `/init` 按段权限逐页装载，`RW` 段对 ring3 可写）；`make test-fs` 19 项 / `make test-user` 32 项 PASS）。Shell 里 `ls/cat/cd` 属 v0.9、磁盘 FS（FAT32）与 `mkfs` 属 v0.8 | filesystem.md / syscall.md |
| **v0.8 ✅** | Driver + PCI + Disk（2026-10-11 验收：驱动注册框架 + PCI 枚举 + 块设备 + **legacy virtio-blk**（`make test-driver` 22 项）；**FAT12/16/32 只读后端挂到 VFS** + H-03 `mkfs`（`make test-fat` 21 项），双盘启动读 `/disk`（启动盘）与 `/mnt/fat`（测试盘）） | drivers.md / filesystem.md |
| **v0.9** 🟡 | Shell + User Programs + Network（**已开局**：运行时前置条件第一步 **用户堆** = `brk`/`mmap`/`munmap` + `libc` 全局分配器，`make test-user` 59 项；下一步 `argc/argv`、进程对象/`fork` 系、动态链接） | syscall.md / drivers.md / filesystem.md |
| **v1.0** | 完整可启动的类 Linux 小型 OS | — |

每个版本的详细任务清单见 [../DEVELOPMENT.md](../DEVELOPMENT.md)。

## 4. 阶段划分（与文档对应）

1. **第一阶段（已完成）**：架构 + 路线 + 结构 + 文件框架。
2. **第二阶段（v0.1–v0.2）**：Boot、GDT/IDT、异常处理 —— 编写 boot.md（✅）。
3. **第三阶段（v0.3）**：物理/虚拟内存 + 内核堆 —— 编写 memory.md（✅）。
4. **第四阶段（v0.4）**：中断控制器 + Timer + 键盘 + 上下文切换原语（含 H-04/H-05 Go 工具） —— 见 drivers.md（✅）。
5. **第五阶段（v0.5）**：进程/线程/调度 + IPC（pipe 骨架） —— 见 process.md（✅）。
6. **第六阶段（v0.6–v0.7）**：系统调用/用户态/文件系统 —— v0.6（syscall 入口 + ring3 + 隔离 + 用户态脚手架）与 v0.7（VFS/RAMFS/fd 表/文件 syscall/ELF Loader）均已落地并验收，见 syscall.md 与 filesystem.md（均 🟡）；剩余的 `fork/exec/wait`、进程对象、用户堆、磁盘 FS 归入 v0.8+。
7. **第七阶段（v0.8–v0.9）**：v0.8 已完成（驱动注册框架 / PCI 枚举 / 块设备 / legacy virtio-blk + **FAT 磁盘后端挂到 VFS** + `mkfs`），
   见 drivers.md 与 filesystem.md（均 ✅）；v0.9 的网络/Shell 仍为占位（⬜ 待编写）。
8. **第八阶段（v1.0）**：整合、安全机制、发布。

## 5. 修订记录

| 日期 | 变更 |
| ---- | ---- |
| 2026-10-06 | 第一阶段初稿：10 个里程碑 + 25 步依赖顺序 |
| 2026-10-06 | v0.1 完成，里程碑表标记 ✅ |
| 2026-10-06 | v0.2 完成（GDT/IDT/异常 + B-01/B-05 修复），里程碑表标记 ✅ |
| 2026-10-07 | v0.3 完成（帧分配器/页表 Mapper/内核堆 + B-06/B-07 修复），里程碑表标记 ✅ |
| 2026-10-07 | 补完 v0.3 后尚未整理的文档：完善 docs/drivers.md / docs/syscall.md / docs/filesystem.md / docs/process.md，补记 DEVELOPMENT.md v0.4 验收项与 docs/README.md 索引状态 |
| 2026-10-08 | v0.4 完成（PIC/PIT 100Hz/PS-2 键盘/上下文切换原语 + 自动化验收），里程碑表标记 ✅ |
| 2026-10-08 | v0.5 内核侧核心完成（线程/PID/就绪队列/Round Robin 时间片抢占 + `make test-scheduler`），里程碑表标记 🟡（IPC 待做）；本次 docs 迭代统一各阶段文档顶部元信息、修订记录与主开发文档（DEVELOPMENT.md）描述一致 |
| 2026-10-08 | v0.4/v0.5 收官复核：`make go-test` / `make test-orch` 实测通过（H-04/H-05 落地），IPC pipe 骨架落代码并通过 `make test-ipc`；v0.4、v0.5 里程碑均标记 ✅ |
| 2026-10-10 | v0.6 核心落地：syscall/sysret 入口 + ring3 + 用户地址空间隔离 + 首批 4 个 syscall，`make test-user` PASS；里程碑标记 🟡（ELF Loader 完整化 / 进程对象留到 v0.7） |
| 2026-10-11 | v0.6 用户态脚手架可用：`libc/` 落地（syscall 封装 + `_start` + `print!/println!`）、真实用户程序 `user/hello` 并入 workspace、`kernel/build.rs` 构建期摊平内嵌、`load_program` 支持多代码页；新增 `make user`，`make test-user` 增至 19 项断言 |
| 2026-10-11 | **v0.8 前半落地**（驱动注册框架 + PCI 枚举 + 块设备 + legacy virtio-blk）：新增 `driver/{driver,pci,block,virtio_blk}.rs` 与 `arch/x86_64/port.rs`、`frame::allocate_contiguous`；启动盘改为 virtio-blk（`disable-modern=on`）；新增 `make test-driver`（22 项）与编排第 9 个场景；该阶段里程碑当时记为 🟡（FAT/`mkfs` 未做，三日内的同一版本内补齐后标 ✅） |
| 2026-10-11 | **v0.7 核心落地**：VFS + RAMFS + fd 表 + `read/open/write/close` + **ELF Loader**（用户程序改为从 RAMFS `/init` 装载，按段权限逐页映射；`build.rs` 不再摊平）；`user.ld` 四段 `ALIGN(0x1000)`；新增 `make test-fs` 与 `tools/check_user_elf.py`，`make test-user` 增至 32 项；v0.7 里程碑标记 🟡（Shell `ls/cat/cd` → v0.9，磁盘 FS + `mkfs` → v0.8） |
| 2026-10-11 | **v0.9 开局（运行时前置条件第一步：用户堆）**：`syscall/memory.rs` 实现 `brk`/`mmap`/`munmap`（256 KiB 堆窗口 + 256 KiB 匿名 mmap 窗口、按需映射、新页全零、W^X、`invlpg`），`libc` 提供 `#[global_allocator]`（bump over mmap）与封装，`user/hello` 加入堆自测，`user.rs` 新增页帧泄漏检查；`make test-user` 32 → **59 项** PASS，`make test-orch` 10 场景 PASS；v0.9 里程碑标记 🟡（Shell/网络仍未做） |
| 2026-10-11 | **v0.8 收官**（FAT 磁盘后端 + H-03 `mkfs`）：`fs/fat.rs`（FAT12/16/32 只读：BPB 按簇数定类型、固定根目录区/簇链目录、8.3 + NT 标志 + LFN 校验和、任意偏移读、`EROFS`/`EUCLEAN`）挂到 VFS（`fs::mount_disks` → `/disk` 启动盘、`/mnt/fat` 测试盘，验证最长前缀路由）；`FileSystem` trait 改为 `&mut self` + `read_all -> Vec<u8>`；`tools/mkfs/mkfs.py` 自己生成 FAT12/16/32 镜像并由 mtools 独立复核；新增 `make fat-image` / `make test-fat`（21 项，双盘）与编排第 10 个场景；v0.8 里程碑标记 ✅ |
