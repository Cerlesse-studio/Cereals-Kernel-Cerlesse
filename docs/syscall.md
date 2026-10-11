# 系统调用 / 用户空间 / ELF（阶段文档）

- 阶段：第六阶段（v0.6 – v0.7）+ 第九阶段开局（v0.9 运行时前置条件）
- 最后更新：2026-10-11
- 状态：🟡 v0.7 已落地：**ELF Loader**（从 RAMFS 上的 `/init` 装载，按段权限逐页映射）+ 文件 syscall
  （`read`/`write`/`open`/`close`，对接 VFS/RAMFS）+ 用户 `RW` 段对 ring3 可写；
  **v0.9 第一步已落地：用户堆**（`brk` 256 KiB / 匿名 `mmap` 256 KiB / `munmap`，
  用户态 `#[global_allocator]` 可用 `Vec`；见 3.5）；
  `make test-user` 59 项 / `make test-fs` 19 项串口断言 PASS；
  `fork/exec/wait`、进程对象、`argc/argv`、动态链接仍未做（见 3.4）

> 本文档顶部保留阶段 / 最后更新 / 状态字段，与项目文档维护约定对齐。
- 依赖：process.md（进程模型）、memory.md（用户地址空间）

## 1. 概述

v0.6 与 v0.7 一起构成系统调用 / 用户空间阶段，但两者有清晰先后：

1. **v0.6 核心**：建立用户态 / 内核态隔离 + 系统调用入口 + 第一批 syscall + 用户态 `hello` 运行。
2. **v0.7 扩展（本版已完成）**：ELF Loader（从文件装载、按段权限映射）、
   VFS 对接的文件语义（`open`/`read`/`write`/`close`）。
3. **v0.9 第一步（本版已做）**：用户堆 —— `brk`/`mmap`/`munmap` + libc 的全局分配器（见 3.5）。
4. **仍未做**：`argc/argv`、`fork`/`execve`/`wait4` 与进程对象、`mprotect`（W^X 转换）、
   动态链接（`PT_INTERP`/`ld.so`）、Shell 与 `ls`/`cat` 程序。

## 2. v0.6 实际实现（2026-10-10 落地）

### 2.1 入口选型（已固化，不再是草案）

**决定了 `syscall` / `sysret` 快速路径**（`int 0x80` 方案不再保留）。

| 项 | 取值 | 说明 |
| ---- | ---- | ---- |
| 入口指令 | `syscall` → `IA32_LSTAR` = `syscall_entry` | 保存 RIP→`rcx`、RFLAGS→`r11`，**不切栈** |
| 返回指令 | `sysretq` | CS=`STAR[63:48]+16`、SS=`STAR[63:48]+8` |
| syscall 号 | `rax` | 越界（≥ `SYS_MAX`=256）返回 `-ENOSYS` |
| 参数 | `rdi, rsi, rdx, r10, r8` | 与 Linux x86_64 一致，便于以后直接跑现成用户态程序 |
| 返回值 | `rax` | 负数 = `-errno`（如 `-ENOSYS` = -38、`-EFAULT` = -14） |
| 进入内核时的 RFLAGS 掩码 | `IA32_FMASK` = IF\|DF\|TF = `0x700` | 内核在关中断下运行 syscall handler |

`syscall` 不切换栈，因此入口汇编的第一件事是把用户 RSP 存到 `CER_USER_RSP` 并
切到**专用系统调用栈**（`trampoline.rs` 的 16 KiB 静态栈）。

### 2.2 GDT 布局（由 `IA32_STAR` 决定，顺序不可随意调整）

```text
0x00 null | 0x08 内核代码 | 0x10 内核数据 | 0x18 用户数据 | 0x20 用户代码 | 0x28 TSS
```

- `syscall`：CS = STAR[47:32] = `0x08`，SS = CS+8 = `0x10`；
- `sysret`：STAR[63:48] = `0x10` → SS = base+8 = `0x18`，CS = base+16 = `0x20`。

「用户数据段紧邻用户代码段之前」是硬约束；改动顺序会让 `sysret` 直接 #GP。
`trampoline.rs` 里有两条编译期断言把汇编里的立即数 `0x1b`/`0x23` 与 GDT 常量绑定。

### 2.3 CPU 侧使能项

