/* Cerlesse 用户程序链接脚本（`user/hello/user.ld` 的副本；v0.9 第四步的程序沿用）
 *
 * 这份脚本与 `user/hello/user.ld` **必须逐条一致**：内核的 ELF Loader 与
 * `kernel/build.rs` 的构建期校验都建立在这几条约定上。改一处要同步全部 `user/*`。
 *
 * 1. 装载地址 = 0x0000_0080_0000_0000（PML4 下标 1，与
 *    `kernel/src/syscall/address_space.rs::USER_VIRT_BASE` 一致）。内核不做加载时
 *    重定位，链接地址必须等于运行地址；
 * 2. 每个段 **页对齐**：装载器按 `p_flags` 逐页设权限，同页既 W 又 X 会被拒绝
 *    （`user: ELF rejected: W and X segments share a page`）；
 * 3. `.text.start` 排在映像最前（`_start` 就在这个段里，见 libc 的 `global_asm!`）；
 * 4. 只保留可装载段：`.eh_frame`/`.comment`/`.note` 丢掉，避免把映像撑大。
 *    `.bss` 显式列 `.lbss.*`：large 代码模型下零初始化静态量落在按名字命名的段里，
 *    漏掉它们会变成孤儿段、破坏 RW 段的对齐。
 *
 * 链接地址在 2^39（> 2GiB），默认 small 代码模型会产生 R_X86_64_32S 重定位而
 * 链接失败，因此用户程序必须用 large 代码模型构建：
 *   RUSTFLAGS="-C relocation-model=static -C code-model=large"
 * 该标志由 `kernel/build.rs` 与 `Makefile` 的 `user` 目标成对维护。
 * large 模型与 LTO 不兼容，所以用 workspace 的 `user-release` profile。
 */
ENTRY(_start)

SECTIONS
{
    . = 0x8000000000;

    /* `R E` 段：入口在最前 */
    .text : ALIGN(0x1000) {
        KEEP(*(.text.start))
        *(.text .text.*)
        *(.ltext .ltext.*)
    }

    /* `R` 段：只读数据（不可执行） */
    .rodata : ALIGN(0x1000) {
        *(.rodata .rodata.*)
        *(.lrodata .lrodata.*)
    }

    /* `RW` 段：可写数据（不可执行） */
    .data : ALIGN(0x1000) {
        *(.data .data.*)
        *(.ldata .ldata.*)
        *(.data.rel.ro .data.rel.ro.*)
        *(.got .got.*)
    }

    /* `RW` 段的 NOBITS 尾巴：静态变量的零初始化部分 */
    .bss : ALIGN(0x1000) {
        *(COMMON)
        *(.bss .bss.*)
        *(.lbss .lbss.*)
    }

    /DISCARD/ : {
        *(.eh_frame .eh_frame_hdr)
        *(.comment)
        *(.note .note.*)
    }
}
