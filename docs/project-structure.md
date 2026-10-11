# 项目目录结构（第一阶段）

- 阶段：第一阶段 / 结构规划
- 最后更新：2026-10-11
- 状态：✅ 已完成（代码框架已按本结构创建占位文件；占位文件内标注了计划实现的版本号；2026-10-08 本次 docs 迭代统一元信息与修订记录）

> 本文档顶部保留阶段 / 最后更新 / 状态字段，与项目文档维护约定对齐。

## 1. 完整目录树

```text
Cerlesse/
│
├── shared/                      # boot/kernel 共享数据结构（v0.1 ✅）
│   └── src/lib.rs               # shared::BootInfo、BOOT_MAGIC
│
├── boot/                        # Bootloader crate（v0.1 ✅）
│   ├── src/                     # main.rs（UEFI 入口+ELF 装载）、serial.rs、uefi.rs
│   └── Cargo.toml
│
├── kernel/                      # 内核主体（Rust, no_std）
│   ├── src/
│   │   ├── arch/x86_64/         # 架构相关：boot/cpu/gdt/idt/interrupt/paging/context/port（v0.8 端口 I/O）
│   │   ├── memory/              # frame/page/heap/allocator
│   │   ├── interrupt/           # exception/irq/controller
│   │   ├── process/             # process/thread/context/pid
│   │   ├── scheduler/           # scheduler/queue/round_robin
│   │   ├── syscall/             # ✅ v0.6–v0.7：trampoline/address_space/elf/syscall_table/process/file/user
│   │   ├── fs/                  # ✅ v0.7：vfs/inode/mount/ramfs/file
│   │   ├── driver/              # ✅ v0.8：driver（注册框架）/pci/block/virtio_blk + serial（v0.1）/keyboard（v0.4）
│   │   ├── time/                # timer/clock
│   │   ├── ipc/                 # pipe/channel/shared_memory
│   │   ├── network/             # ethernet/arp/ipv4/udp/tcp/socket
│   │   ├── security/            # permission/credential/capability
│   │   └── main.rs              # 内核入口（v0.1 ✅：_start + kernel_main）
│   ├── link.ld                  # 链接脚本：装载到 32MiB（ADR-007）
│   ├── build.rs                 # 注入链接脚本 + 构建用户程序并校验/内嵌其 ELF（v0.7）
│   └── Cargo.toml
│
├── user/                        # 用户态程序（v0.6 起加入 workspace）
│   ├── hello/                   # ✅ v0.6/v0.7：真实 Ring3 用户程序（libc + user.ld + build.rs）
│   │                            #   构建产物由内核内嵌为 RAMFS 上的 /init
│   └── init/  shell/  ls/  cat/  echo/  ps/   # v0.9 占位
│
├── libc/                        # ✅ v0.6/v0.7：最小用户态运行时（syscall 封装 / _start / print!）
│
├── tools/                       # 主机侧工具
│   ├── image-builder/           # Python：磁盘镜像组装
│   ├── check_user_elf.py        # ✅ v0.7：用户程序 ELF 段布局检查（不启 QEMU）
│   └── mkfs/                    # Python：文件系统镜像生成（v0.8）
│
├── tools-go/                    # Go：测试编排 / 串口日志分析等主机服务
│
├── tests/                       # 集成测试（Python 驱动 QEMU）
│
├── docs/                        # 全部开发文档（见 docs/README.md）
│
├── Cargo.toml                   # Cargo Workspace（shared/boot/kernel/libc/user/*，含 user-release profile）
├── Makefile                     # 构建/QEMU/测试入口（v0.1 ✅）
├── DEVELOPMENT.md               # 主开发文档
└── README.md
```

## 2. 模块职责速查