- `EFER.SCE`（使能 `syscall`/`sysret`）；`EFER.NXE`（页表 NX 位，v0.3 起堆页已用 bit63）；
- `CR4.SMEP/SMAP` **清除**：ring0 需要直接读写用户页（`sys_read`/`sys_write` 拷用户缓冲）。
  v0.7 **未**改为 `stac`/`clac` 精细控制（内核仍直接访问用户页），该项列入 3.4 未做项；
- `IA32_STAR` / `IA32_LSTAR` / `IA32_FMASK` 见上表。

### 2.4 用户地址空间（`syscall/address_space.rs`）

> v0.6 形态。“代码页（1..=8 页、只读可执行）”在 v0.7 已被“映像窗口（16 页）按 ELF 段权限逐页映射”
> 取代，见 3.1；本节的页表构造（PML4 下标 1、惰性清 `USER` 位、每级置 `USER`）至今未变。

- 用户映射放在 **PML4 下标 1**（`0x0000_0080_0000_0000` = 512 GiB，规范低半区）。
  内核恒等映射只用下标 0，因此用户子树完全归本模块所有，`Drop` 时可整棵释放；
- 新 PML4 先**逐项拷贝**内核 PML4（内核在新地址空间里照样可见，切 CR3 不崩）；
- 再**惰性清理**拷贝过来的内核子树中的 `USER` 位：只在 `USER` 确实为 1 时才写回，
  并把清位数打到串口；
- 用户子树 PDPT/PD/PT **每一级**都置 `USER`：ring3 要求每级都置位才能到达叶子页；
- 用户代码页 = `PRESENT|USER`（只读可执行，不可写），**按映像大小逐页映射**（1..=8 页，每页 4 KiB）；
  用户栈 4 页 = `PRESENT|WRITABLE|USER|NO_EXEC`（固定排在代码区上限之后，地址不随程序大小漂移）；
- 帧分配任何一步失败都会回滚已分配的帧，不留泄漏（v0.6 是 `new()` 与 `load_program()` 各自回滚；
  v0.7 的入口是 `map_image_page()` —— ELF Loader 调用它逐页映射，失败时由 `Drop` 统一回收）。

> 固件页表只读（实测发现）：OVMF/EDK2 通过 EnablePageTableProtection 把自己建立的
> 页表页标记为**只读**。因此“把整棵内核子树无条件重写一遍”会在启动路径上直接 #PF
> （实测：写 `0x1fc03000` 处的 PDPT 项，`error=P|W`）。这正是上面“惰性清理”的原因：
> `USER` 本来就为 0 的表项“改成 0”没有任何意义，而这个写入会立刻崩溃。

### 2.5 用户态执行期的内核状态收敛

进入 ring3 前必须做两件事（否则会以难以定位的方式炸掉）：

1. **挂起调度器**（`scheduler::suspend`）。用户程序不在调度器任务表里，若 PIT 中断在
   用户态上下文里切到内核任务，那个任务会在「用户页表 + 用户栈」的中间状态上恢复执行。
   挂起后 PIT 仍递增 tick 与打印心跳，只是不再切换；
2. **`TSS.RSP0` 指向专用陷入栈**（`trampoline::trap_stack_top()`）。进入用户态时内核
   调用栈仍然存活（`enter_user` 的调用者还压着返回地址），若沿用同一条栈，用户态期间的
   中断会把帧压到活动的内核栈上，覆盖内核自身的栈帧。返回内核后恢复原 `RSP0`。

两条栈分开：用户态中的中断/异常走陷入栈，`syscall` 走系统调用栈。

### 2.6 异常处理（ring3 不再 panic）

`exception_dispatch` 用 **CS 的 RPL 位**区分异常来源：

- `cs & 3 == 3` → 来自 ring3：打印 `user: FAULT vector/error/rip/cr2` 与
  `user: isolation PASS` 提示，然后 `trampoline::leave()` **终止用户态执行**并回到内核调用者，
  内核继续存活；
- 否则维持原行为（打印完整诊断 + `KERNEL PANIC`）。

### 2.7 分发层与首批 syscall（已实现）

