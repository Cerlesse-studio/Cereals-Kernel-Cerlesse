# 设备驱动 / PCI / 磁盘（阶段文档）

- 阶段：第八阶段（v0.8 驱动框架 / PCI / 块设备；v0.4 中断控制时钟键盘已完成）
- 最后更新：2026-10-11
- 状态：🟡 v0.8 前半已完成（**驱动注册框架 + PCI 枚举 + 块设备抽象 + virtio-blk（legacy）**，`make test-driver` 22 项 PASS）；**磁盘文件系统（FAT32 挂到 VFS）仍待做**；v0.4（中断控制器 + PIT + PS/2 键盘 + 上下文切换原语 + H-04/H-05 Go 工具）已完成
- 依赖：interrupt（IRQ 分发）、memory（连续物理帧 → DMA / virtqueue）、arch::port（端口 I/O）

> 本文档顶部保留阶段/最后更新/状态字段，与项目文档维护约定对齐。

## 1. 当前状态（2026-10-11）

### 1.1 v0.8 已落地：驱动框架 + PCI 枚举 + 块设备

代码位置与职责：

| 文件 | 职责 |
| ---- | ---- |
| `kernel/src/driver/pci.rs` | PCI 枚举（配置机制 #1：`0xCF8`/`0xCFC`）、BAR 探测与窗口大小计算、设备表、`print_bdf` |
| `kernel/src/driver/driver.rs` | 驱动注册表 + `Driver` trait（`name`/`matches`/`attach`）+ `probe_all()` 匹配与挂接 + `ProbeStats` |
| `kernel/src/driver/block.rs` | 块设备抽象 `BlockDevice`（按 512 字节扇区读写）+ 设备注册表 + 块设备验收自测 |
| `kernel/src/driver/virtio_blk.rs` | legacy virtio-blk 0.9.5 驱动：virtqueue 布局、描述符链、notify、used ring 轮询、bounce buffer |
| `kernel/src/arch/x86_64/port.rs` | 端口 I/O 统一入口（`inb/inw/inl` / `outb/outw/outl`） |
| `kernel/src/memory/frame.rs` | `allocate_contiguous(count)` / `free_contiguous(...)`：virtqueue 需要的**物理连续**帧 |

启动顺序（`kernel/src/main.rs`，在 `Kernel started!` 之后、调度器/用户态之前）：

```text
pci::init()            → 枚举 256×32 个 function，填设备表并逐设备打印一行
find_by_id(1af4,1001)   → 断言启动盘所在的 legacy virtio-blk 确实在总线上
driver::register_builtin() → 注册表里放入 virtio-blk-pci
driver::probe_all()    → 逐设备匹配（注册顺序优先）→ attach → 统计 matched/attached/failed
block::selftest(disk)  → 引导扇区 / 分块读 / 写回读 / 负例
```

### 1.2 关键设计决策（为什么是这样）

**PCI 用配置机制 #1（`0xCF8`/`0xCFC`）而不是 ECAM。** 机制 #1 每次访问两条端口指令，不依赖 ACPI；
ECAM 需要先解析 MCFG（ACPI）表，那是引入 ACPI 之后才划算的优化。代价（约 8k 次端口读扫描全总线）
在启动日志里可忽略，换来的是「枚举」这条路径自成一体、可被单测形态的串口断言完全覆盖。

**只实现 legacy virtio（0.9.5），不做 modern（virtio 1.0）。** 设备 ID 决定接口：`0x1001`（legacy，
BAR0 是 I/O 窗口，寄存器是简单偏移）/ `0x1042`（modern，寄存器在 capability 指向的 MMIO 里，
队列布局也不同）。legacy 的实现量大约只有现代接口的三分之一，而 QEMU 用
`disable-modern=on` 就能稳定提供 legacy 设备；真遇到 modern 设备时 `attach` 会**打印明确原因并返回错误**
（不静默失败），错误会计入 `ProbeStats::failed`。

**块 I/O 走同步轮询，不用中断。** v0.8 不配置 MSI-X/INTx，每次请求提交后自旋等 used ring 的 `idx` 前进
（上限由 PIT tick 给出）。理由是：块 I/O 常在无法睡眠的路径上发生，而中断上下文里做块 I/O 是另一类
复杂度的开始。代价是等待期间忙等；v0.9 起再改为「中断 + 等待队列」。

