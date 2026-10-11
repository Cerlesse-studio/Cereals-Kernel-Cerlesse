# tools/ — 主机侧 Python 工具

语言分工见 docs/architecture.md 第 2 节：Python 只运行在开发主机，不进入内核。

| 工具 | 用途 | 计划版本 | 状态 |
| ---- | ---- | ---- | ---- |
| `image-builder/` | 组装 boot + kernel + 文件系统磁盘镜像（64 MiB FAT，mtools） | v0.1 | ✅ |
| `check_user_elf.py` | 用户程序 ELF 段布局检查（基址/页对齐/`W^X`/映像窗口/无重定位） | v0.7 | ✅ |
| `mkfs/` | 文件系统镜像生成（FAT12/16/32 空盘或预置目录树，供磁盘 FS 测试） | v0.8 | ✅ |
| `pkgmgr/ckc.py` | **CKC 包容器（后缀 `.ckc`）**：`pack` / `verify` / `info`（确定性 ustar + 64 字节头） | H-06 P0a | ✅ |
| （tests/ 驱动） | QEMU 启动与串口日志断言 | v0.1 | ✅ |

状态：均已实现（`pkgmgr/` 只实现了 P0a；事务/catalog 见 `docs/package-manager.md` 11.1）。

`mkfs/mkfs.py`（H-03）自己生成 FAT 镜像，**不调用 `mformat`**；正确性由 mtools 独立复核：

```bash
python3 tools/mkfs/mkfs.py --out build/fat.img --from tests/fatroot/data   # `make fat-image`
mdir  -i build/fat.img ::                  # 独立实现（mtools）读同一张盘
mtype -i build/fat.img ::hello.txt
```

镜像在 `make test-fat` 里作为第二块 virtio-blk 盘挂到 `/mnt/fat`，
内核侧逐项断言文件名/大小/内容（跨簇、空文件、长文件名、二级子目录），见 `tests/fatroot/README.md`。

## `pkgmgr/ckc.py` —— CKC 包容器（`.ckc`）

内核对包管理器的设计见 `docs/package-manager.md`（3.2 是容器规格）。这里是 P0a 的实现：
后缀 `.ckc`（CKC = Cerlesse Kernel Container）、头魔数 `CKC\x01`、payload 是**确定性 ustar**，
身份 = payload 的 SHA-256（与打包时间/机器无关）。

```bash
python3 tools/pkgmgr/ckc.py pack   user/hello -o build/hello-0.9.0.ckc --print-rev
python3 tools/pkgmgr/ckc.py verify build/hello-0.9.0.ckc
python3 tools/pkgmgr/ckc.py info   build/hello-0.9.0.ckc
```

`pack` 的输入是一棵带 `PKG.toml` 的目录树（只支持一个明确的 TOML 子集，见模块文档），
并会拒绝：目标越出 scope 白名单、`setuid`/`setgid`/`sticky`、`hooks = true`、重复 target、
超长路径（ustar 字段 100 字节）。退出码与 `docs/package-manager.md` 9.2 一致：
`0` 成功、`1` 用法/参数错误、`2` 校验失败、`4` I/O 错误。

**验证**：`make test-pkg`（`tests/test_ckc.py`，37 项，不启 QEMU）—— 其中最后几项是
**独立复核**：用系统 `tar` 读同一段 payload，条目清单与文件内容必须与 `ckc.py` 一致
（与自己验自己相对，与 `mkfs` + mtools 的做法同理）。
