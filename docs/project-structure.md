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
│   │   │                        #    ✅ v0.9 第一步：memory（brk/mmap/munmap，用户堆）
│   │   │                        #    ✅ v0.9 第二步：file::read_stdin（键盘行缓冲 → fd 0）
│   │   │                        #    ✅ v0.9 第三步：process 的 fork/execve/wait4 + argv 用户栈
│   │   │                        #    ✅ v0.9 第五步：dir（getdents）+ process::getprocs（目录/进程表枚举）
│   │   ├── fs/                  # ✅ v0.7–v0.8：vfs/inode/mount/ramfs/file + fat（FAT12/16/32 只读后端）
│   │   ├── driver/              # ✅ v0.8：driver（注册框架）/pci/block/virtio_blk + serial（v0.1）
│   │   │                        #    ✅ v0.9：keyboard 加行缓冲与行编辑（v0.4 起为 IRQ1 回显）
│   │   │                        #    ✅ v0.9 第四步：net（NetworkDevice 抽象）+ virtio_net（legacy 网卡驱动）
│   │   ├── time/                # timer/clock
│   │   ├── ipc/                 # pipe/channel/shared_memory
│   │   ├── network/             # ✅ v0.9 第四步：ethernet/arp/ipv4/udp/dhcp + mod（Stack + selftest）
│   │   │                        #    tcp/socket 仍为占位（完整 TCP/IP 是第一版非目标）
│   │   ├── security/            # permission/credential/capability
│   │   └── main.rs              # 内核入口（v0.1 ✅：_start + kernel_main）
│   ├── link.ld                  # 链接脚本：装载到 32MiB（ADR-007）
│   ├── build.rs                 # 注入链接脚本 + 构建用户程序并校验/内嵌其 ELF（v0.7）
│   └── Cargo.toml
│
├── user/                        # 用户态程序（v0.6 起加入 workspace）
│   ├── hello/                   # ✅ v0.6/v0.7：真实 Ring3 用户程序（libc + user.ld + build.rs）
│   │                            #   构建产物由内核内嵌为 RAMFS 上的 /init
│   ├── shell/                   # ✅ v0.9：交互式 Shell（提示符 root@cerlesse C:>、内建命令、
│   │                            #    外部命令走 fork/execve/wait4），内嵌为 RAMFS 上的 /bin/sh
│   ├── hello-bin/               # ✅ v0.9 第三步：**外部程序**（打印 pid/argc/argv 后 exit(1)），
│   │                            #    内嵌为 RAMFS 上的 /bin/hello —— 不由内核直接运行，只被 Shell 跑起来
│   ├── ls/                      # ✅ v0.9 第五步：/bin/ls（getdents 列举目录，带 kind/size）
│   ├── cat/                     # ✅ v0.9 第五步：/bin/cat（open/read/write/close 打印文件）
│   ├── echo/                    # ✅ v0.9 第五步：/bin/echo（argc/argv + 一次 write）
│   ├── ps/                      # ✅ v0.9 第五步：/bin/ps（getprocs 打印进程表）
│   └── init/                    # v0.9 占位（内核实际跑的 /init 是 user/hello 的构建产物）
│
├── libc/                        # ✅ v0.6/v0.7：最小用户态运行时（syscall 封装 / _start / print!）
│                                #    ✅ v0.9 第一步：heap（#[global_allocator]，bump over mmap）
│                                #    ✅ v0.9 第二步：EAGAIN 与 read(0) 行缓冲语义
│                                #    ✅ v0.9 第三步：args（argc/argv）与可用的 fork/execve/wait4
│                                #    ✅ v0.9 第五步：getdents/getprocs 与 dirent_*/procinfo_* 解析器
│
├── tools/                       # 主机侧工具
│   ├── image-builder/           # Python：磁盘镜像组装
│   ├── check_user_elf.py        # ✅ v0.7：用户程序 ELF 段布局检查（不启 QEMU）
│   ├── mkfs/                    # ✅ v0.8：FAT12/16/32 镜像生成（mkfs.py，mtools 独立复核）
│   └── pkgmgr/                  # ✅ H-06 P0a：CKC 包容器（ckc.py，后缀 .ckc；tar 独立复核）
│
├── tools-go/                    # Go：测试编排 / 串口日志分析等主机服务
│
├── tests/                       # 集成测试（Python 驱动 QEMU）
│   └── fatroot/                 # ✅ v0.8：FAT 测试盘的内容（data/）与复核说明
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
| `kernel/src/syscall/` | 系统调用入口/分发、用户地址空间、ELF Loader、首批与文件 syscall、**用户堆（`memory.rs`：`brk`/`mmap`/`munmap`）**、**进程调用（`process.rs`：`fork`/`execve`/`wait4` + argv 用户栈 + `getprocs`）**、**目录枚举（`dir.rs`：`getdents`）** | v0.6–v0.7 ✅ / v0.9 第一·三·四步 ✅ |
| `kernel/src/fs/` | VFS 抽象（trait + 路径工具）、挂载表路由、RAMFS、fd 表、**FAT12/16/32 只读后端** | v0.7 ✅ / v0.8 ✅ |
| `kernel/src/driver/` | 串口（v0.1）、键盘（v0.4）、**驱动注册框架 / PCI 枚举 / 块设备抽象 / virtio-blk**（v0.8 ✅） | v0.4 / v0.8 |
| `kernel/src/driver/{pci,driver,block,virtio_blk}.rs` | `pci` 只回答「总线上有什么」；`driver` 回答「谁来管它」（`Driver` trait + `probe_all`）；`block` 是上层唯一可见的按扇区接口；`virtio_blk` 是具体实现（同时实现 `Driver` 与 `BlockDevice`） | v0.8 ✅ |
| `kernel/src/fs/fat.rs` | 块设备上的 FAT12/16/32 **只读**后端（第二个 `FileSystem` 实现）：BPB 解析、固定根目录区/簇链目录、8.3 + NT 标志 + LFN、任意偏移读；写操作 `EROFS`；损坏结构 `EUCLEAN` | v0.8 ✅ |
| `kernel/src/memory/frame.rs` | 位图帧分配器；v0.8 追加 `allocate_contiguous` / `free_contiguous`（virtqueue 与 DMA 要求的物理连续缓冲） | v0.3 ✅ / v0.8 ✅ |
| `kernel/src/arch/x86_64/port.rs` | 端口 I/O 统一入口：`inb/inw/inl` + `outb/outw/outl`（PCI 配置空间与 legacy virtio 寄存器窗口依赖 32 位读写） | v0.8 ✅ |
| `kernel/src/ipc/` | 管道、通道、共享内存 | v0.5+ |
| `kernel/src/network/` | 以太网 → IP → TCP/UDP → Socket | v0.9 |
| `kernel/src/security/` | 权限、凭证、Capability | v1.0 |
| `libc/` | 最小用户态运行时：syscall 封装（含 `open/read/close`、`brk/mmap/munmap` 与 `fork/execve/wait4`）、`_start`（入口寄存器 `argc`/`argv`）、`print!/println!`、panic handler、`heap`（`#[global_allocator]`）、`args` | v0.6 ✅ / v0.7 ✅ / v0.9 ✅ |
| `user/` | 用户程序（`hello` ✅ v0.6/v0.7 → `/init`；`shell` ✅ v0.9 → `/bin/sh`；`hello-bin` ✅ v0.9 第三步 → `/bin/hello`（外部程序）；**`ls` / `cat` / `echo` / `ps` ✅ v0.9 第五步 → `/bin/*`**；`init/` 仍为占位） | v0.6–v0.9 |
| `tools/`（Python） | 镜像构建、用户程序 ELF 检查（v0.7）、`mkfs/` FAT 镜像生成（v0.8 ✅）、**`pkgmgr/ckc.py`（CKC 包容器 `.ckc`，H-06 P0a ✅）** | v0.1 起 |
| `tools-go/`（Go） | 测试编排、串口日志服务 | v0.4 起 |
| `tests/` | QEMU 集成测试 | v0.1 起 |

