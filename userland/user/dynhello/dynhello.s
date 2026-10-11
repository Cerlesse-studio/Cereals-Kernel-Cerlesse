/*
 * dynhello —— v0.9 动态链接最小闭环的**动态可执行文件**（ET_DYN / PIE）。
 *
 * 它不是一个 Rust 程序：裸目标（x86_64-unknown-none）不支持 `cdylib`，因此
 * 这一对「动态主程序 + 共享库」由 **binutils**（`as` + `ld`）在构建期产出，
 * 见 `kernel/build.rs::build_dynamic_fixture`。整个环路要验证的能力：
 *
 *   PT_INTERP        → 内核解析出 /lib/ld.so，并真的把该对象装载进用户地址空间
 *   DT_NEEDED        → 内核解析出 libgreet.so，从 RAMFS /lib 读进来装载
 *   R_X86_64_RELATIVE→ .data 里的 `msg_ptr`（.quad main_msg）由内核按装载基址修正
 *   R_X86_64_JUMP_SLOT→ `call greet@PLT` 的 GOT 槽由内核填上共享库里的 greet 地址
 *
 * 编译链接（与 kernel/build.rs 成对维护，改一处要同步另一处）：
 *
 *   as -o dynhello.o user/dynhello/dynhello.s
 *   ld -pie --dynamic-linker=/lib/ld.so -L<dir-of-libgreet.so> -lgreet -o dynhello dynhello.o
 *
 * `-pie` 让链接地址从 0 开始（位置无关）；内核装载时选择运行基址（USER_VIRT_BASE），
 * 因此链接期地址只能通过重定位修正 —— 这正是本夹具要暴露的东西。
 *
 * 独立复核：`make dyn-hostcheck` 会把同一份源码用**宿主机的 glibc ld.so**
 * （`--dynamic-linker=/lib64/ld-linux-x86-64.so.2`）链一遍并在 Linux 上真跑，
 * 输出与退出码必须与 QEMU 一致 —— 两套独立装载器互相印证。
 */

    .section .rodata
main_msg:
    .ascii "main: dynamic link ok\n"
    .set MAIN_MSG_LEN, . - main_msg

    /* .data 里的绝对指针：链接期是 main_msg 的虚拟地址，装载期必须 + 基址
     * （R_X86_64_RELATIVE）。这里刻意**不走 PLT**，让 RELATIVE 单独可见。 */
    .data
    .align 8
msg_ptr:
    .quad main_msg

    .text
    .globl _start
    .type _start,@function
_start:
    /* write(1, msg_ptr, len)：msg_ptr 由 R_X86_64_RELATIVE 修正 */
    movl $1, %eax
    movl $1, %edi
    movq msg_ptr(%rip), %rsi
    movl $MAIN_MSG_LEN, %edx
    syscall

    /* greet()：经 PLT → GOT，GOT 由 R_X86_64_JUMP_SLOT 填成共享库里的地址 */
    call greet@PLT

    /* exit(greet 的返回值) */
    movl %eax, %edi
    movl $60, %eax
    syscall
    .size _start, . - _start