| syscall | 号 | 状态 | 行为 |
| ---- | ---- | ---- | ---- |
| `read` | 0 | ✅ v0.7 | fd 0 → 0（stdin 未接通，不卡住调用方）；fd ≥ 3 → 从 VFS 读入用户缓冲（要求**可写**，否则 `-EFAULT`）；偏移量随之推进，EOF 返回 0 |
| `write` | 1 | ✅ v0.6/v0.7 | `fd=1/2` 写串口；`fd ≥ 3` 写文件（VFS，偏移量随之推进）；缓冲要求可读 |
| `open` | 2 | ✅ v0.7 | `open(path_ptr, path_len, flags)`：路径用**指针 + 长度**（内核已校验区间，不在不可信内存上找 NUL）；`flags` 只接受 `O_RDONLY`/`O_WRONLY`/`O_RDWR`/`O_CREAT`，返回**最小空闲 fd**（≥3） |
| `close` | 3 | ✅ v0.7 | 释放动态 fd（0/1/2 是保留位，关闭它们返回 `-EBADF`） |
| `sleep`（nanosleep） | 35 | ✅ v0.6 | 以 PIT tick（10ms）为单位等待；期间 `sti; hlt` 让 PIT 继续推进 |
| `getpid` | 39 | ✅ v0.6 | 返回当前用户程序 PID（串口打印 `user: getpid -> N`） |
| `exit` | 60 | ✅ v0.6 | 记录退出码并结束用户态执行（不返回） |
| `mmap` | 9 | ✅ v0.9 第一步 | 匿名映射：只收 `MAP_PRIVATE\|MAP_ANONYMOUS`、`fd` 必须 -1、**只传 5 个参数（无 offset）**；first-fit 落在 256 KiB 的 mmap 窗口；新页**保证全零**；`PROT_WRITE\|PROT_EXEC` 拒绝（W^X）、`PROT_NONE` 拒绝（没有 `mprotect`） |
| `munmap` | 11 | ✅ v0.9 第一步 | 整段归还：页对齐 + 全段已映射，否则 `-EINVAL`；执行时清页表项 **并 `invlpg`**（否则 TLB 会留下幽灵映射） |
| `brk` | 12 | ✅ v0.9 第一步 | 增长/收缩堆顶（窗口 256 KiB）；`addr = 0` 只查询；**Linux 语义：不返回错误**，失败返回原值（`ret < addr` 即失败）；增长全成或全不成 |
| `fork` / `execve` / `wait4` | 57 / 59 / 61 | ⬜ 后续 | 落在 ENOSYS 桩上（需要进程对象） |
| `mprotect` | 10 | ⬜ 后续 | 已列入「号已定义但未实现」（`syscall_table::UNIMPLEMENTED`）：`PROT_NONE` 的映射与「先写后换只执行」都依赖它 |

分发表按 `SYS_MAX`(=256) 排满：未实现的号落在公共 ENOSYS 桩，
「号合法但未实现」与「号越界」是两种可区分的失败路径。

文件 syscall 的两条硬规则（v0.7 新增）：

1. **用户指针先校验再用**：`write`/`open` 要求缓冲可读、`read` 要求缓冲可写，
   按**页**检查（半页在映射里、半页越界也会被拒），失败返回 `-EFAULT` 并留痕
   `user: EFAULT write ptr=0xdead00000000 len=1 (buffer not readable in user space)`；
2. **单次 I/O 上限** 4096 字节（`IO_MAX`）与路径上限 256 字节（`PATH_MAX`），
   避免用户态用巨大 count 让内核长时间不返回。

文件相关错误的来源分别是：`-EBADF`（fd 未打开/模式不符）、`-ENOENT`（路径不存在）、
`-EISDIR`/`-ENOTDIR`（类型不符）、`-EEXIST`（`create` 已存在）、`-ENOSPC`（fd 用尽 /
文件超过 RAMFS 上限）、`-EINVAL`（flags 未知位/非法路径）—— 与 `fs` 层的 errno 一致。

### 2.8 用户程序来源与运行驱动（`syscall/user.rs`，v0.6 形态）

> v0.7 已把这里描述的两条路径升级为「构建期内嵌 **ELF** → RAMFS `/init` → ELF Loader」，
> 见 3.1。本节保留 v0.6 的形态，因为它是 `source=` 字段与自测结构的由来。

v0.6 **没有** ELF Loader（那是 v0.7），用户程序的获取方式是「**构建期摊平 + 内嵌**」：

- `hello`：来自 `user/hello`（Rust + `libc`）。`kernel/build.rs` 在构建内核时用嵌套 cargo
  构建它（large 代码模型 + `user-release` profile），解析 ELF 并摊平成「从 `USER_VIRT_BASE`
  开始的连续映像」，写入 `$OUT_DIR/user_hello.bin` 后由内核 `include_bytes!` 内嵌。
  因此**跑的不是内核里的临时机器码，而是用户态脚手架的产物**（见 2.11）；
