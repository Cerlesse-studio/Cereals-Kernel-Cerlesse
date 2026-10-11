# Cereals-Kernel-Cerlesse

> 代号 **Cerlesse OS** — 使用 Rust 从零开发的类 Linux 高性能宏内核。
> 内核核心：**Rust**；主机侧工具：**Python**（构建/测试）+ **Go**（编排/日志服务）。
> 支持 Linux / Windows 主机交叉开发，目标平台 x86_64（UEFI + QEMU 优先）。

## 当前进度

**🔗 动态链接的最小闭环已跑通（2026-10-11，v0.9 第六步：内核侧最小动态链接）**：内核第一次装载了 `ET_DYN` ——
主程序 `/bin/dynhello`（PIE，链接基址 0）带 `PT_INTERP=/lib/ld.so` 与 `DT_NEEDED=libgreet.so`：
内核解析这两条、从 RAMFS 的 `/lib` 读入解释器与共享库、把它们依次装进**共享库窗口**（基址 + 1 MiB、32 页），
再应用 `R_X86_64_RELATIVE`（主程序 `.data` 里的绝对指针）与 `R_X86_64_JUMP_SLOT`（`call greet@PLT` 的 GOT 槽）
—— 然后 `iretq` 进 ring3。两条输出证明代码真的跑到了：`main: dynamic link ok`（主程序自己的 `.rodata`）与
`greet: hello from libgreet.so`（共享库的 `.rodata`），退出码就是 `greet()` 返回的 **42**。
验收：`make test-dyn`（**10 项**，从 `PT_INTERP` 到 `frames free` 无泄漏的整条链）；
独立复核对齐 `mkfs`+mtools 的思路：`make dyn-hostcheck` 用**宿主机的 glibc `ld.so`** 跑同一批 `.s`
（`as`+`ld` 重编），两套完全不同的装载器要跑出相同输出与退出码。
边界（刻意）：链接仍由**内核**做，`PT_INTERP` 指向的解释器被装载但不被进入 —— 真正的用户态 `ld.so`
需要一个能 `mmap` 文件的内核，属后续版本；完整动态链接仍是第一版非目标。规格见 docs/syscall.md 3.9。

**🧰 基础用户程序已可用（2026-10-11，v0.9 第五步：`ls` / `cat` / `echo` / `ps`）**：`/bin` 里不再只有一个 `hello`。
四个命令行程序都是**普通用户程序**（Rust + libc，由 Shell 的 `fork`+`execve` 起来），各自只需要一条内核能力：
`ls` → 新增的 **`getdents`**（把 VFS 的目录项写成定长 88 字节记录）、`ps` → 新增的 **`getprocs`**
（定长 40 字节的进程表快照；Linux 没有这个调用，所以放在 **256** 起的 Cerlesse 扩展号段，
`syscall_table.rs` 因此写明「`0..=255` = Linux 语义、`>= 256` = Cerlesse 扩展」）、
`cat` → 文件 syscall、`echo` → `argc`/`argv` + 一次 `write`。
`ps` 于是能读出 `pid=3 ppid=0 state=RUNNING comm=sh` 与 `pid=7 ppid=3 … comm=ps` —— 这正是 `fork` 建出的父子关系
（`Process::comm` 是新增的定长 16 字节字段，`execve` 成功后改写为程序名）。
验收：`make test-programs`（**17 项 + 5 条正则**，QEMU monitor 注入 `ls` / `ls /bin` / `ps` / `/bin/echo …` / `/bin/cat /etc/motd`）——
其中 `ls: /bin entries=7` 说明 RAMFS 上恰好是本次构建的 7 个程序（`sh`/`hello`/`ls`/`cat`/`echo`/`ps` + 第六步的 `dynhello`），
`cat: /etc/motd bytes=23` 说明程序端真的读到了内核提供的文件。
规格见 docs/syscall.md 3.8。注：内建命令会**遮蔽**同名程序（与真实 shell 一致），所以 `cat`/`echo` 要走程序得写 `/bin/cat` / `/bin/echo`；
`make test-proc` 的“找不到的程序”负例已从 `ls` 改为 `foo`。