**bounce buffer 换掉一整类 DMA 对齐问题。** 调用方缓冲长度只要整扇区对齐即可，驱动内部用一块
页对齐静态缓冲（8 扇区 = 4 KiB）分块搬运。因此不要求调用方缓冲物理连续、也不要求对齐；
一次性拷贝的成本换来「上层随便传缓冲」的可用性。

**驱动框架只做匹配与挂接，不做生命周期管理。** 注册表容量固定（`MAX_DRIVERS`），启动阶段写入、
此后只读；一个设备最多被一个驱动接管；`attach` 失败不阻断其它设备。上层（`main`）只需要看
`ProbeStats`，不需要知道总线上有多少种设备。

### 1.3 验收证据（`make test-driver`，22 项断言；以下是真实串口输出）

```text
pci: 00:03.0 vendor=0x1af4 device_id=0x1001 class=0x1 subclass=0x0 prog_if=0x0   ← legacy virtio-blk
pci: 00:03.0 bar0=io:0x6000 size=0x80                                            ← BAR0 是 I/O 窗口（legacy 的标志）
pci: enumerated devices=5
pci: virtio-blk (legacy) at 00:03.0 class=0x1 subclass=0x0
driver: registered index=0 name=virtio-blk-pci
driver: match 00:03.0 -> virtio-blk-pci
virtio-blk: legacy BAR0=0x6000 size=0x80
virtio-blk: host features=0x71006ed4
virtio-blk: guest features=0x0
virtio-blk: queue0 size=256 region=0x100000 (3 pages contiguous)
virtio-blk: capacity=131072 sectors (64 MiB)
virtio-blk: status=0xf (DRIVER_OK)
virtio-blk: registered as block device 0
driver: attached 00:03.0 -> virtio-blk-pci
driver: probe PASS (devices=5 matched=1 attached=1 failed=0)
block: registered devices=1
block: device=virtio-blk0 sector_size=512 sectors=131072 bytes=67108864
block: boot sector sig=0x55aa fstype=FAT16 oem=MTOO4032 sectors_per_cluster=2
block: chunked read PASS (512/4096/4608 bytes consistent at lba 0)
block: write/readback PASS (lba=131071 pattern=0xa5)
block: negative cases PASS (unaligned len, lba beyond capacity, overflow)
block: selftest PASS
```

这些断言的意义（「硬件事实」而不是日志措辞）：

- `device_id=0x1001` + `bar0=io:`：QEMU 在 `disable-modern=on` 下暴露的**就是** legacy 接口，
  与驱动实现的前提一致（modern 是 `0x1042`）；
- `queue0 size=256 region=0x100000 (3 pages contiguous)`：队列深度来自设备，队列区来自
  `frame::allocate_contiguous`（描述符表 + avail + 4 KiB 对齐的 used 三段必须物理连续，
  因为 legacy 的队列地址寄存器只装一个 PFN）；
- `capacity=131072 sectors (64 MiB)`：容量由设备报出，与 `tools/image-builder/build_image.py`
  的镜像尺寸互相印证；
- `boot sector sig=0x55aa fstype=FAT16`：从磁盘真的读回了本次构建的 FAT 引导扇区
  （类型串取自 BPB 的根目录项数决定的位置，不是写死的字符串）；
- `chunked read PASS`：4608 字节的读**跨过**驱动内部 8 扇区（4 KiB）搬运边界，与 4096 / 512
  两次独立读的拼接结果逐字节一致 → 分块重组与 LBA 偏移换算正确；
- `write/readback PASS (lba=131071 ...)`：最后一个扇区写 → 读回 → **还原** → 再读回；
  用非 snapshot 方式启动后比对镜像 md5，确认「还原」真的把盘恢复原样（见 1.5）；
- `negative cases PASS`：长度非整扇区、LBA 越过容量、LBA 溢出（`u64::MAX-1`）三种输入都必须
  报错 —— 最后一条曾经真的漏过（`lba + sectors` 环绕后看起来在容量内），见 B-16/B-17。