- `probe`：位置无关的手写机器码（`global_asm!`，只用立即数与寄存器间接寻址，
  因此整段字节可直接拷到用户代码页执行），读内核映像地址 `0x200_0000` 验证隔离。
  v0.7 仍然保留这条路径（`UserSpace::map_raw`）：隔离自测需要一段**必然越权**的代码，
  把它写成 ELF 只会让探针依赖被测的 ELF Loader。

运行驱动 `run(name, source, program, pid)` 的顺序（每一步都有理由，顺序错了就会崩）：

```text
UserSpace::new() → load_program()
→ TSS.RSP0 = 陷阱栈顶 → CER_USER_SPACE = &space → write_cr3(用户页表根)
→ iretq 到 ring3（enter_user）
→ ... 用户程序跑 syscall / 触发 ring3 异常 ...
→ leave_user 回到内核 → write_cr3(内核根) → 恢复 TSS.RSP0 → CER_USER_SPACE = 0
→ drop(space) 回收全部用户帧与页表帧
```

### 2.9 验收（v0.6）

已通过 `make test-user`（**v0.6 时为 19 项串口断言**，QEMU + OVMF；v0.7 增至 32 项，
v0.9 第一步增至 59 项，见 3.3 / 3.5）：

- [x] 脚手架生效：`user: program=hello source=libc bytes=6584 code_pages=2`
      （比串口断言更硬的是可重复性：`make user` 不启 QEMU 就能单独构建 `libc` + `user/hello`，
      产物 ELF 无重定位、入口 `0x8000000000`）
- [x] ring3 进入可观测：`user: enter name=hello pid=1 entry=0x8000000000`
- [x] syscall 入口/返回可观测：`user: write fd=1 len=22`、`user: getpid -> 1`
- [x] `exit` 回收退出码：`user: exit code=42` → `user: hello PASS`
- [x] `sleep` 可用且计时真实推进：`user: sleep ticks=2` / `user: sleep done ticks=2`
- [x] 用户地址空间隔离：probe 读内核页被拦下，
      `user: FAULT vector=0xe error=0x5` + `user: FAULT cr2=0x2000000` +
      `user: isolation PASS`，且**内核不 panic、继续运行**
- [x] 用户态 `/init` 通过 **ELF Loader** 加载运行 → v0.7 已完成（见 3.1 / 3.3）
- [ ] `fork`/`exec`/`wait` 骨架 → 推到 v0.8（v0.6/v0.7 未提供进程对象，`exit` 语义为
      「结束当前用户态执行」而非「销毁进程」）

### 2.10 v0.6 明确未做的事（避免误读）

- **没有** ELF Loader（用户程序是构建期摊平的内嵌映像，不是从文件加载）→ **v0.7 已做**（3.1）；
  没有用户栈参数传递（`argc/argv`）→ 仍未做；
- **没有**用户堆/`brk`/`mmap`。v0.6 里用户程序连静态数据都不能写（代码页整体只读）；
  v0.7 起 `RW` 段（`.data`/`.bss`）映射为 ring3 可写（实测 `hello: static counter=1`），
  但仍然**没有堆**；
- **没有**进程/线程对象：一次只跑一个用户程序，`exit` 结束该次执行并回到内核；
- **没有** `fork/exec/wait`；v0.6 没有 `read/open/close` → **v0.7 已做**（见 2.7 / 3.2）；
- **没有**高半区迁移：内核仍在低地址恒等映射内，隔离靠**页表项 `USER` 位**而不是地址空间划分
  （ADR-009 的“高半区”一项推迟，实现方式见 2.4）；
- 用户页表是内核 PML4 的**快照**：内核在用户态运行期间新增 PML4 槽位级的映射不会出现在
  用户页表中（用户态路径只做串口输出与 tick 等待，不触发这类增长）。

### 2.11 用户态脚手架（`libc/` + `user/*`，2026-10-11 落地）

v0.6 的目标之一是「新用户程序不必再手写机器码」。脚手架由三部分组成：

| 组成 | 位置 | 职责 |
| ---- | ---- | ---- |
| 运行时 | `libc/` | syscall 封装（`write`/`getpid`/`nanosleep`/`exit`）、`_start` → 用户 `main` → `exit`、`print!/println!`、`#[panic_handler]` |
| 用户程序 | `user/hello/` | 第一个真实程序：`Cargo.toml` + `build.rs`（传链接脚本）+ `user.ld` + `src/main.rs` |
| 构建链 | `kernel/build.rs` | 构建用户程序 → 校验 ELF → `include_bytes!` 内嵌（v0.7：**不再摊平**，见 3.1） |