**🌐 简化网络栈已打通（2026-10-11，v0.9 第四步）**：内核第一次真的收发了网络报文。
QEMU 里加一块 legacy `virtio-net-pci`（`-netdev user` = slirp，自带 DHCP 服务器与虚拟网关），启动后内核：
① 从**设备配置空间**读到 MAC（协商了 `VIRTIO_NET_F_MAC`；驱动的兜底地址刻意不同，因此日志里的
`52:54:00:12:34:56` 只可能是真读到的）；② ARP 广播请求解析网关 → `10.0.2.2 -> 52:55:0a:00:02:02`；
③ DHCP DISCOVER（UDP 68→67 广播）→ OFFER（UDP 67→68）往返，解析出 `yiaddr`/server/mask/router。
代码：`kernel/src/driver/net.rs` + `driver/virtio_net.rs`（RX/TX 双队列、10 字节 virtio-net 头、同步轮询）
与 `kernel/src/network/{ethernet,arp,ipv4,udp,dhcp}.rs`（Ethernet II → ARP → IPv4 → UDP，含校验和）。
验收：`make test-network`（**13 项**串口断言，**主机侧不需要跑任何服务**）。
TCP/ICMP 与面向用户态的 socket 仍是占位（见 docs/drivers.md 1.8）。

**H-06 包管理器 P0a 已落地（2026-10-11）**：内核包有了**独有后缀 `.ckc`**
（**CKC** = Cerlesse Kernel Container）—— 64 字节头（magic `CKC\x01` / 版本 / 头长 / flags /
payload_len / payload_off / 32 字节 payload_sha256 / 保留位，小端）+ **确定性 ustar** payload
（归档名 = 安装目标路径去 `/`、`PKG.toml` 第一、其余按字节序、mtime/uid/gid 全 0、仅常规文件、不压缩）。
**身份 = payload 的 SHA-256**（换算成 `rev`），因此同一棵目录树在任何机器上打包结果逐字节相同。
实现：`tools/pkgmgr/ckc.py`（纯标准库 `pack`/`verify`/`info`，退出码 0/1/2/4）；
验收：`make test-pkg`（`tests/test_ckc.py`，**37 项**，含用系统 `tar` 读同一段 payload 的
**独立复核**）；规格见 docs/package-manager.md 3.2。

**🧩 外部程序真的能跑了（2026-10-11，v0.9 第三步：进程对象 + `fork`/`execve`/`wait4`）**：
Shell 里的 `hello world` 现在会：`fork()`（复制 Shell 的地址空间与当前寄存器帧）→ 子进程
`execve("/bin/hello", argv)`（换映像、把 `argv` 写进新进程的用户栈）→ 父进程 `wait4(pid)`
（恢复子进程运行、回收它的地址空间、拿走退出码）。
关键不是这三个号“有实现”，而是它们背后终于有了**进程对象**：每进程一棵页表 +
**每进程 64 KiB 内核栈**（栈底有哨兵 `KERNEL STACK OVERFLOW` 告警）+ 停在 `syscall` 里的完整用户帧。
验收：`make test-proc`（**8 项断言 + 4 条正则**，QEMU monitor 注入 `hello world` / `foo` / `exit`）——
用**反向引用**把 fork / execve / wait4 三处的 PID 断言为**同一个值**（PID 数值依赖启动顺序，写常数会碎），
并断言 `user: frames free before=N after=N`：父子进程的地址空间**全部归还**。
找不到的程序（`foo` → `/bin/foo`；v0.9 第五步起 `/bin/ls` 已存在，不能再当负例）也是完整链路：
execve 返回 `-ENOENT`，子进程 `exit(127)`，父进程拿到 127。
设计、状态机与两条实测踩坑（debug 构建下 16 KiB 内核栈静默溢出踩坏 `.bss` 帧分配器；退出时必须先恢复 CR3 再 Drop 地址空间）见 docs/process.md 1.4 与 docs/syscall.md 3.7。