## 3. 修订记录

| 日期 | 变更 |
| ---- | ---- |
| 2026-10-11 | v0.9 第五步（基础用户程序）：`kernel/src/syscall/dir.rs`（`getdents`）与 `process.rs::sys_getprocs` 落地；`user/{ls,cat,echo,ps}` 从占位变为实现（RAMFS `/bin/*`），`kernel/build.rs` 的程序清单 3 → 7；`libc/` 新增 `getdents`/`getprocs` 与 `dirent_*`/`procinfo_*` 解析器 —— 目录树、模块表与 `libc/` 描述同步 |
| 2026-10-11 | v0.9 第三步（进程对象与外部程序）：`kernel/src/syscall/process.rs` 的 `fork`/`execve`/`wait4` 落地，`process/process.rs` 换成真正的 PCB（每进程内核栈），`trampoline` 改为按进程切换；`libc/` 新增 `args.rs`；新增 `user/hello-bin`（RAMFS `/bin/hello`）—— 目录树、模块表与 `libc/` 描述同步 |
| 2026-10-11 | v0.9 第二步（交互式 Shell）：`user/shell` 从占位变为实现（首个 `/init` 之外的用户程序，RAMFS 上的 `/bin/sh`），`kernel/build.rs` 由「单个用户程序」改为「按列表构建/校验/内嵌」，`fs::init` 增加 `/bin`；`driver/keyboard.rs` 加行缓冲（目录树注释同步） |
| 2026-10-06 | 第一阶段初稿，同步创建代码文件框架占位 |
| 2026-10-06 | v0.1 落地：新增 `shared/` crate、`link.ld`、`build.rs`，boot/kernel 标记完成 |
| 2026-10-07 | v0.3 落地：`memory/` 与 `arch/x86_64/{paging,cpu}.rs` 实现完成；link.ld 增加 `__kernel_start/__kernel_end` |
| 2026-10-08 | 本次 docs 迭代统一文档顶部元信息（阶段 / 最后更新 / 状态）与修订记录，保持与 DEVELOPMENT.md 与索引文档描述一致 |
| 2026-10-08 | 按开发文档流程继续规划/占位更新：v0.5 IPC 模块（pipe/channel/shared_memory）位置已保留，pipe 优先；v0.6 syscall 模块细化路径已补充至 syscall.md，让后续可直接开工 |
| 2026-10-11 | v0.6 用户态脚手架落地：`libc/` 与 `user/hello` 从占位变为实现并加入 workspace；`kernel/build.rs` 新增构建期用户程序构建与 ELF 摊平；workspace 新增 `user-release` profile 与 `default-members` |
| 2026-10-11 | v0.7 落地：`kernel/src/fs/` 从占位变为实现（`vfs`/`inode`/`mount`/`ramfs`/`file`），`kernel/src/syscall/` 新增 `elf.rs` 与文件 syscall；`kernel/build.rs` 由「摊平内嵌」改为「校验 ELF 后原样内嵌」（RAMFS `/init`）；新增 `tools/check_user_elf.py`（`make user` 验段布局） |
| 2026-10-11 | v0.8 前半落地：`kernel/src/driver/` 新增 `pci.rs` / `driver.rs` / `block.rs` / `virtio_blk.rs`（模块表里补上分层职责），`kernel/src/arch/x86_64/port.rs`（端口 I/O），`memory/frame.rs` 追加连续帧分配；`user/{init,shell,ls,...}` 仍为占位 |
| 2026-10-11 | H-06 P0a：`tools/` 新增 `pkgmgr/ckc.py`（CKC 包容器，后缀 `.ckc`）；目录树与模块表同步标注 |
| 2026-10-11 | v0.9 第一步：`kernel/src/syscall/` 新增 `memory.rs`（用户堆 syscall），`libc/` 新增 `heap.rs`（`#[global_allocator]`）；目录树与模块表同步标注 |
| 2026-10-11 | v0.8 收官：`kernel/src/fs/` 新增 `fat.rs`（块设备上的 FAT 只读后端，模块表补一行）；`tools/mkfs/` 从占位变为实现（`mkfs.py`）；`tests/fatroot/`（测试盘内容 + mtools 复核说明）；`tests/test_boot.py` 支持 `--disk2`（双盘启动） |