### 1.4 已知限制（明确未做，不要读成已完成）

- **磁盘文件系统（FAT32）尚未挂到 VFS**：v0.8 只做到「块设备可用」，`block.rs` 之上还没有 FAT 后端；
  因此 `docs/filesystem.md` 里的磁盘后端、以及 H-03（Python `mkfs`）仍未落地。
- **不分配/重定位 BAR**：直接使用固件分配的地址。真实平台上若出现未分配窗口（全 0）会打日志而不是猜测
  （v0.8 的实现里 `read_bar` 对全 0 返回「未实现」）。
- **不解析 capability 链、不配置中断**：MSI-X/INTx、设备特定能力结构都留给 v0.9 与网络驱动一起做。
- **只支持一块 virtio-blk 盘**（`block` 注册表容量 2，驱动状态是单个实例）。
- **不实现 modern（virtio 1.0）接口**：见 1.2；遇到 modern 设备时明确报错。
- **queue 只支持 0 号队列**：块设备只有一个 virtqueue，够用；多队列留给 v0.9+。
- 端口 I/O 的新统一入口（`arch/x86_64/port.rs`）只被 PCI / virtio 使用；早期驱动（serial/keyboard/PIC）
  仍各自保留私有副本 —— 改造它们只会扩大 diff，与 v0.8 目标无关。

### 1.5 磁盘写入的「无损」验证

`block: selftest` 会写最后一个扇区再做还原。为确认还原有效（而不只是「读回自身缓存」），
用**不带 `snapshot=on`** 的方式启动一次并比对镜像：

```text
before=26a5e771b617734a3ac211caff0fa189
after =26a5e771b617734a3ac211caff0fa189
IMAGE UNCHANGED (write test restored the scratch sector)
```

测试套件里则统一用 `snapshot=on`（临时写时复制），既让并行场景共享同一镜像，也不污染原始文件。

### 1.6 v0.4 已落地部分（中断控制器 + PIT 100Hz + PS/2 键盘 + 上下文切换原语）

- [x] 中断控制器骨架与 PIC 第一版（初始化、IRQ→向量映射、mask/EOI 原语）
- [x] `kernel/src/interrupt/` 绑定为 `controller` / `exception` / `irq` 三子模块
- [x] `kernel_main` 中接入 PIC 初始化（默认全屏蔽，不使能外设 IRQ）
- [x] 选型策略记录：`kernel/src/interrupt/pic_policy.md`
- [x] PIT/Timer 子系统（`kernel/src/time/`：通道 0 重装载值、tick 计数）
- [x] IRQ 分发入口与 16 槽 IRQ 注册表（`kernel/src/interrupt/irq.rs`）
- [x] IRQ0（PIT）回调：100Hz，每 100 tick 打印一次 `IRQ0_heartbeat tick=<n>`
- [x] PS/2 键盘中断输入（`kernel/src/driver/keyboard.rs`）：8042 初始化、IRQ1、set 1 解码、回显 `KB <字符>`
- [x] 上下文切换原语（`kernel/src/arch/x86_64/context.rs`）：`switch_to` / `init_context` + 内核自测
- [x] `make test-keyboard`：QEMU monitor `sendkey` 注入真实按键并断言回显

### 1.7 中断/时钟侧的已知状态与限制

- PIC 映射固定为 master/slave + IRQ0..15 → 向量 32..47；APIC（LAPIC/IOAPIC/redirection）仍为占位。
- IRQ 汇编入口（`irq_common`）保存/恢复 15 个通用寄存器，**不保存** XMM/FPU 状态（内核不用浮点）。
- 键盘暂无环形缓冲，主循环无法「读取」按键（v0.9 控制台时再补）；驱动在中断上下文直接打印。

## 2. 开发路线

### 2.1 v0.8（Driver Manager → PCI → 块设备 → 磁盘 FS 挂载）