| 目录 | 职责 | 计划版本 |
| ---- | ---- | ---- |
| `shared/` | BootInfo 等跨 boot/kernel 纯数据结构 | v0.1 ✅ |
| `boot/` | UEFI 引导、内存检测、装载内核、传递 BootInfo | v0.1 ✅ |
| `kernel/src/arch/x86_64/` | GDT、IDT、控制寄存器（`cpu.rs`）、页表操作（`paging.rs`）、上下文切换 | v0.1–v0.3 ✅ |
| `kernel/src/memory/` | 物理帧分配、虚拟内存映射、内核堆、全局分配器 | v0.3 ✅ |
| `kernel/src/interrupt/` | 异常处理、IRQ 分发、PIC/APIC | v0.2 / v0.4 |
| `kernel/src/time/` | PIT/APIC Timer、系统时钟、sleep/yield | v0.4 |
| `kernel/src/process/` | 进程/线程模型、PID、状态机 | v0.5 |
| `kernel/src/scheduler/` | 调度队列、Round Robin | v0.5 |
| `kernel/src/syscall/` | 系统调用入口/分发、用户地址空间、ELF Loader、首批与文件 syscall | v0.6–v0.7 ✅ |
| `kernel/src/fs/` | VFS 抽象（trait + 路径工具）、挂载表路由、RAMFS、fd 表 | v0.7 ✅（磁盘后端 v0.8） |
| `kernel/src/driver/` | 串口（v0.1）、键盘（v0.4）、**驱动注册框架 / PCI 枚举 / 块设备抽象 / virtio-blk**（v0.8 ✅） | v0.4 / v0.8 |
| `kernel/src/driver/{pci,driver,block,virtio_blk}.rs` | `pci` 只回答「总线上有什么」；`driver` 回答「谁来管它」（`Driver` trait + `probe_all`）；`block` 是上层唯一可见的按扇区接口；`virtio_blk` 是具体实现（同时实现 `Driver` 与 `BlockDevice`） | v0.8 ✅ |
| `kernel/src/memory/frame.rs` | 位图帧分配器；v0.8 追加 `allocate_contiguous` / `free_contiguous`（virtqueue 与 DMA 要求的物理连续缓冲） | v0.3 ✅ / v0.8 ✅ |
| `kernel/src/arch/x86_64/port.rs` | 端口 I/O 统一入口：`inb/inw/inl` + `outb/outw/outl`（PCI 配置空间与 legacy virtio 寄存器窗口依赖 32 位读写） | v0.8 ✅ |
| `kernel/src/ipc/` | 管道、通道、共享内存 | v0.5+ |
| `kernel/src/network/` | 以太网 → IP → TCP/UDP → Socket | v0.9 |
| `kernel/src/security/` | 权限、凭证、Capability | v1.0 |
| `libc/` | 最小用户态运行时：syscall 封装（含 `open/read/close`）、`_start`、`print!/println!`、panic handler | v0.6 ✅ / v0.7 ✅ |
| `user/` | 用户程序（`hello` ✅ v0.6/v0.7，其 ELF 即 RAMFS 上的 `/init`；init/shell/ls/cat/echo/ps 待 v0.9） | v0.6–v0.9 |
| `tools/`（Python） | 镜像构建、用户程序 ELF 检查（v0.7）、mkfs（v0.8） | v0.1 起 |
| `tools-go/`（Go） | 测试编排、串口日志服务 | v0.4 起 |
| `tests/` | QEMU 集成测试 | v0.1 起 |

## 3. 修订记录

| 日期 | 变更 |
| ---- | ---- |
| 2026-10-06 | 第一阶段初稿，同步创建代码文件框架占位 |
| 2026-10-06 | v0.1 落地：新增 `shared/` crate、`link.ld`、`build.rs`，boot/kernel 标记完成 |
| 2026-10-07 | v0.3 落地：`memory/` 与 `arch/x86_64/{paging,cpu}.rs` 实现完成；link.ld 增加 `__kernel_start/__kernel_end` |
| 2026-10-08 | 本次 docs 迭代统一文档顶部元信息（阶段 / 最后更新 / 状态）与修订记录，保持与 DEVELOPMENT.md 与索引文档描述一致 |
| 2026-10-08 | 按开发文档流程继续规划/占位更新：v0.5 IPC 模块（pipe/channel/shared_memory）位置已保留，pipe 优先；v0.6 syscall 模块细化路径已补充至 syscall.md，让后续可直接开工 |
| 2026-10-11 | v0.6 用户态脚手架落地：`libc/` 与 `user/hello` 从占位变为实现并加入 workspace；`kernel/build.rs` 新增构建期用户程序构建与 ELF 摊平；workspace 新增 `user-release` profile 与 `default-members` |
| 2026-10-11 | v0.7 落地：`kernel/src/fs/` 从占位变为实现（`vfs`/`inode`/`mount`/`ramfs`/`file`），`kernel/src/syscall/` 新增 `elf.rs` 与文件 syscall；`kernel/build.rs` 由「摊平内嵌」改为「校验 ELF 后原样内嵌」（RAMFS `/init`）；新增 `tools/check_user_elf.py`（`make user` 验段布局） |
| 2026-10-11 | v0.8 前半落地：`kernel/src/driver/` 新增 `pci.rs` / `driver.rs` / `block.rs` / `virtio_blk.rs`（模块表里补上分层职责），`kernel/src/arch/x86_64/port.rs`（端口 I/O），`memory/frame.rs` 追加连续帧分配；`tools/mkfs/` 与 `user/{init,shell,ls,...}` 仍为占位 |
