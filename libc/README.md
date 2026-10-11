# libc（最小用户态运行时）

- 版本：v0.6 建立 / **v0.7 补齐文件 syscall** / **v0.9 第一步补齐用户堆** / **v0.9 第二步补齐 `stdin`** /
  **v0.9 第三步补齐进程调用与 `argc`/`argv`**
- 状态：✅ 已实现（2026-10-11，随 `make test-user` **61 项** + `make test-shell` **11 项** + `make test-proc` **8 项 + 4 正则**断言实测通过；
  `user/hello` 作为 RAMFS 上的 `/init`、`user/shell` 作为 `/bin/sh`、`user/hello-bin` 作为 `/bin/hello`
  （**由 Shell `fork` + `execve` 起来**），三者都经 ELF Loader 装载并跑在 ring3）
- 关联：[docs/syscall.md](../docs/syscall.md) 2.11 / 3.1 / 3.5 / 3.6 / 3.7、[user/hello](../user/hello)、
  [user/shell](../user/shell)、[user/hello-bin](../user/hello-bin)、[docs/process.md](../docs/process.md) 1.4、
  [docs/filesystem.md](../docs/filesystem.md) 2.6

## 1. 这是什么

`libc` 把内核 ABI 封装成用户程序可以直接调用的形状，让新用户程序**不必再手写机器码**。
它是用户态脚手架的底座：`libc`（运行时）+ `user/*`（用户程序）+ `kernel/build.rs`
（构建、校验 ELF 并内嵌）+ 内核 ELF Loader（`syscall/elf.rs`）。

| 组成 | 内容 |
| ---- | ---- |
| `syscall` | 系统调用号常量 + 裸入口 `raw(nr, a1..a5)` + `write` / `read` / `open` / `close` / `getpid` / `nanosleep` / `exit` / `brk` / `mmap` / `munmap` / `fork` / `execve` / `wait4` |
| `args` | `argc()` / `arg(i)` / `arg_str(i)` / `Args`：入口寄存器里的 `rdi`/`rsi` 由 `_start` 存进来（v0.9 第三步） |
| `heap` | `#[global_allocator]`（bump over `mmap`，64 KiB 块）—— 用户程序因此可以用 `Vec`/`String`/`Box`（v0.9 第一步） |
| `console` | `print!` / `println!`（固定 256 字节缓冲，**一次宏调用 = 一次 `write`**） |
| `_start` | `global_asm!` 放在 `.text.start`：`_start` → 用户 `main` → `exit(main())` |
| 运行时支撑 | `#[panic_handler]`（打 stderr 后 `exit(101)`）、`no_std`、errno / `O_*` 常量 |

## 2. 用法

```rust
// user/<程序名>/src/main.rs
#![no_std]
#![no_main]

use libc::{close, open, println, read, write, O_CREAT, O_RDONLY, O_WRONLY};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    println!("hello from user space"); // 一次 write（fd=1）

    // 读内核提供的文件（RAMFS）
    let fd = open("/etc/motd", O_RDONLY);   // fd ≥ 3，或负 errno
    if fd >= 0 {
        let mut buf = [0u8; 64];
        let n = read(fd as i32, &mut buf);  // 0 = EOF
        if n > 0 {
            let _ = write(1, &buf[..n as usize]);
        }
        let _ = close(fd as i32);
    }

    // 写文件（内核随后可从 RAMFS 读回，见 `fs: readback ...`）
    let wfd = open("/tmp/note.txt", O_CREAT | O_WRONLY);
    if wfd >= 0 {
        let _ = write(wfd as i32, b"written by user program\n");
        let _ = close(wfd as i32);
    }

    0 // 退出码
}
```

文件 syscall 的形状（与 Linux 的差异写在类型里）：

| 调用 | 参数 | 返回 |
| ---- | ---- | ---- |
| `open(path: &str, flags: i32)` | 路径用**指针 + 长度**（内核已校验区间，不在不可信内存上找 NUL）；`flags` = `O_RDONLY`/`O_WRONLY`/`O_RDWR` 与 `O_CREAT` 的组合 | fd（≥ 3）或负 errno |
| `read(fd, buf: &mut [u8])` | `buf` 必须对 ring3 **可写**（栈或 `RW` 段） | 读到字节数（0 = EOF）或负 errno |
| `write(fd, buf: &[u8])` | fd 1/2 写串口，fd ≥ 3 写文件 | 写出字节数或负 errno |
| `close(fd)` | 只接受 fd ≥ 3（0/1/2 是保留位） | 0 或负 errno |