**🐚 交互式 Shell 已可用（2026-10-11，v0.9 第二步）**：`/bin/sh` 不再是占位。
键盘行缓冲（`driver/keyboard.rs`：行编辑 + 终端式回显 + `kbd: line ready len=N`）→
`read(0)`（`syscall/file.rs::read_stdin`：**整行交付**，行尾 `\n`；3 秒无整行返回 `-EAGAIN`）→
`user/shell`：提示符按默认加载样式 **`root@cerlesse C:>`**（用户名@主机名 盘符>，一次 `write`），
内建 `help` / `echo` / `cat` / `pid` / `exit`，执行真实文件 syscall（`cat /etc/motd` 走 `open/read/write/close`），
外部命令走上面的 `fork`+`execve`+`wait4`。
验收：`make test-shell`（**11 项**，QEMU monitor 注入真实按键：`help` → `cat /etc/motd` → `exit`）；
`make test-user` 增至 **61 项**（含非交互路径的 Shell 步骤）；规格见 docs/syscall.md 3.6/3.7。

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
set 1 解码（字母/数字/Shift/扩展前缀）→ 串口回显（v0.9 起为**行缓冲 + 终端式原样回显**，
`Enter` 交付整行并打印 `kbd: line ready len=N`）→
内核态上下文切换原语（callee-saved + 栈切换，含内核自测 `context: switch PASS`）。

主机侧（H-04 testorch / H-05 serialmon）：`make go-test`（`go vet` + `go test`）PASS；
`make test-orch`（Go 编排器并行跑 6 个集成测试场景 + 串口二次复核）PASS（6 passed / 0 failed）。

自动化验证（内核侧）：`make test` 回归 PASS；`make test-exception`（#DE）PASS；
`make test-memory`（堆/帧/页表 + 上下文切换自测）PASS；`make test-pagefault`（#PF 诊断 + CR2）PASS；
`make test-keyboard`（经 QEMU monitor 注入真实按键，断言 `KBIRQ ENTRY` / `kbd: line ready len=3` / `KBIRQ EXIT` / 心跳）PASS；
`make test-shell`（注入 `help`/`cat /etc/motd`/`exit`，断言提示符、`read fd=0`、`sh: cat /etc/motd bytes=23` 与 `user: shell PASS`）PASS；
`make test-proc`（注入 `hello world`/`foo`/`exit`，断言 fork/execve/wait4 全链路与 PID 一致性、`user: selftest PASS`）PASS；
`make test-programs`（注入 `ls`/`ls /bin`/`ps`/`/bin/echo`/`/bin/cat /etc/motd`，断言 `getdents`/`getprocs` 与四个程序真的被 exec 起来，17 项 + 5 正则）PASS。

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
- **fd 表与文件 syscall**（`fs/file.rs` + `syscall/file.rs`）：0/1/2 保留（stdin = 键盘行缓冲、串口输出），
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

v0.7 仍**未做**（后续版本补齐）：动态链接、内核高半区迁移（用户堆已于 v0.9 第一步补齐，
`argc/argv` 与 `fork`/`execve`/`wait4` 与进程对象已于 v0.9 第三步补齐）；
磁盘 FS（FAT32）与 `mkfs`（H-03）属 v0.8，Shell 与外部程序属 v0.9。

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

下一步：按 DEVELOPMENT.md 第 5 节顺序继续 —— v1.0（用户/组/权限、启动链收口与发布）；
`mprotect`、完整动态链接（用户态 `ld.so`）、TCP/ICMP 与 socket、更多用户程序按需补。
v1.0 之后的**桌面环境兼容路线**（内核侧地基与接口对齐：D0 地基 → D1 输入 → D2 显示 →
D3 音频 → D4 会话/电源 → D5 收尾）已写入 [docs/desktop.md](docs/desktop.md)（规划）。

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

