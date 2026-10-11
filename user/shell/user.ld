/* Cerlesse 用户程序链接脚本（Shell 用；与 user/hello/user.ld 逐条相同的四类约束）
 *
 * 1. 装载地址 = 0x0000_0080_0000_0000（PML4 下标 1，与内核
 *    `kernel/src/syscall/address_space.rs::USER_VIRT_BASE` 必须一致）。
 *    内核不做加载时重定位，链接地址必须等于运行地址；
 * 2. 每个段 **页对齐**（`ALIGN(0x1000)`）：v0.7 的 ELF Loader 按页设置权限
 *    （`p_flags` → `NO_EXEC`/`WRITABLE`），同页既 W 又 X 会让 W^X 退化，装载器直接拒绝；
 * 3. `.text.start` 排在映像最前（`_start` 在 libc 的 `global_asm!` 里）；
 * 4. 只保留可装载段：`.eh_frame`/`.comment` 之类丢掉（映像窗口只有 16 页）。
 *
 * 与 hello 的差异：没有。Shell 更在乎映像大小（解析 + 格式化代码），
 * 因此 `.lbss.*` 也一并列出：零初始化的静态量在 large 代码模型下会进
 * `.lbss.<symbol>`，漏掉它们会变成孤儿段被塞在 `.data` 之后 → RW 段不再页对齐。
 *
 * 链接地址在 small 代码模型范围之外，所以必须用 large 代码模型构建：
 *   RUSTFLAGS="-C relocation-model=static -C code-model=large"
 * 该标志由 `Makefile` 的 `user` 目标与 `kernel/build.rs` 成对维护。
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

    /* `RW` 段的 NOBITS 尾巴 */
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