判定错误请用 `libc` 里的 errno 常量（`ENOENT`/`EBADF`/`EAGAIN`/`EFAULT`/`EISDIR`/`EINVAL`/`ENOSPC`/`ENOMEM`/`ENOSYS`）。\
**`read(0)` 是行缓冲（cooked）**：内核交付**一整行**（行尾统一为 `\n`），所以 `cat` 之类的程序
不必自己拼行；没有整行的 3 秒内返回 `-EAGAIN`(-11)（无进程模型时内核不会无限等，
因此交互程序应该把 `EAGAIN` 当作“暂无输入”而不是错误）。\
**寄存器约定**：与 Linux syscall ABI 一致 —— 一次往返后只有 `rax`（返回值）、`rcx`、`r11` 变化，
其余寄存器保持进入前的值（内核入口汇编会保存/恢复 `rdi/rsi/rdx/r10/r8/r9`）。

## 3. 新增一个用户程序

1. 复制 `user/hello/`（`Cargo.toml` + `build.rs` + `user.ld` + `src/main.rs`）并改包名；
2. 在 workspace 根 `Cargo.toml` 的 `members` 里加上 `"user/<程序名>"`；
3. 在 `kernel/build.rs` 的 `USER_PROGRAMS` 列表里加上 `(包名, "<文件名>.elf")`，
   并在 `kernel/src/syscall/user.rs` 用 `include_bytes!` 取它、`fs::init` 写进 RAMFS
   （现有两个：`user-hello` → `/init`、`user-shell` → `/bin/sh`）；
4. 单独验证脚手架：`make user` —— 编译 + 链接 + `tools/check_user_elf.py` 检查段布局。

构建参数不是可选的（改错会在链接期或 ring3 立刻炸）：

| 参数 | 取值 | 原因 |
| ---- | ---- | ---- |
| 目标 | `x86_64-unknown-none` | 裸机用户态，无宿主 libc |
| 代码模型 | `-C code-model=large` | 链接地址 2^39 超出 small 模型 ±2GiB，否则 `R_X86_64_32S out of range` |
| 重定位模型 | `-C relocation-model=static` | 内核不做加载时重定位（ELF Loader 只接受 `ET_EXEC`） |
| profile | `user-release`（workspace 根定义） | large 模型与 LTO 不兼容，该 profile `lto = false`、`opt-level = 2` |
| 链接脚本 | `user.ld`（由各程序的 `build.rs` 传入） | 固定链接到 `USER_VIRT_BASE`，每个段 `ALIGN(0x1000)` |

## 4. 四条硬约束（都有实测理由）

1. **每个段必须页对齐**（`user.ld` 里 `ALIGN(0x1000)`）。v0.7 的 ELF Loader 按**页**设置权限
   （`W` → 可写、非 `X` → `NX`）；同页既 `W` 又 `X` 会让 `W^X` 退化，装载器直接拒绝：
   `user: ELF rejected: W and X segments share a page`；
2. **必须固定链接到 `USER_VIRT_BASE`（0x0000_0080_0000_0000）**：链接地址 = 运行地址，
   程序里的绝对地址因此不需要重定位（内核不做加载时重定位）；
3. **静态变量必须归入 `.bss`**：链接在 2^39（large 代码模型）时 rustc 把零初始化静态量放进
   按名字命名的 `.lbss.<symbol>` 段，`user.ld` 必须显式把它们收进 `.bss`，否则它们会变成
   **孤儿段**被 lld 塞在 `.data` 之后，RW 段不再页对齐（实测落在 `0x8000003040`）；
4. **内存窗口是固定布局**：映像（`.text`/`.rodata`/`.data`/`.bss`）上限 64 KiB、栈 16 KiB，
   v0.9 第一步又加了 **brk 堆窗口 256 KiB** 与 **匿名 mmap 窗口 256 KiB**（见第 5 节）；
   超出窗口的 `brk`/`mmap` 会失败（`brk` 返回原值、`mmap` 返回 `-ENOMEM`），
   而且**没有 `mprotect`**（`PROT_NONE` 与「先写后换只执行」仍不支持）。

`make user` 会跑 `tools/check_user_elf.py`，逐项检查上述 1/2（以及映像窗口、无重定位），
不用启动 QEMU 就能在改坏链接脚本时立刻发现。

尚未实现的调用：**只剩 `mprotect`**（W^X 转换用），它在内核侧落在公共 ENOSYS 桩上；
环境变量（`envp`）、用户态抢占与动态链接属后续版本。

## 5. 用户堆（v0.9 第一步）

```rust
use libc::{brk, mmap, munmap, MAP_ANONYMOUS, MAP_PRIVATE, PROT_READ, PROT_WRITE};
use alloc::vec::Vec;   // 用户程序里 `extern crate alloc;`

let base = brk(0);                       // 查询当前堆顶
let grown = brk(base + 8192);            // 增长两页（页对齐）
let p = base as *mut u8;                 // 新页保证全零，可直接写
unsafe { core::ptr::write_volatile(p, 7) };
println!("brk {} -> {}", base, grown);

let m = mmap(4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS); // >0 = 地址，<0 = -errno
let _ = munmap(m as u64, 4096);

let mut v: Vec<u32> = Vec::new();        // 全局分配器在 heap.rs，走 mmap
v.push(1);
```

内核侧语义（完整版见 [docs/syscall.md](../docs/syscall.md) 3.5）：