**v0.1 – v0.9 已全部完成并通过自动化验收（v0.9 六步：用户堆 / 交互式 Shell / 进程对象 / 简化网络栈 / 基础用户程序 / 最小动态链接）。**

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
| v0.9 | ✅ | 用户堆 → `stdin` + 交互式 Shell → 进程对象 + `fork`/`execve`/`wait4` → 简化网络栈 → 基础用户程序（`ls`/`ps`/`cat`/`echo`）→ **内核侧最小动态链接** |
| v1.0 | ⬜ | 权限/凭证 + 完整启动链 + 发布 |

**v0.9 已落地的六步**：

1. **用户堆**：`brk`/`mmap`/`munmap`（W^X、新页全零、`invlpg`）+ `libc` 的 `#[global_allocator]`，
   用户程序可以用 `Vec`/`String`/`Box`；
2. **`stdin` + 交互式 Shell**：键盘行缓冲（行编辑 + 终端式回显）→ `read(0)` 整行交付
   （3 秒无整行 `-EAGAIN`）→ `/bin/sh`（提示符 `root@cerlesse C:>`，内建 `help`/`echo`/`cat`/`pid`/`exit`）；
3. **进程对象 + `fork`/`execve`/`wait4` + `argc`/`argv`**：每进程独立地址空间 +
   每进程 64 KiB 内核栈（栈底哨兵）+ 停在 syscall 里的完整用户帧；Shell 的外部命令
   `/bin/hello` 由 `fork`+`execve`+`wait4` 真正跑起来（找不到的程序 → `ENOENT` → `exit(127)`）；
4. **简化网络栈**：virtio-net → Ethernet → ARP → IPv4 → UDP（DHCP 往返，`make test-network` 13 项）；
5. **基础用户程序**：`/bin/{ls,cat,echo,ps}` 由 Shell 真的 exec 起来（新增 `getdents`/`getprocs`，
   `make test-programs` 17 项 + 5 正则）；
6. **最小动态链接**：`ET_DYN` + `PT_INTERP` + `DT_NEEDED` + `RELATIVE`/`JUMP_SLOT`
   （`make test-dyn` 10 项 + `make dyn-hostcheck` glibc 独立复核）。

详细的实现记录、踩坑与验收证据见 [DEVELOPMENT.md](DEVELOPMENT.md) 第 5、9 节。

**下一步（规划）：纯内核化边界收敛** —— 内核只保留机制（syscall / 进程 / 调度 / 内存 / VFS /
网络栈 / 驱动 / 中断 / IPC / 安全），Shell、init、`libc`、coreutils 与包管理器迁往独立的
**Cerlesse Userland** 项目。逐文件迁移清单与六阶段（冻结基线 → 边界门禁 → ABI 单一来源 →
用户空间迁移 → 构建/测试拆分 → 接口稳定）见 [docs/pure-kernel.md](docs/pure-kernel.md)。
现状：删掉 `libc/`、`user/` 后内核**无法构建**（`cargo build -p kernel` 退出码 101），
因此「不依赖用户态即可编译内核」正是本改造的第一目标。

## 已实现能力

- **引导与基础**：UEFI 引导、BootInfo、GDT/IDT、异常诊断（#PF 带 CR2）、串口控制台。
- **内存**：位图帧分配器、4 级页表与 `AddressSpace`、内核堆 `#[global_allocator]`、
  `allocate_contiguous`（virtqueue/DMA 前置）。
- **中断与时钟**：PIC（8259A）与 IRQ 分发表、PIT 100Hz 心跳、PS/2 键盘行缓冲。
- **进程与调度**：进程对象（PCB：地址空间/用户帧/内核栈/状态机）、Round Robin 200ms 时间片、pipe。
- **系统调用与用户态**：`syscall`/`sysret` 快速路径、独立用户 PML4（每级 `USER`、代码 RO+X、栈 RW+NX）、
  ring3 异常终止而不 panic；`read`/`write`/`open`/`close`/`mmap`/`munmap`/`brk`/`getpid`/`nanosleep`/
  `fork`/`execve`/`exit`/`wait4`（`mprotect` 待实现）；`ET_DYN` + `PT_INTERP` + `DT_NEEDED` +
  重定位（内核侧最小动态链接，见 docs/syscall.md 3.9）。
