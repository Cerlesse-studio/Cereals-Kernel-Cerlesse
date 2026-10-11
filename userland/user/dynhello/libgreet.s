/*
 * libgreet.so —— v0.9 动态链接最小闭环的**共享库**（ET_DYN，DT_SONAME=libgreet.so）。
 *
 * 只做两件事，但两件都必须是「真的发生了」：
 *   1. 在 ring3 里发一次 `write(1, ...)` —— 缓冲区在**共享库自己的 .rodata**里，
 *      它的虚拟地址落在内核新建的「共享库窗口」（USER_DYN_BASE），因此这条
 *      串口输出同时证明：库被装载到了正确的窗口、且 syscall 的用户缓冲校验
 *      认可该窗口（见 `address_space.rs::page_flags`）；
 *   2. 返回 42 —— 主程序把它当退出码，`make test-dyn` 断言 `exit code=42`。
 *
 * 库内部刻意只用 PC 相对寻址（`leaq greet_msg(%rip), %rsi`），
 * 因此它自己没有任何重定位 —— 需要内核处理的重定位集中在主程序里，最小化噪音。
 *
 * 编译链接：
 *   as -o libgreet.o user/dynhello/libgreet.s
 *   ld -shared -soname libgreet.so -o libgreet.so libgreet.o
 */

    .section .rodata
greet_msg:
    .ascii "greet: hello from libgreet.so\n"
    .set GREET_MSG_LEN, . - greet_msg

    .text
    .globl greet
    .type greet,@function
greet:
    movl $1, %eax
    movl $1, %edi
    leaq greet_msg(%rip), %rsi
    movl $GREET_MSG_LEN, %edx
    syscall
    movl $42, %eax
    ret
    .size greet, . - greet