1. [x] 驱动框架：`Driver` trait + 注册表 + 匹配/挂接统计（`driver/driver.rs`）。
2. [x] PCI 枚举：遍历总线、解析类别/BAR、按 Vendor/Device ID 匹配。
3. [x] 块设备抽象（`block.rs`）与 virtio-blk（legacy）驱动（`virtio_blk.rs`）。
4. [ ] **磁盘文件系统后端：FAT32 挂载进 VFS**（见 `docs/filesystem.md`）—— v0.8 剩余部分。
5. [ ] 验收补齐：`ls`/`cat` 能读到磁盘上的文件（依赖 4）。

验收（v0.8）：

- [x] PCI 列表输出，包含 Vendor/Device ID（`make test-driver`）
- [x] 磁盘读写可验证（引导扇区读 + 最后扇区写回读 + 分块读一致性 + 负例）
- [ ] FAT32 挂载完成，可在 VFS 层访问磁盘文件

### 2.2 v0.9（网络栈顺序 + Socket syscall 最小集）

1. Network Driver（例如 virtio-net，可复用 v0.8 的 `Driver` 框架与 PCI 枚举）
2. Ethernet → ARP → IPv4 → ICMP → UDP → TCP（尽力） → Socket API

验收（v0.9）：

- [ ] Shell 交互可用
- [ ] ping/HTTP 简例可跑

## 3. 驱动列表

### 3.1 已实现

| 驱动 | 版本 | 说明 |
| ---- | ---- | ---- |
| Serial（串口） | v0.1 | 开发期唯一日志通道（`print`/`println`/`print_dec`/`print_hex`/`print_hex_pad`/`put_bytes`） |
| Keyboard（PS/2） | v0.4 | IRQ1 扫描码解码与串口回显 |
| Timer（PIT） | v0.4 | 100Hz tick：调度时间片与块 I/O 超时基准 |
| PCI | v0.8 | 枚举与 BAR 探测（配置机制 #1） |
| Disk（virtio-blk legacy） | v0.8 | 块设备：`BlockDevice` trait 的实现者 |

### 3.2 后续（按需补）

- virtio-net（v0.9，网络栈的底座）、AHCI/NVMe、USB、Audio、GPU、Framebuffer（视需要）。

## 4. 非目标 / 后续

- SMP 多核下的中断均衡与高级 APIC 语义：v1.0 之后。
- USB、Wi-Fi、GPU、完整 TCP/IP 工业栈：非第一版目标。
- DMA 缓冲管理细节：v0.8 只做了「连续物理帧」这一最小的 DMA 前置条件；IOMMU/散列（scatter-gather）
  留给真实硬件需求出现时。

## 5. 修订记录

| 日期 | 变更 |
| ---- | ---- |
| 2026-10-06 | 第一阶段创建占位 |
| 2026-10-07 | 补记当前状态：PIC 第一版已接入 kernel_main；v0.4 剩余（Timer/键盘/切换原语）与 v0.8/v0.9 待编写 |
| 2026-10-08 | v0.4 完成：IRQ stub 修正（B-08）、IRQ 入口保存寄存器（B-09）、PIT 100Hz、PS/2 键盘回显、上下文切换原语；新增 `make test-keyboard` |
| 2026-10-08 | v0.4 最后一项 H-04/H-05 落地：Go 编排器 + 串口分析器；`make go-test` / `make test-orch` 通过 |
| 2026-10-08 | 统一文档顶部元信息与修订记录（阶段 / 最后更新 / 状态） |
| 2026-10-11 | **v0.8 前半落地**：新增 `driver/pci.rs`（机制 #1 + BAR 探测 + 窗口大小）、`driver/driver.rs`（注册框架 + `probe_all`）、`driver/block.rs`（按扇区读写 + 验收自测）、`driver/virtio_blk.rs`（legacy virtio 0.9.5，同步轮询 + bounce buffer）、`arch/x86_64/port.rs`（端口 I/O）、`frame::allocate_contiguous`；启动盘改为 virtio-blk（Makefile / `tests/test_boot.py` / `tests/test_keyboard.py`，`disable-modern=on`）；新增 `make test-driver`（22 项）与编排场景 `driver`（9 场景）；修 B-16（BAR 大小 32→64 位宽错误）、B-17（LBA 溢出未拦）；状态行由「v0.8 尚未编写」改为「v0.8 前半完成，FAT32 待做」 |
