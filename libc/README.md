# libc（最小用户态运行时）

- 版本：v0.6 建立 / **v0.7 补齐文件 syscall**
- 状态：✅ 已实现（2026-10-11，随 `make test-user` 32 项断言实测通过；`user/hello` 作为 RAMFS 上的
  `/init` 经 ELF Loader 装载并跑在 ring3）
- 关联：[docs/syscall.md](../docs/syscall.md) 2.11 / 3.1、[user/hello](../user/hello)、
  [docs/filesystem.md](../docs/filesystem.md) 2.6

## 1. 这是什么

`libc` 把内核 ABI 封装成用户程序可以直接调用的形状，让新用户程序**不必再手写机器码**。
它是用户态脚手架的底座：`libc`（运行时）+ `user/*`（用户程序）+ `kernel/build.rs`
（构建、校验 ELF 并内嵌）+ 内核 ELF Loader（`syscall/elf.rs`）。

| 组成 | 内容 |
| ---- | ---- |
| `syscall` | 系统调用号常量 + 裸入口 `raw(nr, a1..a5)` + `write` / `read` / `open` / `close` / `getpid` / `nanosleep` / `exit` |
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

判定错误请用 `libc` 里的 errno 常量（`ENOENT`/`EBADF`/`EFAULT`/`EISDIR`/`EINVAL`/`ENOSPC`/`ENOMEM`）。\
**寄存器约定**：与 Linux syscall ABI 一致 —— 一次往返后只有 `rax`（返回值）、`rcx`、`r11` 变化，
其余寄存器保持进入前的值（内核入口汇编会保存/恢复 `rdi/rsi/rdx/r10/r8/r9`）。

## 3. 新增一个用户程序

1. 复制 `user/hello/`（`Cargo.toml` + `build.rs` + `user.ld` + `src/main.rs`）并改包名；
2. 在 workspace 根 `Cargo.toml` 的 `members` 里加上 `"user/<程序名>"`；
3. 在 `kernel/build.rs` 里让它参与构建与内嵌（`USER_PROGRAM` / `include_bytes!` 路径），
   内核侧按需增加一次装载调用（当前内核把内嵌 ELF 写进 RAMFS 的 `/init`）；
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
4. **只能用「映像窗口 + 4 页栈」**：映像（`.text`/`.rodata`/`.data`/`.bss`）上限 64 KiB、
   栈 16 KiB；**没有用户堆** —— `brk`/`mmap` 仍是 `ENOSYS`，动态内存只能放在栈或静态数组里。

`make user` 会跑 `tools/check_user_elf.py`，逐项检查上述 1/2（以及映像窗口、无重定位），
不用启动 QEMU 就能在改坏链接脚本时立刻发现。

尚未封装的调用（`fork`/`execve`/`wait4`）在内核侧仍是 ENOSYS 桩；用户堆与 `argc/argv` 属 v0.8+。