- **文件系统**：VFS（`FileSystem` trait + 最长前缀挂载路由）、RAMFS、fd 表、
  磁盘 FAT12/16/32 **只读**后端（8.3 + NT 标志 + LFN、任意偏移读、`EROFS`/`EUCLEAN`）。
- **驱动**：驱动注册框架（`Driver` trait + `probe_all` + `ProbeStats`）、PCI 枚举（机制 #1 + BAR 探测）、
  块设备抽象、legacy virtio-blk 0.9.5（virtqueue + 轮询 + bounce buffer）。
- **用户程序**：`libc` 最小运行时（syscall 封装 / `_start` / `print!` / panic handler / 堆 / `argc`-`argv`）；
  `user/hello`（`/init`）、`user/shell`（`/bin/sh`）、`user/hello-bin`（外部程序 `/bin/hello`）、
  `user/{ls,cat,echo,ps}`（→ `/bin/*`）、动态链接夹具 `user/dynhello`（→ `/bin/dynhello` +
  `/lib/{ld.so,libgreet.so}`，由 `as`+`ld` 现编）。
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
| binutils（`as` / `ld`） | v0.9 动态链接夹具由内核构建期现编（`user/dynhello/*.s`），构建内核必需 |
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
make test-all   # 全部 16 个集成测试目标
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
libc/      最小用户态运行时（v0.9 ✅）            syscall/fs/驱动/进程/动态链接）
shared/    boot/kernel 共享结构         user/     用户程序（hello / shell / hello-bin /
                                                     ls / cat / echo / ps / dynhello ✅）
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

> 目录边界规划：本仓库只保留内核 + 引导 + 构建工具，`libc/` 与 `user/*` 将迁往独立的
> **Cerlesse Userland** 仓库（内核只提供机制），见 [docs/pure-kernel.md](docs/pure-kernel.md)。

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
make test-user       # v0.7/v0.9：ELF 从 RAMFS 装载 + 文件 syscall + 用户堆 + 隔离 + Shell（61 项）
make test-shell      # v0.9：注入按键跑交互式 Shell（help / cat /etc/motd / exit，11 项）
make test-proc       # v0.9：fork/execve/wait4 全链路（外部程序 /bin/hello + argv + 退出码回收，8 项 + 4 正则）
make test-programs   # v0.9：从 Shell 跑 /bin/{ls,cat,echo,ps}（getdents 列目录 + getprocs 列进程，17 项 + 5 正则）
make test-dyn        # v0.9：内核侧最小动态链接（PT_INTERP + DT_NEEDED + RELATIVE/JUMP_SLOT，10 项）
make dyn-hostcheck   # v0.9：同一批动态链接夹具交给宿主机 glibc ld.so 独立复核（缺 binutils 时跳过）
make test-driver     # v0.8：断言 PCI 枚举 + 驱动挂接 + virtio-blk 扇区读/写/负例
make fat-image       # v0.8：用 tools/mkfs/mkfs.py 生成 FAT32 测试盘（内容见 tests/fatroot/）
make test-fat        # v0.8：双盘启动，断言 FAT16/32 挂到 VFS 后的读/ls/LFN/跨簇/偏移读/EROFS
make test-network    # v0.9：virtio-net + Ethernet/ARP/IPv4/UDP（ARP 网关解析 + DHCP UDP 往返，13 项）
make user            # v0.7/v0.9：单独构建并校验七个 Rust 用户程序（hello/shell/hello-bin/ls/cat/echo/ps → 段页对齐的 ELF；动态链接夹具由内核构建期现编）
make test-pkg        # H-06 P0a：CKC 包容器（.ckc）与打包/校验规则（主机侧，不启 QEMU）
make test-all        # 全部集成测试
make go-test         # v0.4 H-04/H-05：Go 单元测试（串口分析 + 并行编排器）
make test-orch       # v0.4 H-04：Go 编排器并行跑全部 15 个集成测试场景（tests/orchestrate.json）
```