构建与装载流水线（v0.6 形态；v0.7 的替换见 3.1）：

```text
user/hello (Rust + libc)
  └─ cargo build -p user-hello --target x86_64-unknown-none --profile user-release
       RUSTFLAGS="-C relocation-model=static -C code-model=large"
  └─ user-hello ELF（链接在 0x8000000000，无重定位）
       └─ kernel/build.rs::flat_image() ⟶ $OUT_DIR/user_hello.bin（第 0 字节即入口）
            └─ include_bytes! ⟶ UserSpace::load_program()（逐页拷贝 + 逐页映射 PRESENT|USER）
                 └─ iretq ⟶ ring3
```

两个「错了必然跑不起来」的约束（`kernel/build.rs` 在构建期直接断言，不留给运行期；
v0.7 又加了「段页对齐」与「映像窗口」，见 3.1）：

1. 映像基址 = `USER_VIRT_BASE`（0x0000_0080_0000_0000）—— 内核不做加载时重定位；
2. 不存在可装载重定位（`SHT_RELA`/`SHT_REL` + `SHF_ALLOC`）—— 同上的直接后果。

链接参数也不是可选的：链接地址 2^39 超出 small 代码模型 ±2GiB 的绝对寻址范围，
必须 `code-model=large`；而 large 模型与 LTO 不兼容（实测 rust-lld 报
`failed to load bitcode of module core-*.o`），因此用户程序用 workspace 的
`user-release` profile（inherits `dev`、`lto = false`、`opt-level = 2`、`strip = true`）。
另外，外层 cargo 会把 `CARGO_ENCODED_RUSTFLAGS` 注入 build script 的环境且其优先级
高于 `RUSTFLAGS`，所以 `kernel/build.rs` 调用嵌套 cargo 时必须显式移除它，
否则会用内核的 rustflags 去编用户程序（直接报 `R_X86_64_32S out of range`）。

用法（新程序的完整约定见 [libc/README.md](../libc/README.md)）：

```rust
#![no_std]
#![no_main]

use libc::{getpid, nanosleep, println};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    println!("hello from user space"); // 一次宏调用 = 一次 write
    println!("pid={}", getpid());      // core::fmt 可用，不需要堆
    nanosleep(2);                      // 2 个 PIT tick = 20ms
    42                                 // 退出码
}
```

验证方式：`make user`（不启 QEMU，验编译链接 **+ 段布局**）与 `make test-user`（ring3 实测，
断言 `user: program=/init source=elf`）。

## 3. v0.7 实际实现（2026-10-11 落地）

### 3.1 ELF Loader（`kernel/src/syscall/elf.rs` + `address_space.rs`）

v0.6 的「构建期摊平 + 原样拷贝」被替换成真实的加载链：

```text
user/hello (Rust + libc)
  └─ cargo build（large 代码模型 + user-release profile）
  └─ user-hello ELF（链接在 0x8000000000；段页对齐、无重定位）
       └─ kernel/build.rs：校验（基址/页对齐/W^X/映像窗口/无重定位）→ $OUT_DIR/user_hello.elf
            └─ include_bytes! → fs::init：写进 RAMFS 的 /init
                 └─ vfs::read_all("/init")（挂载表路由 + 文件读）
                      └─ elf::load() → UserSpace::map_image_page() 按段权限逐页映射
                           └─ iretq → ring3
```

装载做的事（全部在**内核 CR3 下**、切 CR3 之前完成）：

1. **解析**：只接受 `ELF64 / little-endian / ET_EXEC / EM_X86_64`；每个 `PT_LOAD` 必须在
   用户映像窗口 `[0x0000_0080_0000_0000, +64 KiB)` 内，入口必须落在某个段里；
2. **映射**：按**页**并集各段权限 —— `p_flags` 的 `W` → `WRITABLE`、非 `X` → `NO_EXEC`；
   页帧在首次映射时分配并清零，同页被多个段覆盖时只并集权限（不重复清零）；
3. **拷贝**：每段 `p_filesz` 字节写进对应页，`p_memsz - p_filesz` 保持 0（这就是 `.bss`）；
4. **入口**：直接用 ELF 头的 `e_entry`（不再要求「映像第 0 字节即入口」）。

