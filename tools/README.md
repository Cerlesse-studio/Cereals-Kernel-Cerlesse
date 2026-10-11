# tools/ — 主机侧 Python 工具

语言分工见 docs/architecture.md 第 2 节：Python 只运行在开发主机，不进入内核。

| 工具 | 用途 | 计划版本 | 状态 |
| ---- | ---- | ---- | ---- |
| `image-builder/` | 组装 boot + kernel + 文件系统磁盘镜像（64 MiB FAT，mtools） | v0.1 | ✅ |
| `check_user_elf.py` | 用户程序 ELF 段布局检查（基址/页对齐/`W^X`/映像窗口/无重定位） | v0.7 | ✅ |
| `mkfs/` | 文件系统镜像生成（FAT 空盘/预置文件，供磁盘 FS 测试） | v0.8 | ⬜ 占位 |
| （tests/ 驱动） | QEMU 启动与串口日志断言 | v0.1 | ✅ |

状态：除 `mkfs/` 外均已实现。

注（v0.8，2026-10-11）：块设备层（`kernel/src/driver/block.rs` + `virtio_blk.rs`）已落地，
QEMU 启动盘已经是 **virtio-blk（legacy）**，因此 `mkfs/` 只等 FAT32 后端（v0.8 剩余部分）一起做 ——
届时它负责生成「带已知文件集的 FAT 镜像」，让内核侧能断言「从磁盘读到了哪个文件」。
