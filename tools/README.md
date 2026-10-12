# tools/ — 主机侧 Python 工具

语言分工见 docs/architecture.md 第 2 节：Python 只运行在开发主机，不进入内核。

| 工具 | 用途 | 计划版本 | 状态 |
| ---- | ---- | ---- | ---- |
| `image-builder/` | 组装 boot + kernel + 文件系统磁盘镜像（64 MiB 设备；**FAT 只占前面 63 MiB**，尾部 1 MiB 是 CKM 状态区，见下，mtools） | v0.1 / CKM-2 | ✅ |
| `check-boundary.py` | **内核/用户态边界门禁**（内核不得调用外部命令/内嵌用户态字节/依赖用户态包），含 `--selftest` 反向自检 | 纯内核化阶段二 | ✅ |
| `check_user_elf.py` | 用户程序 ELF 段布局检查（基址/页对齐/`W^X`/映像窗口/无重定位）——**由 `userland/Makefile` 调用**（它检查的是用户态产物） | v0.7 | ✅ |
| `mkfs/` | 文件系统镜像生成（FAT12/16/32 空盘或预置目录树，供磁盘 FS 测试）；**已追加 GPT/MBR 分区表镜像生成**（GPT 两分区 / MBR 双分区 / 主表头损坏 / 无分区表四类夹具，见 `docs/storage.md` 第 9 节 `make storage-images`） | v0.8 / CSS-1 | ✅ |
| `pkgmgr/ckc.py` | **CKC 包容器（后缀 `.ckc`）**：`pack` / `verify` / `info`（确定性 ustar + 64 字节头） | H-06 P0a | ✅ |
| （tests/ 驱动） | QEMU 启动与串口日志断言 | v0.1 | ✅ |

状态：均已实现（`pkgmgr/` 只实现了 P0a；事务/catalog 见 `docs/package-manager.md` 11.1）。

边界门禁（`make check-boundary`）的六项检查与它们在纯内核化前后的实测结果：

```bash
python3 tools/check-boundary.py            # 内核一侧：违规即退出码 1
python3 tools/check-boundary.py --selftest # 注入 6 类违规样本，必须全部被抓到
```

| 编号 | 断言 | 改造前 | 改造后 |
| ---- | ---- | ---- | ---- |
| K1 | `kernel/build.rs` 不调用外部命令（嵌套 cargo / as / ld） | ❌ 2 处 | ✅ |
| K2 | `kernel/src/**.rs` 不直接 `include_bytes!` 用户态字节 | ❌ 10 处 | ✅（清单生成，空清单也合法） |
| K3 | 内核**代码**里不出现 `user/`、`libc/` 路径（注释引用允许） | ❌ 大量 | ✅ |
| K4 | `kernel/Cargo.toml` 依赖只有 `shared` / `abi` | ✅ | ✅ |
| K5 | 根 workspace 不含 `libc`、`user/*`，且 `exclude` 声明 `userland` | ❌ 9 项 | ✅ |
| K6 | 内核代码不出现用户态包名 / profile | ❌ | ✅ |

`--selftest` 保证门禁不是「永远 PASS」的空壳：它把 6 类违规写成临时仓库样本，
要求 6 项检查全部报错才算通过（`make check-boundary` 会同时跑两者）。

`mkfs/mkfs.py`（H-03）自己生成 FAT 镜像，**不调用 `mformat`**；正确性由 mtools 独立复核：

```bash
python3 tools/mkfs/mkfs.py --out build/fat.img --from tests/fatroot/data   # `make fat-image`
```

`image-builder/build_image.py` 的镜像布局（CKM-2 起）：

```text
偏移 0          FAT（由 mformat 在 **63 MiB** 的文件上创建；固件从这里读 BOOTX64.EFI）
偏移 63 MiB     CKM 状态区（1 MiB 原始扇区，双槽 A/B + CRC32，见 kernel/src/ckm/store.rs）
```

为什么是「先按 63 MiB 格式化、再追加」：mtools 的 FAT 几何按**文件大小**算，
因此文件系统看不到尾部这 1 MiB —— 状态区在 FAT 之外，不与用户文件争簇；
而设备容量仍是 64 MiB（内核看到 `sectors=131072`，分区识别仍是 `table=none partitions=1`）。

```text
mdir  -i build/fat.img ::                  # 独立实现（mtools）读同一张盘
mtype -i build/fat.img ::hello.txt
```

镜像在 `make test-fat` 里作为第二块 virtio-blk 盘挂到 `/mnt/fat`，
内核侧逐项断言文件名/大小/内容（跨簇、空文件、长文件名、二级子目录），见 `tests/fatroot/README.md`。

### 存储夹具的独立复核（CSS-1）

CSS（[docs/storage.md](../docs/storage.md)）的验收要求「内核与宿主侧两套独立实现都认同一张盘」，
沿用 `mkfs` + mtools 的思路。宿主侧工具现状（本仓库开发环境**实测**）：

| 用途 | 可用 | 缺失 |
| ---- | ---- | ---- |
| FAT 文件内容 | `mtools`（`mdir`/`mtype`/`mformat`/`mlabel`） | `mkfs.fat` |
| **MBR 分区表** | `partx -s <img>`（实测能列出 NR/START/END/SECTORS） | — |
| **GPT 分区表** | （无）—— 实测 `partx`/`blkid` 对镜像文件**不枚举** GPT（合规主/备表头也一样），`blkid -p` 只报最外层 `PTTYPE`（GPT 镜像报 `PMBR`、DOS 镜像报 `dos`） | `sfdisk`、`fdisk`、`parted`、`sgdisk` |
| ext2 / ext4 | `mkfs.ext2`、`mkfs.ext4`、`dumpe2fs`、`debugfs`、`e2fsck` | — |
| exFAT | — | `mkfs.exfat` / `exfatprogs`（**因此 exFAT 目前没有独立复核手段**） |


夹具由 `mkfs.py` 生成（`make storage-images`），并在 `tests/test_storage.py`（`make test-storage-fixtures`，
不启 QEMU）里逐字节断言 GPT 主/备表头与 CRC、MBR 四主分区 + EBR 链结构。
所以 CSS-1 的 **MBR** 复核以 `partx -s <img>` 为准（第三方实现，与内核解析互相印证）；
**GPT** 在本机没有现成工具，先以「`mkfs.py` 生成 + 内核解析」两套自写实现互查（弱证据），
装好 `gdisk`/`parted` 后补一次独立复核（见 `docs/storage.md` 开放问题 4）；
FAT 仍以 `mtools` 为准；ext2/ext4 夹具用系统 `mkfs.ext*` 生成、用 `dumpe2fs` 复核。

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
