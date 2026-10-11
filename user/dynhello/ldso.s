/*
 * ld.so —— PT_INTERP 指向的「解释器」占位对象（ET_DYN）。
 *
 * v0.9 的最小闭环把**装载/链接的职责放在内核**（`kernel/src/syscall/dynamic.rs`）：
 * 真正的用户态 `ld.so`（自己解析 .dynamic、自己 mmap 文件、自己解析符号）不在
 * 本版范围内 —— `docs/architecture.md` 第 5 节明确把「完整 ELF 动态链接」列为非目标。
 *
 * 但 PT_INTERP **必须被真的解析、且指向的工件事先存在**：内核会读 `/lib/ld.so`、
 * 把这份 ET_DYN 像任何共享对象一样映射进「共享库窗口」并处理它的 `.dynamic`
 * （这里只有一条 NULL）。于是「PT_INTERP 解析 → 装载解释器」这条路径有可断言的
 * 证据（串口 `dyn: interp=/lib/ld.so ... base=... pages=1`），而不是只读了个字符串。
 *
 * 编译链接：
 *   as -o ldso.o user/dynhello/ldso.s
 *   ld -shared -o ld.so ldso.o
 */

    .text
    .globl _cerlesse_interp_marker
    .type _cerlesse_interp_marker,@function
_cerlesse_interp_marker:
    ret
    .size _cerlesse_interp_marker, . - _cerlesse_interp_marker
