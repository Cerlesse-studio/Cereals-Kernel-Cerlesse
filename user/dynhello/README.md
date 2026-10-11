# user/dynhello —— v0.9 动态链接最小闭环的夹具

这里不是普通的 Rust 用户程序（`user/{hello,shell,ls,…}`）。裸目标
`x86_64-unknown-none` **不支持 `cdylib`**，而动态链接恰恰需要「共享库 + 带
PLT/GOT 的 PIE」，所以这一组工件由 **binutils**（`as` + `ld`）在**构建期**
现场汇编 / 链接，由 `kernel/build.rs::build_dynamic_fixtures` 产出到
`$OUT_DIR/dyn/` 并被内核 `include_bytes!` 内嵌：

| 源 | 产物 | 角色 |
| ---- | ---- | ---- |
| `dynhello.s` | `dyn/dynhello` | ET_DYN PIE：`PT_INTERP=/lib/ld.so` + `DT_NEEDED=libgreet.so` |
| `libgreet.s` | `dyn/libgreet.so` | ET_DYN 共享库：导出 `greet` |
| `ldso.s` | `dyn/ld.so` | `PT_INTERP` 指向的解释器占位（ET_DYN） |

启动时三个产物都作为**普通文件**落进 RAMFS：`/bin/dynhello`、`/lib/libgreet.so`、
`/lib/ld.so`。内核从 VFS 按 `PT_INTERP` / `DT_NEEDED` 里的**名字**读它们 ——
和真实系统里「解释器与 `.so` 就是文件系统里的文件」一致。

## 这个闭环验证了什么（逐步对应内核代码）

```text
dynhello 的 PT_INTERP=/lib/ld.so      → syscall/dynamic.rs 解析出解释器路径
        DT_NEEDED=libgreet.so         → 拼成 /lib/libgreet.so 并从 RAMFS 读入
        R_X86_64_RELATIVE  (msg_ptr)  → 数据指针按装载基址修正
        R_X86_64_JUMP_SLOT (greet)    → 跨对象符号解析，GOT 槽填共享库地址
        ↓
    主程序写 "main: dynamic link ok"（自己的 .rodata）
    greet() 写 "greet: hello from libgreet.so"（共享库的 .rodata，在共享库窗口里）
    退出码 = greet() 的返回值 42
```

可断言的串口轨迹（`make test-dyn`）：

```text
dyn: PT_INTERP=/lib/ld.so
dyn: load <main> base=0x8000000000 segments=4 pages=5
dyn: load /lib/ld.so base=0x8000100000 segments=4 pages=3
dyn: NEEDED libgreet.so -> /lib/libgreet.so
dyn: load /lib/libgreet.so base=0x8000103000 segments=4 pages=4
dyn: reloc R_X86_64_JUMP_SLOT sym=greet -> obj=/lib/libgreet.so
dyn: link PASS objects=3 relocs=2 entry=0x8000001020
main: dynamic link ok
greet: hello from libgreet.so
user: exit code=42
user: dynamic link PASS (PT_INTERP + libgreet.so + RELATIVE/JUMP_SLOT)
```

## 边界（刻意，见 docs/architecture.md 第 5 节）

「完整 ELF 动态链接」是**非目标**。本版实现的是最小闭环：

- 链接由**内核**做（`kernel/src/syscall/dynamic.rs`）；`PT_INTERP` 指向的解释器
  **会被装载和重定位，但不被进入**。真正的用户态 `ld.so` 需要一个能 `mmap`
  文件的内核，留到后续版本。
- 只处理 `R_X86_64_RELATIVE` / `GLOB_DAT` / `JUMP_SLOT` / `R_X86_64_64`，
  符号按名字**线性扫描** `.dynsym`，不支持版本节、`rpath`、TLS、`INIT/FINI`。

## 独立复核：用宿主机的 glibc ld.so 跑同一份夹具

`make dyn-hostcheck` 会把**同一批 `.s`** 用宿主机真实的动态链接器
（`/lib64/ld-linux-x86-64.so.2`）重新汇编 / 链接并在 Linux 上执行。
两个完全不同实现的装载器（内核 vs glibc）都要把它跑出相同的两行输出与
`exit(42)`，才说明 ELF 结构 / 机器码 / 重定位语义是对的 —— 与
`mkfs.py` 用 mtools 复核是同一个思路。环境没有 binutils / glibc 时该目标**跳过**。

## 手动构建

```bash
# 内核构建会自动做这一步；这里只用于单独调试
as -o /tmp/dynhello.o user/dynhello/dynhello.s
as -o /tmp/libgreet.o user/dynhello/libgreet.s
as -o /tmp/ldso.o     user/dynhello/ldso.s
ld -shared -soname libgreet.so -o /tmp/libgreet.so /tmp/libgreet.o
ld -shared -o /tmp/ld.so /tmp/ldso.o
ld -pie --dynamic-linker=/lib/ld.so -L/tmp -lgreet -o /tmp/dynhello /tmp/dynhello.o
```

> 构建内核因此需要 **binutils**（`as`/`ld`）在 `PATH` 上；`ld` 也可以用
> `rust-lld -flavor gnu` 代替。`make dyn-hostcheck` 会明确提示缺失的工具。