用户虚拟地址布局（固定，不随程序大小漂移）：

```text
0x0000_0080_0000_0000 + 0x0000   页 0..15   用户映像窗口（64 KiB，ELF PT_LOAD 按段权限映射）
0x0000_0080_0000_0000 + 0x10000  页 16..19  用户栈（16 KiB，RW、NX）
0x0000_0080_0000_0000 + 0x20000  页 32..95  brk 堆窗口（256 KiB，按需映射，RW、NX；v0.9）
0x0000_0080_0000_0000 + 0x60000  页 96..159 匿名 mmap 窗口（256 KiB，first-fit，RW 或 RX；v0.9）
```

四个窗口都在同一张 PT 覆盖的 2 MiB 内（共 160 页），因此仍然只需要一张页表；
窗户之间（如页 20..31）没映射 —— ring3 摸到就是 #PF，这是隔离的正常表现。

两个新的构建期/装载期约束（都有实测理由）：

| 约束 | 不满足时 |
| ---- | ---- |
| 每个段**页对齐**（`user.ld` 里 `ALIGN(0x1000)`） | 同页既 `W` 又 `X` → `W^X` 退化，装载器拒绝：`user: ELF rejected: W and X segments share a page` |
| 零初始化静态量用 `.lbss.*` 段名（large 代码模型） | `user.ld` 漏列 `.lbss.*` 时它们成为**孤儿段**被塞在 `.data` 之后，RW 段不再页对齐（实测落在 `0x8000003040`） |

链接脚本还要注意：`.text`/`.rodata`/`.data`/`.bss` 四个段分别 `ALIGN(0x1000)` 之后，
`user/hello` 的 ELF 是 4 个 `PT_LOAD`（`RX` / `R` / `RW` / `RW`（`.bss`））、4 个映像页、2 个可写页。

`make user` 不启 QEMU 就能独立验证这套约定：`tools/check_user_elf.py` 检查基址、段页对齐、
`W^X` 不重叠、映像窗口与「无加载时重定位」。

### 3.2 文件语义对接（已完成）

| 项 | 实现位置 | 说明 |
| ---- | ---- | ---- |
| fd 表 | `kernel/src/fs/file.rs` | 0/1/2 保留（stdin/串口），动态 fd 从 3 开始、取最小空闲槽位；记录 `path` + `offset` + 模式 |
| `open` | `syscall/file.rs::sys_open` | 路径用「指针 + 长度」；带 `O_CREAT` 时不存在则创建（`EEXIST` 视为成功） |
| `read`/`write` | `syscall/file.rs` | 用户缓冲按页校验（读要求可写、写要求可读）；fd ≥ 3 转 `vfs::read/write` |
| VFS/RAMFS | `kernel/src/fs/` | 见 [filesystem.md](filesystem.md)；挂载表最长前缀路由，`/mnt` 上挂第二个 RAMFS |

### 3.3 验收（v0.7，实测）

`make test-user`（v0.7 时 32 项；v0.9 第一步增至 59 项，见 3.5）与 `make test-fs`（19 项）
串口断言全部 PASS，关键证据：

- [x] **ELF 从文件装载**：`fs: create /init` → `user: program=/init source=elf bytes=15596 segments=4
      image_pages=7 writable_pages=2 entry=0x8000000000`（`source=elf` 与 `segments/writable_pages`
      是「走的是 ELF Loader 且段权限生效」的证据；`bytes`/`image_pages` 随 `/init` 加入堆自测而增长）；
- [x] **文件 syscall 全链路**：`user: open path=/etc/motd flags=0x0 fd=3` →
      `user: read fd=3 want=64 got=23` → 内容由用户态 `write` 打到串口（`user: write fd=1 len=23`）→
      `user: read fd=3 want=64 got=0`（EOF，证明偏移在推进）→ `user: close fd=3`；
- [x] **用户写入落到文件系统**：`user: open path=/tmp/hello.txt flags=0x41 fd=3`（fd 复用 3）→
      `user: write fd=3 len=24` → 内核读回 `fs: readback /tmp/hello.txt len=24` +
      `fs: user data: written by user program`；
- [x] **`RW` 段对 ring3 可写**：`hello: static counter=1`（v0.6 会 #PF —— 代码页整体只读）；
- [x] **syscall 参数校验**：`user: EFAULT write ptr=0xdead00000000 len=1 (buffer not readable in user space)`
      且用户侧收到 `hello: bad write ret=-14`；