| 调用 | 语义 | 失败 |
| ---- | ---- | ---- |
| `brk(addr)` | `addr = 0` 查询堆顶；否则页对齐增长/收缩（256 KiB 窗口） | **不返回错误**：返回原值（用 `ret < addr` 判断） |
| `mmap(len, prot, flags)` | 只收 `MAP_PRIVATE\|MAP_ANONYMOUS`，`fd` 固定 -1，**无 offset 参数**；first-fit；新页全零 | `-EINVAL`（位非法/文件映射/`PROT_WRITE\|PROT_EXEC`/`PROT_NONE`）、`-ENOMEM`（超窗口） |
| `munmap(addr, len)` | 整段归还（要求页对齐且全段已映射） | `-EINVAL` |

分配器的**限制**（写在代码注释里，也写在这里）：`libc::heap` 是只前进的 bump 分配器，
`dealloc` 是空操作 —— 目前靠「一次运行结束由内核 `UserSpace::Drop` 整棵释放」兜底，
长驻程序的内存占用会单调上升；回收式分配器与 `mprotect` 属后续版本。

## 6. 交互式输入（v0.9 第二步）

`read(0)` 是**行缓冲**：内核的行缓冲（`driver/keyboard.rs`）已经做了行编辑与回显，
一次 `read(0)` 给你一整行（行尾是 `\n`，`\r` 不会出现）。没有整行时等待，但**最多等 3 秒**
后返回 `EAGAIN` —— 所以在 `-EAGAIN` 上要能继续循环（而不是当成错误退出）：

```rust
use libc::{print, println, read, EAGAIN, STDIN};

let mut line = [0u8; 128];
let mut idles = 0u32;
loop {
    print!("root@cerlesse C:>");            // 一次 write（日志里可见 len=17）
    let n = read(STDIN, &mut line);
    if n == EAGAIN {
        // 暂无输入：交互式程序应继续等；这里用连续两次空闲退出来避免卡住整机
        idles += 1;
        if idles >= 2 { break; }
        continue;
    }
    if n <= 0 { break; }                   // 其他错误（0 在 stdin 接通后不会出现）
    idles = 0;
    let text = core::str::from_utf8(&line[..n as usize]).unwrap_or("").trim();
    println!("got: {}", text);
}
```

参考实现是 [user/shell](../user/shell)（提示符、参数切分、内建命令、外部命令走的 `fork`+`execve`+`wait4`）。
外部程序现在真的能跑起来 —— 见第 7 节。

## 7. 进程调用与命令行参数（v0.9 第三步）

三个包装的后端是真正的进程对象（独立地址空间 + 独立内核栈 + 停在 `syscall` 里的完整
寄存器帧），内核侧实现见 [docs/process.md](../docs/process.md) 1.4：

| 调用 | 返回 | 说明 |
| ---- | ---- | ---- |
| `fork() -> isize` | 父进程：子进程 PID；子进程：`0`；失败：负 errno（进程表满 `EAGAIN`） | 地址空间与**当前寄存器帧**一起复制，子进程从同一条用户指令继续 |
| `execve(path, argv: &[&str]) -> isize` | **成功不返回**；失败：`-ENOENT` / `-E2BIG` / `-EFAULT` / `-EINVAL` | 换映像 + 换地址空间，`argv` 写进新进程的用户栈 |
| `wait4(pid: i64) -> isize` | 子进程退出码（可为负）；非直接子进程：`ECHILD` | 运行并**回收**子进程（地址空间整棵归还） |

标准写法原样可用（`user/shell` 的 `cmd_external` 就是这么写的）：

```rust
use libc::{execve, exit, fork, wait4};

let pid = fork();
if pid == 0 {
    let _ = execve("/bin/hello", &["hello", "world"]); // 成功不返回
    exit(127);                                          // 失败：exec 的常规退出码
}
let code = wait4(pid);
println!("exit code={}", code);
```

**参数在入口寄存器里**（不是 Linux 的「压在用户栈顶」）：`_start` 的 `rdi = argc`、
`rsi = argv`，由 `cer_start` 存进 `libc::args`。用户程序侧：

```rust
use libc::{arg_str, argc, println};

println!("argc={}", argc());
for i in 0..argc() {
    println!("argv[{}]={}", i, arg_str(i).unwrap_or("<non-utf8>"));
}
```

字符串由内核写在**新进程的用户栈**上（`syscall::process::build_user_frame`），
生命周期与进程相同，所以 `arg(i)` 给的是 `&'static [u8]`；上限 `ARG_MAX` = 8 项、
单项 256 字节。

**与 Linux 的调度差异**（刻意，且对调用方透明）：单 CPU、用户态无抢占，`fork` 之后
子进程**不会立刻运行**，它要等父进程 `wait4` 时才被恢复 —— 也就是「父进程在等的时候，
子进程在跑」。因此 `fork`/`execve`/`wait4` 的标准写法不需要为这个内核改写。
