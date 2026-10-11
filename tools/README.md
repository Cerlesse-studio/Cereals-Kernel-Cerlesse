# tools/ — 主机侧 Python 工具

语言分工见 docs/architecture.md 第 2 节：Python 只运行在开发主机，不进入内核。

| 工具 | 用途 | 计划版本 | 状态 |
| ---- | ---- | ---- | ---- |
| `image-builder/` | 组装 boot + kernel + 文件系统磁盘镜像（64 MiB FAT，mtools） | v0.1 | ✅ |
| `check_user_elf.py` | 用户程序 ELF 段布局检查（基址/页对齐/`W^X`/映像窗口/无重定位） | v0.7 | ✅ |
| `mkfs/` | 文件系统镜像生成（FAT12/16/32 空盘或预置目录树，供磁盘 FS 测试） | v0.8 | ✅ |
| （tests/ 驱动） | QEMU 启动与串口日志断言 | v0.1 | ✅ |

状态：均已实现。

`mkfs/mkfs.py`（H-03）自己生成 FAT 镜像，**不调用 `mformat`**；正确性由 mtools 独立复核：

```bash
python3 tools/mkfs/mkfs.py --out build/fat.img --from tests/fatroot/data   # `make fat-image`
mdir  -i build/fat.img ::                  # 独立实现（mtools）读同一张盘
mtype -i build/fat.img ::hello.txt
```

镜像在 `make test-fat` 里作为第二块 virtio-blk 盘挂到 `/mnt/fat`，
内核侧逐项断言文件名/大小/内容（跨簇、空文件、长文件名、二级子目录），见 `tests/fatroot/README.md`。