- [x] **寄存器保存规则**：一次 `syscall` 往返后除 `rax`/`rcx`/`r11` 外全部寄存器保持进入前的值
      （Linux syscall ABI 语义，见 2.1 与 B-15）；
- [x] 隔离与退出码回归不变：`FAULT vector=0xe error=0x5 cr2=0x2000000` → `user: isolation PASS`，
      `user: exit code=42` → `user: init PASS`；
- [ ] 除 `/init` 之外的**多个**用户程序（需要 `argc/argv` 或 shell）→ v0.9；
- [ ] `ls`/`cat`/`cd` 用户程序 → v0.9（v0.7 在内核侧与 `/init` 里验证了同样的语义）。

### 3.4 仍未做（v0.9 起逐步补齐）

- ~~用户堆~~ → **已做**（3.5：`brk`/`mmap`/`munmap` + libc 全局分配器）；仍缺 `mprotect`
  （`PROT_NONE`、W^X 转换）与文件映射；
- `argc/argv`／环境变量传递：`_start` 不接收参数；
- `fork`/`execve`/`wait4` 与进程对象（`exit` 仍是「结束当前用户态执行」）；
- `stac`/`clac` 精细控制（内核目前直接读写用户页）、内核高半区迁移；
- 动态链接 / `PT_INTERP` / `ET_DYN`（只接受 `ET_EXEC` + 无重定位）。

### 3.5 用户堆（v0.9 第一步，2026-10-11 落地）

v0.9 的运行时前置条件按依赖顺序补：**先有可分配的用户内存**（本版），再有 `argc/argv`、
`fork`/`execve`/`wait4` 与进程对象，最后才是动态链接（`ld.so`）与 shell。
理由很直接：glibc、`ld.so`、任何真实程序的第一件事都是要堆。

| 项 | 实现位置 | 说明 |
| ---- | ---- | ---- |
| 地址空间窗口 | `syscall/address_space.rs` | brk 窗口 256 KiB（页 32..95）+ 匿名 mmap 窗口 256 KiB（页 96..159），均**按需映射**：没碰过的页 `page_flags` 返回 `None`，因此指向它的用户指针会被 syscall 层判为 `EFAULT` |
| `brk` / `mmap` / `munmap` | `syscall/memory.rs` | 语义与错误码见 2.7；每个失败路径都留一行 `user: <op> rejected (...)` 或 `user: <op> errno=<n>` |
| 用户态分配器 | `libc/src/heap.rs` | `#[global_allocator]`（bump over `mmap`，64 KiB 块，`dealloc` 不回收）—— `Vec`/`String`/`Box` 从此可用 |
| 自测/验收 | `user/hello/src/main.rs` | 堆自测写在参考用户程序里（因此“堆能不能用”是 **ring3 事实**，不是内核侧自说自话） |

四条有实测理由的性质（`make test-user` 逐条断言）：

1. **新页必须是零**：`hello: heap zeroed=1` / `hello: mmap zeroed=1`。把上一任使用者的内容
   送给新调用方是信息泄漏，所以 `brk`/`mmap` 分配帧时都先清零；
2. **堆页要能当 syscall 缓冲**：用户程序把**堆上**的 26 字节交给 `write` 打到串口
   （`user: write fd=1 len=26` + `hello: heap write ret=26`）——证明 `check_range` 经 `page_flags`
   认可堆/mmap 页，而不是只认映像窗口与栈；
3. **`munmap` 之后真的访问不到**：清页表项的同时 `invlpg`（`paging::flush_tlb`）。
   少了刷新就会出现「页帧已归还、TLB 里还是旧映射」的幽灵访问；实测里释放后的同一地址
   又被下一次 `mmap` 复用（`user: mmap len=65536 ... -> 0x8000060000`），旧映射没有残留；
4. **用户态结束后页帧全部归还**：`UserSpace::Drop` 释放映像/栈/堆/mmap 四类页帧；
   `user: frames free before=119159 after=119159` → `user: heap frame reclamation PASS`
   （内核在 selftest 里直接对比帧分配器的 `free_frames`，不靠日志措辞）。

负例也逐条具体（不是「都失败就行」）：文件映射 / `MAP_SHARED` / `PROT_WRITE|PROT_EXEC` /
未对齐的 `munmap` / 重复 `munmap` → `-EINVAL`(-22)；超过 256 KiB 窗口 → `-ENOMEM`(-12)。

**仍然没有的**（写下来避免误读）：`mprotect`（因此 `PROT_NONE` 与「先写后换只执行」都不支持）、
文件映射（`mmap` 无 offset 参数、`fd` 必须 -1）、回收式分配器（`dealloc` 是空操作：
一次运行结束由内核整棵释放；长驻程序的内存占用会单调上升）。

## 4. 调用链示例

实际调用链（v0.7 + v0.9 用户堆）：

```text
用户态 syscall 指令
  → syscall_entry（保存用户 RSP + rdi/rsi/rdx/r10/r8/r9，切系统调用栈，压 rcx/r11/rax）
  → syscall_dispatch（kernel/src/syscall/mod.rs）
  → syscall_table::parse（号 → handler）
  → file::sys_read/sys_write/sys_open/sys_close
       └─ fs::file（fd 表）→ fs::vfs（挂载表路由）→ fs::ramfs
  → memory::sys_mmap / sys_munmap / sys_brk（用户堆：地址空间窗口 + 页帧分配）
  → process::sys_exit / sys_getpid / sys_sleep
  → sysretq 回到用户态（恢复 rdi/rsi/rdx/r10/r8/r9；exit 与 ring3 异常走 leave_user 回内核）
```

v0.6 遗留的「`write` 把串口写死」已按 ADR-004 收口：fd 1/2 继续走串口（它是控制台设备），
fd ≥ 3 一律经 VFS 抽象路由，syscall 层不认识具体后端。

## 5. 非目标 / 后续

- 完整 ELF 动态链接：非第一版目标。
- 完整 POSIX：非第一版目标。
- 完整内存保护细节（`stac/clac` 精细控制、内核高半区迁移）：在 v0.7 起记录，必要时追加 ADR。

## 6. 修订记录

| 日期 | 变更 |
| ---- | ---- |
| 2026-10-06 | 第一阶段创建占位 |
| 2026-10-07 | 补记路线细化：明确 v0.6/v0.7 的分工、syscall 列表拆分、libc 封装位置与验收占位 |
| 2026-10-08 | 本次 docs 迭代统一文档顶部元信息与修订记录 |
| 2026-10-10 | **v0.6 核心落地**：填入真实入口选型（`syscall`/`sysret`）、寄存器/错误码约定、GDT 布局约束、用户地址空间构造、ring3 异常终止语义、首批 4 个 syscall 与 `make test-user` 验收结果；补记实测坑（固件页表只读 → 惰性清 `USER` 位）与 v0.6 明确未做项 |
| 2026-10-11 | **v0.6 用户态脚手架可用**：新增 2.11（`libc/` API、`user.ld` 链接约束、large 代码模型/`user-release` profile、构建期摊平与检查项）；2.8 改为「用户程序来源」并区分 `source=libc` 与 `source=asm`；2.4 更新为多代码页映射；验收项增至 19 项 |
| 2026-10-11 | **v0.9 第一步：用户堆** —— 新增 `syscall/memory.rs`（`brk`/`mmap`/`munmap`，含 W^X、`MAP_*` 白名单、5 参数无 offset 的取舍说明与逐条错误码）+ `address_space.rs` 的堆窗口（256 KiB）与匿名 mmap 窗口（256 KiB，first-fit，含 `invlpg`）；`libc` 新增 `heap`（`#[global_allocator]`，bump over mmap）与 `brk/mmap/munmap` 封装；`user/hello` 加入堆自测（零页、模式读回、堆缓冲 `write`、6 个负例、`Vec`/`format!`）；`user.rs` 新增页帧泄漏检查（`frames free before/after` → `heap frame reclamation PASS`）；`make test-user` 断言由 32 项增至 **59 项**（`orchestrate.json` 的 `user` 场景同步），全量回归（`test-fs` 19 项 / `test-driver` 22 项 / `test-fat` 21 项 / `make go-test`）PASS |
| 2026-10-11 | **v0.7 落地**：第 3 节从规划改为实现记录 —— ELF Loader（解析/按段权限逐页映射/入口取 `e_entry`）、用户地址布局（映像窗口 64 KiB + 栈 4 页）、段页对齐与 `.lbss` 孤儿段的实测坑、`make user` 的独立 ELF 检查（`tools/check_user_elf.py`）；2.7 填入 `read/open/write/close`（含 fd 表与两条硬规则）；2.10 标注 v0.7 已做项；4 节更新调用链；3.4 列出仍未做项（用户堆/`argc/argv`/`fork` 系/动态链接） |
