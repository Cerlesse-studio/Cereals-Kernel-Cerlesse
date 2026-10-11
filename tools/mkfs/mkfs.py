#!/usr/bin/env python3
"""Cerlesse mkfs（H-03）：生成 FAT12/16/32 镜像（v0.8 实现）。

## 为什么需要它

v0.8 的块设备层（`kernel/src/driver/block.rs`）只会按扇区读写；要验证「磁盘文件系统」
需要一块**内容已知**的盘。启动盘里只有 OVMF 需要的 EFI 引导文件，够验证「读真实 FAT」，
但不够断言文件语义。本工具生成「带已知文件集的 FAT 镜像」：`make fat-image` 调用它，
QEMU 里作为第二块 virtio-blk 盘挂到 `/mnt/fat`，`make test-fat` 逐项断言。

## 设计取舍

- **自己写 FAT 而不是调 `mformat`**：H-03 要的就是「我们能生成 FAT 镜像」这件事。
  镜像的正确性反过来由 **mtools 独立复核**（`mdir`/`mtype` 读同一张盘，见
  `tests/fatroot/README.md`）——两套独立实现互相印证，而不是自己验证自己。
- 目录项写完整的 **LFN（长文件名）+ 8.3 短名 + NT 大小写标志**，与 Windows/mtools 一致：
  内核侧因此能断言「带空格的长文件名路径也查得到」。
- 时间戳固定成 2026-01-01 00:00:00，让同一棵目录树生成**逐字节相同**的镜像。
- 只做「格式化 + 写入一棵目录树」，没有删除/追加/碎片整理：v0.8 的内核侧只读，
  测试也不需要改写既有镜像；真正要写盘时（v0.9 起）再补分配器。
- 写完**自己回读校验**（几何 → FAT 副本 → 簇链 → 目录项 → 文件长度），失败就非零退出，
  免得把坏镜像交给 QEMU 再回头怀疑内核。

## 用法

```bash
python3 tools/mkfs/mkfs.py --out build/fat.img --from tests/fatroot          # FAT32（64 MiB）
python3 tools/mkfs/mkfs.py --out build/fat16.img --fat 16 --from tests/fatroot
python3 tools/mkfs/mkfs.py --out build/fat12.img --fat 12 --from tests/fatroot
python3 tools/mkfs/mkfs.py --out build/empty.img --fat 32                    # 空盘
```

CSS-1（`docs/storage.md`）起还生成**分区表夹具**：GPT/MBR 表 + 每个分区的载荷，
以及故意损坏的变体（用来断言内核的 `degraded`/`invalid` 路径）：

```bash
# GPT：ESP 型 FAT16（带 tests/fatroot/data）+ ext4（宿主 mkfs.ext4 生成）+ 探测夹具
python3 tools/mkfs/mkfs.py --out build/storage/gpt.img --table gpt --from tests/fatroot/data \
    --part efi-esp:8M:fat16:efi --part root:8M:ext4:linux \
    --part probe-exfat:4M:exfat:ms-data --part probe-iso:1M:iso9660:unknown \
    --part blank:1M --payload root=build/storage/ext4-root.img

# MBR：FAT12 主分区 + 扩展分区里两个逻辑分区（EBR 链）
python3 tools/mkfs/mkfs.py --out build/storage/mbr.img --table mbr --from tests/fatroot/data \
    --part efi-esp:4M:fat12:fat12 --part ext:auto:extended \
    --logical probe-ntfs:2M:ntfs --logical blank:1M

# 损坏变体：主表头 CRC 坏（内核应改用备表头）/ 表项 CRC 坏 / 两张表头都坏 / EBR 成环
python3 tools/mkfs/mkfs.py --out build/storage/gpt-backup.img --table gpt \
    --part efi-esp:4M:fat16:efi --part probe-exfat:1M:exfat:ms-data --corrupt gpt-primary-crc
```

载荷有三种，**可独立复核程度不同，必须分清**（tools/README.md 的工具表）：

| `FS` | 生成方式 | 宿主侧独立复核 |
| ---- | ---- | ---- |
| `fat12/16/32` | 本工具自己写（复用 [`FatImage`]） | `mdir`/`mtype`（mtools，`-i img@@offset` 可指定分区偏移） |
| `ext2/ext4` | **必须** `--payload` 填入宿主 `mkfs.ext*` 的产物 | `dumpe2fs`/`e2fsck` |
| `exfat/ntfs/iso9660` | **探测夹具**：只写探针会读的那几个字段 | 无（本机无 `mkfs.exfat` 等；它们只用来断言「认出格式、无后端 ⇒ `health=unsupported`」） |

内核侧的 GPT/MBR 解析由 `tests/test_storage.py` 用**另一套解析器**（`zlib.crc32` + 直接读字节）
与 `partx -s`（MBR）互查；本机无 `sfdisk`/`parted`/`sgdisk`，详见 `docs/storage.md` 开放问题 4。
"""

from __future__ import annotations

import argparse
import hashlib
import math
import sys
import zlib
from pathlib import Path

SECTOR = 512
ATTR_DIRECTORY = 0x10
ATTR_ARCHIVE = 0x20
ATTR_LFN = 0x0F

# 8.3 短名允许的字符（大小写通过 NT 标志位表达，与 Windows 一致）
SHORT_OK = set("ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789$%'-_@~`!(){}^#&")
# 长文件名里不允许出现的字符
LFN_BAD = set('"*/:<>?\\|')

# 固定时间戳：2026-01-01 00:00:00（date = (year-1980)<<9 | month<<5 | day）
FIXED_DATE = ((2026 - 1980) << 9) | (1 << 5) | 1
FIXED_TIME = 0

# 默认卷序列号 / 卷标（超级软盘夹具沿用；分区夹具按分区名派生出各自的序列号）
FAT_SERIAL = 0x20261011
FAT_LABEL = b"NO NAME    "

# 每类 FAT 的「项宽」用整数分数表示（FAT12 是 3/2 字节），避免浮点误差
FAT_WIDTH = {12: (3, 2), 16: (2, 1), 32: (4, 1)}
EOC = {12: 0x0FFF, 16: 0xFFFF, 32: 0x0FFF_FFFF}
RESERVED_MEDIA = {12: 0x0FF8, 16: 0xFFF8, 32: 0x0FFF_FFF8}
FS_TYPE_STRING = {12: b"FAT12   ", 16: b"FAT16   ", 32: b"FAT32   "}


def parse_size(text: str) -> int:
    """`64M` / `4M` / `8192` → 扇区数。"""
    text = text.strip().upper()
    mult = 1
    for suffix, factor in (("K", 1024), ("M", 1024 * 1024), ("G", 1024 * 1024 * 1024)):
        if text.endswith(suffix):
            text, mult = text[: -len(suffix)], factor
            break
    total_bytes = int(text) * mult
    if total_bytes % SECTOR:
        raise SystemExit(f"size {total_bytes} is not a multiple of {SECTOR}")
    return total_bytes // SECTOR


# ---- 目录树模型 ----


class DirNode:
    """目录（名字 + 子节点）。"""

    def __init__(self, name: str) -> None:
        self.name = name
        self.children: list[DirNode | FileNode] = []

    is_dir = True

    @property
    def data(self) -> bytes:
        return b""


class FileNode(DirNode):
    """普通文件（名字 + 内容）。"""

    def __init__(self, name: str, data: bytes) -> None:
        super().__init__(name)
        self._data = data

    is_dir = False

    @property
    def data(self) -> bytes:
        return self._data


def load_tree(root: Path) -> DirNode:
    """把宿主目录读成目录树（目录在前、名字排序 → 生成结果稳定）。"""
    node = DirNode(root.name)

    def visit(dst: DirNode, src: Path) -> None:
        for item in sorted(src.iterdir(), key=lambda p: (p.is_file(), p.name)):
            if item.is_dir():
                child = DirNode(item.name)
                dst.children.append(child)
                visit(child, item)
            else:
                dst.children.append(FileNode(item.name, item.read_bytes()))

    visit(node, root)
    return node


# ---- 镜像构造 ----


class FatImage:
    """把一棵目录树写成一个 FAT 卷（**超级软盘**布局：引导扇区在 0 号扇区，无分区表）。"""

    def __init__(self, fat_type: int, total_sectors: int, spc: int,
                 reserved: int, root_entries: int,
                 serial: int = FAT_SERIAL, label: bytes = FAT_LABEL) -> None:
        self.fat_type = fat_type
        self.total_sectors = total_sectors
        self.spc = spc
        self.reserved = reserved
        self.root_entries = root_entries
        # 卷序列号/卷标：CSS-1 起分区夹具需要**每个分区各不相同**的序列号（否则
        # 内核的 `vol_id` 会撞在一起并被判成克隆卷），因此允许调用方给定。
        if len(label) > 11:
            raise SystemExit(f"FAT label must be at most 11 bytes, got {len(label)}")
        self.serial = serial & 0xFFFF_FFFF
        self.label = label.ljust(11, b" ")
        self.num_fats = 2
        self.cluster_bytes = spc * SECTOR

        self.fat_size = self.solve_fat_size()
        self.root_dir_sectors = math.ceil(root_entries * 32 / SECTOR)
        self.data_start = reserved + self.num_fats * self.fat_size + self.root_dir_sectors
        self.clusters = (total_sectors - self.data_start) // spc
        if self.clusters < 1:
            raise SystemExit("image too small for the requested geometry")

        self.image = bytearray(total_sectors * SECTOR)
        self.fat: dict[int, int] = {0: RESERVED_MEDIA[fat_type], 1: EOC[fat_type]}
        self.next_cluster = 2
        # FAT32 的根目录是一个普通簇链，规范要求它从簇 2 开始（这也是 boot sector 的默认值）
        self.root_cluster = 2 if fat_type == 32 else 0
        self.files = 0
        self.dirs = 0

    # -- 几何 --

    def fat_entries_bytes(self, clusters: int) -> int:
        num, den = FAT_WIDTH[self.fat_type]
        return -(-(clusters + 2) * num // den)  # 整数向上取整：(n*num + den-1)//den

    def solve_fat_size(self) -> int:
        """簇数依赖 FAT 扇区数、FAT 扇区数又依赖簇数：迭代到稳定。"""
        size = 1
        for _ in range(64):
            clusters = (self.total_sectors - self.reserved - self.num_fats * size) // self.spc
            needed = math.ceil(self.fat_entries_bytes(clusters) / SECTOR)
            if needed <= size:
                return size
            size = needed
        raise SystemExit("cannot solve FAT geometry")

    def detected_type(self) -> int:
        """按簇数判定类型（与内核的自检测同一条规则：<4085 FAT12、<65525 FAT16、否则 FAT32）。"""
        if self.clusters < 4085:
            return 12
        if self.clusters < 65525:
            return 16
        return 32

    # -- 簇分配 --

    def alloc(self, count: int) -> list[int]:
        if self.next_cluster + count > self.clusters + 2:
            raise SystemExit("image full: not enough clusters for the requested tree")
        chain = list(range(self.next_cluster, self.next_cluster + count))
        self.next_cluster += count
        return chain

    def link(self, chain: list[int]) -> None:
        for i, cluster in enumerate(chain):
            self.fat[cluster] = chain[i + 1] if i + 1 < len(chain) else EOC[self.fat_type]

    def cluster_offset(self, cluster: int) -> int:
        if cluster < 2:
            raise SystemExit(f"cluster {cluster} is reserved")
        return (self.data_start + (cluster - 2) * self.spc) * SECTOR

    def write_clusters(self, chain: list[int], data: bytes) -> None:
        """把 `data` 依次写进簇链（尾部补 0），并登记簇链。"""
        if len(data) > len(chain) * self.cluster_bytes:
            raise SystemExit("internal: data does not fit the allocated chain")
        self.link(chain)
        padded = data + b"\x00" * (len(chain) * self.cluster_bytes - len(data))
        for i, cluster in enumerate(chain):
            start = self.cluster_offset(cluster)
            self.image[start : start + self.cluster_bytes] = padded[
                i * self.cluster_bytes : (i + 1) * self.cluster_bytes
            ]

    # -- 短名 / LFN --

    def make_short(self, name: str, taken: set[bytes]) -> tuple[bytes, int, bool]:
        """返回（11 字节短名, NT 大小写标志, 是否需要 LFN 项）。"""
        stem, dot, ext = name.rpartition(".")
        if not dot:
            stem, ext = name, ""
        stem_up, ext_up = stem.upper(), ext.upper()
        fits = (
            len(stem_up) <= 8
            and len(ext_up) <= 3
            and all(c in SHORT_OK for c in stem_up)
            and all(c in SHORT_OK for c in ext_up)
            and not any(c in LFN_BAD for c in name)
        )
        if fits:
            short = (stem_up.ljust(8) + ext_up.ljust(3)).encode("ascii")
            flags = 0
            if stem != stem_up:
                flags |= 0x08  # 基名小写
            if ext and ext != ext_up:
                flags |= 0x10  # 扩展名小写
            return short, flags, False

        # 需要 LFN：造一个 `XXXXXX~N.EXT` 的短名（Windows 的做法）
        cleaned = "".join(c for c in name.upper() if c in SHORT_OK)
        base = (cleaned[:6] or "FILE").ljust(6)
        ext3 = "".join(c for c in ext_up if c in SHORT_OK)[:3]
        for n in range(1, 1000):
            raw = (f"{base}~{n}".ljust(8)[:8] + ext3.ljust(3)).encode("ascii")
            if raw not in taken:
                return raw, 0, True
        raise SystemExit(f"cannot build a unique short name for {name!r}")

    @staticmethod
    def lfn_entry_count(name: str) -> int:
        units = (len(name.encode("utf-16-le")) + 2) // 2
        return max(1, math.ceil(units / 13))

    @staticmethod
    def lfn_checksum(short: bytes) -> int:
        total = 0
        for byte in short[:11]:
            total = (((total & 1) << 7) + (total >> 1) + byte) & 0xFF
        return total

    def lfn_entries(self, name: str, short: bytes) -> list[bytes]:
        """长文件名的 0x0F 目录项（**逆序**排在短名项之前，与规范一致）。"""
        encoded = name.encode("utf-16-le") + b"\x00\x00"
        units = [encoded[i : i + 2] for i in range(0, len(encoded), 2)]
        chunks = [units[i : i + 13] for i in range(0, len(units), 13)]
        checksum = self.lfn_checksum(short)
        entries: list[bytes] = []
        for index, chunk in enumerate(chunks):
            seq = index + 1
            if index == len(chunks) - 1:
                seq |= 0x40  # 最后一项（名字的结尾）
            entry = bytearray(32)
            entry[0] = seq
            entry[11] = ATTR_LFN
            entry[13] = checksum
            padded = list(chunk) + [b"\xff\xff"] * (13 - len(chunk))
            for slot, unit in zip((1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30), padded):
                entry[slot : slot + 2] = unit
            entries.append(bytes(entry))
        entries.reverse()  # 序号大的在前
        return entries

    def dir_entry(self, short: bytes, flags: int, first_cluster: int, size: int, is_dir: bool) -> bytes:
        entry = bytearray(32)
        entry[0:11] = short
        entry[11] = ATTR_DIRECTORY if is_dir else ATTR_ARCHIVE
        entry[12] = flags
        entry[14:16] = FIXED_TIME.to_bytes(2, "little")
        entry[16:18] = FIXED_DATE.to_bytes(2, "little")
        entry[18:20] = FIXED_DATE.to_bytes(2, "little")
        entry[20:22] = ((first_cluster >> 16) & 0xFFFF).to_bytes(2, "little")
        entry[22:24] = FIXED_TIME.to_bytes(2, "little")
        entry[24:26] = FIXED_DATE.to_bytes(2, "little")
        entry[26:28] = (first_cluster & 0xFFFF).to_bytes(2, "little")
        entry[28:32] = size.to_bytes(4, "little")
        return bytes(entry)

    # -- 目录写入 --

    def plan_children(self, node: DirNode) -> list[tuple[DirNode, bytes, int, bool]]:
        """为目录里的每个子项分配短名（同目录内唯一）并算出是否需要 LFN。"""
        taken: set[bytes] = set()
        plan: list[tuple[DirNode, bytes, int, bool]] = []
        for child in node.children:
            short, flags, needs_lfn = self.make_short(child.name, taken)
            taken.add(short)
            plan.append((child, short, flags, needs_lfn))
        return plan

    def dir_entry_count(self, node: DirNode, is_root: bool) -> int:
        total = 0 if is_root else 2  # `.` 与 `..`
        for child, _short, _flags, needs_lfn in self.plan_children(node):
            total += 1 + (self.lfn_entry_count(child.name) if needs_lfn else 0)
        return total

    def dir_cluster_count(self, node: DirNode, is_root: bool) -> int:
        return max(1, math.ceil(self.dir_entry_count(node, is_root) * 32 / self.cluster_bytes))

    def build_dir_body(self, node: DirNode, is_root: bool, self_cluster: int,
                       parent_first: int) -> tuple[bytes, list[tuple[DirNode, list[int]]]]:
        """构造目录内容（含子项条目），并分配子目录/文件的簇。

        `self_cluster` 是当前目录自己的首簇（写 `.` 用），`parent_first` 是父目录首簇
        （写 `..` 用）；两者都在调用前就已确定，因此不需要在递归间共享可变状态。
        """
        body = bytearray()
        if not is_root:
            body += self.dir_entry(b".          ", 0, self_cluster, 0, True)
            body += self.dir_entry(b"..         ", 0, parent_first, 0, True)
        subdirs: list[tuple[DirNode, list[int]]] = []
        for child, short, flags, needs_lfn in self.plan_children(node):
            if needs_lfn:
                entries = self.lfn_entries(child.name, short)
                assert len(entries) == self.lfn_entry_count(child.name)
                body += b"".join(entries)
            if child.is_dir:
                chain = self.alloc(self.dir_cluster_count(child, is_root=False))
                body += self.dir_entry(short, flags, chain[0], 0, True)
                subdirs.append((child, chain))
            else:
                data = child.data
                first = 0
                if data:
                    chain = self.alloc(max(1, math.ceil(len(data) / self.cluster_bytes)))
                    self.write_clusters(chain, data)
                    first = chain[0]
                body += self.dir_entry(short, flags, first, len(data), False)
                self.files += 1
        return bytes(body), subdirs

    def write_subdir(self, node: DirNode, chain: list[int], parent_first: int) -> None:
        """递归写子目录：`.` 指向自己、`..` 指向父目录（都已在分配阶段确定）。"""
        body, subdirs = self.build_dir_body(
            node, is_root=False, self_cluster=chain[0], parent_first=parent_first
        )
        if len(body) > len(chain) * self.cluster_bytes:
            raise SystemExit("internal: directory body exceeds its cluster chain")
        self.write_clusters(chain, body)
        self.dirs += 1
        for child, child_chain in subdirs:
            self.write_subdir(child, child_chain, chain[0])

    def build(self, tree: DirNode) -> None:
        if self.root_entries:
            # FAT12/16：根目录是固定区域（不进簇链），`..` 约定为簇 0
            body, subdirs = self.build_dir_body(
                tree, is_root=True, self_cluster=0, parent_first=0
            )
            if len(body) > self.root_dir_sectors * SECTOR:
                raise SystemExit("root directory too small for this tree (raise root_entries)")
            start = (self.reserved + self.num_fats * self.fat_size) * SECTOR
            self.image[start : start + len(body)] = body
            self.dirs += 1
            for child, child_chain in subdirs:
                self.write_subdir(child, child_chain, 0)
            return

        # FAT32：根目录是簇链，从簇 2 开始 —— **先**分配根目录簇（第一个分配就是簇 2），
        # 再构造内容（子目录/文件的簇在它后面），否则根目录会被排到别处去。
        need = max(1, math.ceil(self.dir_entry_count(tree, is_root=True) * 32 / self.cluster_bytes))
        root_chain = self.alloc(need)
        if root_chain[0] != 2:
            raise SystemExit(f"internal: FAT32 root cluster must be 2, got {root_chain[0]}")
        self.root_cluster = root_chain[0]
        body, subdirs = self.build_dir_body(
            tree, is_root=True, self_cluster=root_chain[0], parent_first=root_chain[0]
        )
        if len(body) > len(root_chain) * self.cluster_bytes:
            raise SystemExit("internal: root directory body exceeds its cluster chain")
        self.write_clusters(root_chain, body)
        self.dirs += 1
        for child, child_chain in subdirs:
            self.write_subdir(child, child_chain, root_chain[0])

    # -- 引导扇区 / FSInfo / FAT --

    def boot_sector(self) -> bytes:
        boot = bytearray(SECTOR)
        boot[0:3] = b"\xeb\x58\x90"
        boot[3:11] = b"CERLESSE"
        boot[11:13] = SECTOR.to_bytes(2, "little")
        boot[13] = self.spc
        boot[14:16] = self.reserved.to_bytes(2, "little")
        boot[16] = self.num_fats
        boot[17:19] = self.root_entries.to_bytes(2, "little")
        total16 = self.total_sectors if self.total_sectors < 0x10000 else 0
        boot[19:21] = total16.to_bytes(2, "little")
        boot[21] = 0xF8  # 介质描述符：固定盘
        boot[22:24] = (self.fat_size if self.fat_type != 32 else 0).to_bytes(2, "little")
        boot[24:26] = (32).to_bytes(2, "little")  # 每磁道扇区（几何值，仅供参考）
        boot[26:28] = (64).to_bytes(2, "little")  # 磁头数
        boot[28:32] = (0).to_bytes(4, "little")   # 隐藏扇区
        boot[32:36] = (0 if total16 else self.total_sectors).to_bytes(4, "little")
        if self.fat_type == 32:
            boot[36:40] = self.fat_size.to_bytes(4, "little")
            boot[44:48] = self.root_cluster.to_bytes(4, "little")
            boot[48:50] = (1).to_bytes(2, "little")  # FSInfo 扇区
            boot[50:52] = (6).to_bytes(2, "little")  # 引导扇区备份
        boot[38 if self.fat_type != 32 else 66] = 0x29  # 扩展引导签名
        id_offset = 39 if self.fat_type != 32 else 67
        boot[id_offset : id_offset + 4] = self.serial.to_bytes(4, "little")
        boot[id_offset + 4 : id_offset + 15] = self.label
        type_offset = 82 if self.fat_type == 32 else 54
        boot[type_offset : type_offset + 8] = FS_TYPE_STRING[self.fat_type]
        boot[510:512] = b"\x55\xaa"
        return bytes(boot)

    def fsinfo_sector(self) -> bytes:
        info = bytearray(SECTOR)
        info[0:4] = (0x41615252).to_bytes(4, "little")
        info[484:488] = (0x61417272).to_bytes(4, "little")
        info[488:492] = (self.clusters + 2 - self.next_cluster).to_bytes(4, "little")  # 空闲簇数
        info[492:496] = self.next_cluster.to_bytes(4, "little")  # 下一个可用簇
        info[508:512] = (0xAA550000).to_bytes(4, "little")
        return bytes(info)

    def fat_bytes(self) -> bytes:
        raw = bytearray(self.fat_size * SECTOR)
        for cluster, value in self.fat.items():
            if self.fat_type == 12:
                offset = cluster * 3 // 2
                if cluster % 2 == 0:
                    raw[offset] = value & 0xFF
                    raw[offset + 1] = (raw[offset + 1] & 0xF0) | ((value >> 8) & 0x0F)
                else:
                    raw[offset] = (raw[offset] & 0x0F) | ((value << 4) & 0xF0)
                    raw[offset + 1] = (value >> 4) & 0xFF
            elif self.fat_type == 16:
                raw[cluster * 2 : cluster * 2 + 2] = value.to_bytes(2, "little")
            else:
                raw[cluster * 4 : cluster * 4 + 4] = (value & 0x0FFF_FFFF).to_bytes(4, "little")
        return bytes(raw)

    def render(self) -> bytes:
        if self.detected_type() != self.fat_type:
            raise SystemExit(
                f"geometry would be detected as FAT{self.detected_type()} "
                f"({self.clusters} clusters) but --fat {self.fat_type} was requested"
            )
        fat = self.fat_bytes()
        for copy in range(self.num_fats):
            start = (self.reserved + copy * self.fat_size) * SECTOR
            self.image[start : start + len(fat)] = fat
        boot = self.boot_sector()
        self.image[0:SECTOR] = boot
        if self.fat_type == 32:
            self.image[SECTOR : 2 * SECTOR] = self.fsinfo_sector()
            self.image[6 * SECTOR : 7 * SECTOR] = boot
            self.image[7 * SECTOR : 8 * SECTOR] = self.fsinfo_sector()
        return bytes(self.image)

    # -- 回读校验（另一条代码路径） --

    def verify(self, image: bytes) -> None:
        bps = int.from_bytes(image[11:13], "little")
        spc = image[13]
        reserved = int.from_bytes(image[14:16], "little")
        nfats = image[16]
        root_entries = int.from_bytes(image[17:19], "little")
        fat_size16 = int.from_bytes(image[22:24], "little")
        fat_size32 = int.from_bytes(image[36:40], "little")
        fat_size = fat_size16 or fat_size32
        if (bps, spc, reserved, nfats, root_entries, fat_size) != (
            SECTOR, self.spc, self.reserved, self.num_fats, self.root_entries, self.fat_size,
        ):
            raise SystemExit("verify: boot sector does not match the requested geometry")
        if image[510:512] != b"\x55\xaa":
            raise SystemExit("verify: missing 0xAA55 boot signature")
        type_offset = 82 if self.fat_type == 32 else 54
        if image[type_offset : type_offset + 8] != FS_TYPE_STRING[self.fat_type]:
            raise SystemExit("verify: wrong FAT type string in the boot sector")
        if self.fat_type == 32:
            root_cluster = int.from_bytes(image[44:48], "little")
            if root_cluster != 2:
                raise SystemExit(f"verify: FAT32 RootClus must be 2, got {root_cluster}")
            if int.from_bytes(image[2 * SECTOR - 4 : 2 * SECTOR], "little") != 0xAA550000:
                raise SystemExit("verify: FSInfo signature missing")
        fat = self.fat_bytes()
        for copy in range(nfats):
            start = (reserved + copy * fat_size) * bps
            if image[start : start + len(fat)] != fat:
                raise SystemExit(f"verify: FAT copy {copy} differs from what was written")
        for cluster, value in self.fat.items():
            if cluster < 2 or value == EOC[self.fat_type]:
                continue
            if not 2 <= value <= self.clusters + 1:
                raise SystemExit(f"verify: cluster {cluster} points outside the data area ({value})")


# ---- 分区表镜像（CSS-1：GPT / MBR + 分区夹具） ----
#
# 为什么要自己写分区表：`docs/storage.md` 第 9 节要求内核的 GPT/MBR 解析器有一张
# **内容已知**的盘可断言，而本机没有 `sfdisk`/`parted`/`sgdisk`（见 tools/README 的
# 工具表）。所以这里沿用 mkfs 的既有打法：自己生成镜像，正确性由第三方实现互查——
# MBR 用 `partx -s`、GPT 用 `tests/test_storage.py` 里**另一套解析器**（zlib CRC32）。
#
# 三种分区载荷：
#   - `fat12|fat16|fat32`：真的 FAT（复用 [`FatImage`]，仍可被 mtools 复核）；
#   - `ext2|ext4`：**必须**用 `--payload 名字=路径` 填入宿主 `mkfs.ext4` 生成的镜像
#     （不自己伪造 ext 超级块：假的超级块只能证明「探针认魔数」，不能证明镜像真的能用）；
#   - `exfat|ntfs|iso9660`：**探测夹具**——只写探针会读的那几个字段，用来断言「认出来了、
#     但没有后端」（`health=unsupported`）。它们不是可挂载的镜像，命名与文档都会说明。

SECTOR_ALIGN = 2048  # 1 MiB 对齐（分区起点与常见工具一致，partx 输出也好读）
GPT_ENTRIES = 128
GPT_ENTRY_SIZE = 128
GPT_HEADER_SIZE = 92
GPT_RESERVED_LBA = 34  # 保护性 MBR(1) + 主表头(1) + 表项(32) → 第一个可用 LBA
GPT_BACKUP_SECTORS = 33  # 备表项（32 扇区）+ 备表头（1 扇区）

# GPT 类型 GUID（文本形式；写盘时转成 EFI_GUID 的混合字节序）
GPT_TYPE_ALIASES = {
    "efi": "C12A7328-F81F-11D2-BA4B-00A0C93EC93B",
    "linux": "0FC63DAF-8483-4772-8E79-3D69D8477DE4",
    "ms-data": "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7",
    "bios-boot": "21686148-6449-6E6F-744E-656564454649",
    "unknown": "0BAD0BAD-0BAD-0BAD-0BAD-0BAD0BAD0BAD",
}
# MBR 分区类型字节（可由 `--part …:0xNN` 直接覆盖）
MBR_TYPE_ALIASES = {
    "fat12": 0x01, "fat16": 0x06, "fat32": 0x0B, "exfat": 0x07, "ntfs": 0x07,
    "ext2": 0x83, "ext4": 0x83, "linux": 0x83, "swap": 0x82, "efi": 0xEF,
}
# 只写「探针读得到的那几个字段」的夹具文件系统
PROBE_ONLY_FS = ("exfat", "ntfs", "iso9660")
FAT_FS = ("fat12", "fat16", "fat32")
KNOWN_FS = FAT_FS + PROBE_ONLY_FS + ("ext2", "ext4", "none")
# 可注入的损坏（每个镜像一种，用来断言 degraded/invalid 路径）：
#   gpt-primary-crc  主表头 CRC 坏（备表头仍然好）→ 内核应改用备表头并标 degraded；
#   gpt-entries-crc  表项 CRC 坏 → 仍然解析，但整盘 degraded；
#   gpt-both-headers  两张表头都坏 → 内核必须判 invalid（不许当 MBR 解析保护表）；
#   ebr-broken       扩展链的最后一个“下一层”指针指到盘外 → 读不到就停下来标 degraded。
CORRUPTIONS = ("gpt-primary-crc", "gpt-entries-crc", "gpt-both-headers", "ebr-broken")


def guid_bytes(text: str) -> bytes:
    """GUID 文本 → GPT 表项里的 16 字节（前三段小端，与 UEFI `EFI_GUID` 一致）。"""
    a, b, c, d, e = text.split("-")
    return (
        int(a, 16).to_bytes(4, "little")
        + int(b, 16).to_bytes(2, "little")
        + int(c, 16).to_bytes(2, "little")
        + int(d, 16).to_bytes(2, "big")
        + int(e, 16).to_bytes(6, "big")
    )


def guid_from_name(name: str) -> bytes:
    """分区名 → 确定性 GUID（夹具不需要随机；同一名字两次生成逐字节相同）。"""
    digest = hashlib.sha256(name.encode()).digest()
    return guid_bytes(
        f"{int.from_bytes(digest[0:4], 'big'):08x}-{int.from_bytes(digest[4:6], 'big'):04x}-"
        f"{int.from_bytes(digest[6:8], 'big'):04x}-{int.from_bytes(digest[8:10], 'big'):04x}-"
        f"{int.from_bytes(digest[10:16], 'big'):012x}"
    )


def serial_from_name(name: str) -> int:
    """分区名 → 确定性 FAT 卷序列号（每个分区各不相同，否则内核会把它们判成克隆卷）。"""
    return (zlib.crc32(name.encode()) & 0xFFFF) | 0x2026_0000


def align_up(value: int, align: int = SECTOR_ALIGN) -> int:
    return -(-value // align) * align


class Part:
    """一个待登记的分区（主分区或逻辑分区）。"""

    def __init__(self, name: str, sectors: int | None, fs: str, type_field: str | None,
                 logical: bool = False, serial: int | None = None) -> None:
        self.name = name
        self.sectors = sectors if sectors is not None else 0  # `auto`（扩展容器）时为 0
        self.fs = fs
        self.type_field = type_field
        self.logical = logical
        # 显式卷序列号（默认由分区名派生）：只有“故意制造重名卷”的夹具才需要它
        self.serial = serial
        self.start = 0        # 绝对起始 LBA（回填）
        self.ebr_lba = 0      # 逻辑分区：承载它的 EBR 所在 LBA
        self.payload = b""    # 分区内容

    @property
    def auto_size(self) -> bool:
        return self.type_field == "extended" and self.sectors == 0

    def summary(self) -> str:
        return f"{self.name}({self.fs}) start={self.start} sectors={self.sectors}"


def parse_part_spec(text: str, logical: bool) -> Part:
    """`名字:大小[:文件系统[:类型[:serial=NN]]]` → [`Part`]；`大小` 可为 `auto`（仅扩展容器）。

    可选的 `key=value` 字段跟在类型之后。目前只有 `serial=`（十六进制或十进制）：
    用来**故意**造出两个序列号相同的卷（克隆盘夹具，断言卷表会报冲突而不是静默合倂）。
    """
    fields = text.split(":")
    if len(fields) < 2:
        raise SystemExit(f"bad --{'logical' if logical else 'part'} spec: {text!r} (want name:size[:fs[:type]])")
    name = fields[0]
    size_field = fields[1].strip()
    fs = (fields[2].strip().lower() if len(fields) > 2 else "none") or "none"
    type_field = (fields[3].strip() if len(fields) > 3 else None) or None
    if fs == "extended":  # `ext:auto:extended` 的写法：扩展容器本身不持有文件系统
        fs, type_field = "none", "extended"
    if fs not in KNOWN_FS:
        raise SystemExit(f"unknown fs {fs!r} in {text!r} (known: {', '.join(KNOWN_FS)})")
    if fs in PROBE_ONLY_FS:
        type_field = type_field or fs
    if size_field.lower() == "auto":
        sectors = 0
    else:
        sectors = parse_size(size_field)
    if sectors == 0 and not (type_field == "extended"):
        raise SystemExit(f"partition {name!r} needs a size (or type=extended with size=auto)")
    serial = None
    for option in fields[4:]:
        key, sep, value = option.strip().partition("=")
        if not sep:
            raise SystemExit(f"bad partition option {option!r} in {text!r} (want key=value)")
        if key == "serial":
            serial = int(value, 0) & 0xFFFF_FFFF
        else:
            raise SystemExit(f"unknown partition option {key!r} in {text!r}")
    return Part(name, sectors, fs, type_field, logical, serial)


# 需要写入 `tests/fatroot/data` 目录树的分区名（其它 FAT 分区留空）
FAT_WITH_TREE = ("efi-esp",)


def make_payload(part: Part, tree: DirNode, payloads: dict[str, Path]) -> bytes:
    """按 `fs` 生成分区内容（大小必须与分区边界逐字节吻合）。"""
    size = part.sectors * SECTOR
    if part.fs in FAT_FS:
        fat = int(part.fs[3:])
        spc, reserved, root_entries = choose_geometry(fat)
        image = FatImage(fat, part.sectors, spc, reserved, root_entries,
                         serial=part.serial if part.serial is not None else serial_from_name(part.name),
                         label=part.name.upper().encode()[:11])
        image.build(tree if part.name in FAT_WITH_TREE else DirNode("/"))
        raw = image.render()
        image.verify(raw)
        return raw
    if part.fs in ("ext2", "ext4"):
        source = payloads.get(part.name)
        if source is None:
            raise SystemExit(
                f"partition {part.name!r} is {part.fs}: pass --payload {part.name}="
                "<host mkfs.ext4 image> (we do not fake ext superblocks)"
            )
        raw = Path(source).read_bytes()
        if len(raw) != size:
            raise SystemExit(
                f"payload for {part.name!r} is {len(raw)} bytes but the partition is {size} "
                f"({part.sectors} sectors)"
            )
        return raw
    return probe_payload(part, size)


def probe_payload(part: Part, size: int) -> bytes:
    """探测夹具：只写**探针会读的字段**（exfat/ntfs/iso9660 本机没有格式化工具）。"""
    raw = bytearray(size)
    serial = part.serial if part.serial is not None else serial_from_name(part.name)
    if part.fs == "exfat":
        raw[3:11] = b"EXFAT   "
        raw[0x64:0x68] = serial.to_bytes(4, "little")
        raw[510:512] = b"\x55\xaa"
    elif part.fs == "ntfs":
        raw[3:11] = b"NTFS    "
        raw[0x48:0x4C] = serial.to_bytes(4, "little")
        raw[510:512] = b"\x55\xaa"
    elif part.fs == "iso9660":
        pvd = 16 * SECTOR
        raw[pvd + 1 : pvd + 6] = b"CD001"
        label = part.name.upper().encode()[:32]
        raw[pvd + 40 : pvd + 72] = label.ljust(32, b" ")
    elif part.fs != "none":
        raise SystemExit(f"no payload generator for fs {part.fs!r}")
    return bytes(raw)


def layout(primaries: list[Part], logicals: list[Part], gpt: bool) -> int:
    """分配每个分区的绝对位置，返回整盘扇区数。"""
    containers = [p for p in primaries if p.type_field == "extended"]
    if len(containers) > 1:
        raise SystemExit("only one extended container is supported")
    for lp in logicals:
        if not containers:
            raise SystemExit("--logical needs a --part with type=extended")

    cursor = SECTOR_ALIGN
    for part in primaries:
        part.start = cursor
        if part.type_field == "extended":
            inner = cursor
            for lp in logicals:
                lp.ebr_lba = inner
                lp.start = inner + 1
                inner = lp.start + lp.sectors
            part.sectors = inner - cursor
            cursor = align_up(inner)
        else:
            cursor = align_up(cursor + part.sectors)
    tail = GPT_BACKUP_SECTORS if gpt else 1
    return align_up(cursor + tail)


def mbr_type_byte(part: Part) -> int:
    """MBR 表项的类型字节：显式 `0xNN`/十进制优先，否则按文件系统推断。"""
    field = (part.type_field or "").lower()
    if field.startswith("0x"):
        return int(field, 16) & 0xFF
    if field.isdigit():
        return int(field) & 0xFF
    if field in MBR_TYPE_ALIASES:
        return MBR_TYPE_ALIASES[field]
    if part.fs in MBR_TYPE_ALIASES:
        return MBR_TYPE_ALIASES[part.fs]
    return 0x83


def chs(lba: int) -> bytes:
    """LBA → 3 字节 CHS（按 255 头 / 63 扇区的常规几何；内核只读 LBA，这里是为了工具友好）。"""
    heads, sectors = 255, 63
    cylinder, rest = divmod(lba, heads * sectors)
    head, sector = divmod(rest, sectors)
    if cylinder > 1023:
        return b"\xfe\xff\xff"
    return bytes([head & 0xFF, ((sector + 1) & 0x3F) | ((cylinder >> 2) & 0xC0), cylinder & 0xFF])


def mbr_entry(ptype: int, start: int, sectors: int) -> bytes:
    entry = bytearray(16)
    entry[1:4] = chs(start)
    entry[4] = ptype
    entry[5:8] = chs(start + max(sectors - 1, 0))
    entry[8:12] = start.to_bytes(4, "little")
    entry[12:16] = sectors.to_bytes(4, "little")
    return bytes(entry)


def render_mbr(primaries: list[Part], logicals: list[Part], disk_sectors: int,
               corrupt: str | None) -> bytes:
    """写 MBR + 扩展链；返回整盘字节。"""
    if len(primaries) > 4:
        raise SystemExit(f"MBR supports 4 primary entries, got {len(primaries)}")
    if corrupt == "ebr-broken" and not logicals:
        raise SystemExit("ebr-broken needs at least one logical partition")
    raw = bytearray(disk_sectors * SECTOR)
    boot = bytearray(SECTOR)
    for index, part in enumerate(primaries[:4]):
        ptype = 0x0F if part.type_field == "extended" else mbr_type_byte(part)
        boot[0x1BE + index * 16 : 0x1BE + (index + 1) * 16] = mbr_entry(ptype, part.start, part.sectors)
    boot[510:512] = b"\x55\xaa"
    raw[0:SECTOR] = boot

    # 扩展链：EBR 在容器里，入口 0 是本层逻辑分区（相对本 EBR），入口 1 指向下一层 EBR
    for index, lp in enumerate(logicals):
        ebr = bytearray(SECTOR)
        ebr[0x1BE:0x1BE + 16] = mbr_entry(mbr_type_byte(lp), lp.start - lp.ebr_lba, lp.sectors)
        if index + 1 == len(logicals):
            if corrupt == "ebr-broken":
                # 断链：最后一层的“下一层”指向盘外（内核必须读失败就停，而不是继续算）
                ebr[0x1BE + 16 : 0x1BE + 32] = mbr_entry(0x0F, disk_sectors + 128, lp.sectors)
        else:
            nxt = logicals[index + 1].ebr_lba - lp.ebr_lba
            ebr[0x1BE + 16 : 0x1BE + 32] = mbr_entry(0x0F, nxt, logicals[index + 1].sectors)
        ebr[510:512] = b"\x55\xaa"
        offset = lp.ebr_lba * SECTOR
        raw[offset:offset + SECTOR] = ebr
    return bytes(raw)


def render_gpt(primaries: list[Part], disk_sectors: int, corrupt: str | None) -> bytes:
    """写保护性 MBR + 主/备 GPT；返回整盘字节。"""
    raw = bytearray(disk_sectors * SECTOR)
    data = [p for p in primaries if p.type_field != "extended"]
    if len(data) > GPT_ENTRIES:
        raise SystemExit("too many partitions for one GPT")

    protect = bytearray(SECTOR)
    protect[0x1BE:0x1CE] = mbr_entry(0xEE, 1, min(disk_sectors - 1, 0xFFFF_FFFF))
    protect[510:512] = b"\x55\xaa"
    raw[0:SECTOR] = protect

    entries = bytearray(GPT_ENTRIES * GPT_ENTRY_SIZE)
    for index, part in enumerate(data):
        entry = bytearray(GPT_ENTRY_SIZE)
        alias = (part.type_field or "unknown").lower()
        guid_text = GPT_TYPE_ALIASES.get(alias, GPT_TYPE_ALIASES["unknown"])
        entry[0:16] = guid_bytes(guid_text)
        entry[16:32] = guid_from_name(part.name)
        entry[0x20:0x28] = part.start.to_bytes(8, "little")
        entry[0x28:0x30] = (part.start + part.sectors - 1).to_bytes(8, "little")
        name = part.name.encode("utf-16-le")[:72]
        entry[0x38:0x38 + len(name)] = name
        entries[index * GPT_ENTRY_SIZE : (index + 1) * GPT_ENTRY_SIZE] = entry

    # 表项 CRC 先按**完好**的表项算出来写进表头，再去损坏表项 —— 这样 `gpt-entries-crc`
    # 夹具真的“表头声称的 CRC 对不上表项”（先损坏再算 CRC 会自己骗自己）。
    entries_crc = zlib.crc32(bytes(entries)) & 0xFFFF_FFFF
    if corrupt == "gpt-entries-crc":
        # 只动**未使用表项**的名字字节：表项 CRC 失败但分区清单不变
        entries[(GPT_ENTRIES - 1) * GPT_ENTRY_SIZE + 0x38] ^= 0xFF

    entries_lba = 2
    # 备表项在最后 33 个扇区里的前 32 个，最后 1 个扇区才是备表头
    backup_entries_lba = disk_sectors - GPT_BACKUP_SECTORS
    raw[entries_lba * SECTOR : entries_lba * SECTOR + len(entries)] = entries
    raw[backup_entries_lba * SECTOR : backup_entries_lba * SECTOR + len(entries)] = entries

    disk_guid = guid_from_name("cerlesse-storage-fixture")

    def header(current: int, backup: int, ent_lba: int, damaged: bool) -> bytes:
        block = bytearray(SECTOR)
        block[0:8] = b"EFI PART"
        block[8:12] = (0x0001_0000).to_bytes(4, "little")
        block[12:16] = GPT_HEADER_SIZE.to_bytes(4, "little")
        block[24:32] = current.to_bytes(8, "little")
        block[32:40] = backup.to_bytes(8, "little")
        block[40:48] = GPT_RESERVED_LBA.to_bytes(8, "little")
        block[48:56] = (disk_sectors - GPT_BACKUP_SECTORS - 1).to_bytes(8, "little")
        block[56:72] = disk_guid
        block[72:80] = ent_lba.to_bytes(8, "little")
        block[80:84] = GPT_ENTRIES.to_bytes(4, "little")
        block[84:88] = GPT_ENTRY_SIZE.to_bytes(4, "little")
        block[88:92] = entries_crc.to_bytes(4, "little")
        crc = zlib.crc32(bytes(block[:GPT_HEADER_SIZE])) & 0xFFFF_FFFF
        if damaged:
            crc ^= 0x0000_00FF  # 存进去的 CRC 与人算的对不上 → 内核必须换用另一张表
        block[16:20] = crc.to_bytes(4, "little")
        return bytes(block)

    primary_damaged = corrupt in ("gpt-primary-crc", "gpt-both-headers")
    backup_damaged = corrupt == "gpt-both-headers"
    raw[1 * SECTOR : 2 * SECTOR] = header(1, disk_sectors - 1, entries_lba, primary_damaged)
    last = (disk_sectors - 1) * SECTOR
    raw[last : last + SECTOR] = header(
        disk_sectors - 1, 1, backup_entries_lba, backup_damaged
    )
    return bytes(raw)


def verify_tables(raw: bytes, table: str, primaries: list[Part], logicals: list[Part],
                  corrupt: str | None) -> None:
    """回读校验（另一条代码路径）：表结构、CRC、表项位置、载荷魔数。"""
    disk_sectors = len(raw) // SECTOR
    if table == "mbr":
        if raw[510:512] != b"\x55\xaa":
            raise SystemExit("verify: MBR boot signature missing")
        listed = []
        for index, part in enumerate(primaries[:4]):
            entry = raw[0x1BE + index * 16 : 0x1BE + (index + 1) * 16]
            listed.append((entry[4], int.from_bytes(entry[8:12], "little"),
                           int.from_bytes(entry[12:16], "little")))
        for index, part in enumerate(primaries[:4]):
            want_type = 0x0F if part.type_field == "extended" else mbr_type_byte(part)
            if listed[index] != (want_type, part.start, part.sectors):
                raise SystemExit(f"verify: MBR entry {index + 1} does not match the layout")
        for lp in logicals:
            offset = lp.ebr_lba * SECTOR
            if raw[offset + 510 : offset + 512] != b"\x55\xaa":
                raise SystemExit(f"verify: EBR for {lp.name} has no signature")
            entry = raw[offset + 0x1BE : offset + 0x1BE + 16]
            if int.from_bytes(entry[8:12], "little") != lp.start - lp.ebr_lba:
                raise SystemExit(f"verify: logical {lp.name} start does not match")
            if int.from_bytes(entry[12:16], "little") != lp.sectors:
                raise SystemExit(f"verify: logical {lp.name} size does not match")
        if corrupt == "ebr-broken":
            last = logicals[-1]
            offset = last.ebr_lba * SECTOR
            nxt = int.from_bytes(raw[offset + 0x1BE + 16 + 8 : offset + 0x1BE + 16 + 12], "little")
            if last.ebr_lba + nxt < disk_sectors:
                raise SystemExit("verify: ebr-broken fixture is not what it claims")
    else:
        if raw[0x1BE + 4] != 0xEE:
            raise SystemExit("verify: protective MBR entry (0xEE) missing")
        entries = raw[2 * SECTOR : 2 * SECTOR + GPT_ENTRIES * GPT_ENTRY_SIZE]
        backup_at = (disk_sectors - GPT_BACKUP_SECTORS) * SECTOR
        backup_entries = raw[backup_at : backup_at + GPT_ENTRIES * GPT_ENTRY_SIZE]
        if entries != backup_entries:
            raise SystemExit("verify: primary and backup GPT entries differ")

        def header_crc_ok(block: bytes) -> bool:
            probe = bytearray(block[:GPT_HEADER_SIZE])
            stored = int.from_bytes(probe[16:20], "little")
            probe[16:20] = b"\x00\x00\x00\x00"
            return zlib.crc32(bytes(probe)) & 0xFFFF_FFFF == stored

        # 损坏是**故意**的，因此这里的期望反过来：坏的那张表必须真的坏，好的必须真的好。
        primary_ok = header_crc_ok(raw[SECTOR : SECTOR + GPT_HEADER_SIZE])
        backup_ok = header_crc_ok(raw[-SECTOR:])
        if corrupt == "gpt-primary-crc":
            if primary_ok or not backup_ok:
                raise SystemExit("verify: gpt-primary-crc fixture is not what it claims")
        elif corrupt == "gpt-both-headers":
            if primary_ok or backup_ok:
                raise SystemExit("verify: gpt-both-headers fixture is not what it claims")
        elif not (primary_ok and backup_ok):
            raise SystemExit("verify: GPT header CRC is wrong")

        stored_entries_crc = int.from_bytes(raw[SECTOR + 88 : SECTOR + 92], "little")
        entries_crc_ok = stored_entries_crc == zlib.crc32(bytes(entries)) & 0xFFFF_FFFF
        if corrupt == "gpt-entries-crc":
            if entries_crc_ok:
                raise SystemExit("verify: gpt-entries-crc fixture is not what it claims")
        elif not entries_crc_ok:
            raise SystemExit("verify: GPT entries CRC is wrong")
        for index, part in enumerate(primaries):
            if part.type_field == "extended":
                continue
            entry = entries[index * GPT_ENTRY_SIZE : (index + 1) * GPT_ENTRY_SIZE]
            if int.from_bytes(entry[0x20:0x28], "little") != part.start:
                raise SystemExit(f"verify: GPT entry {index + 1} start does not match")
            if int.from_bytes(entry[0x28:0x30], "little") != part.start + part.sectors - 1:
                raise SystemExit(f"verify: GPT entry {index + 1} end does not match")
    # 载荷魔数（第 0 扇区）
    for part in primaries + logicals:
        head = raw[part.start * SECTOR : part.start * SECTOR + SECTOR]
        if part.fs in FAT_FS and head[510:512] != b"\x55\xaa":
            raise SystemExit(f"verify: FAT payload of {part.name} has no boot signature")
        if part.fs == "exfat" and head[3:11] != b"EXFAT   ":
            raise SystemExit(f"verify: exFAT fixture of {part.name} is malformed")
        if part.fs == "ntfs" and head[3:11] != b"NTFS    ":
            raise SystemExit(f"verify: NTFS fixture of {part.name} is malformed")
        if part.fs == "iso9660":
            pvd = part.start * SECTOR + 16 * SECTOR
            if raw[pvd + 1 : pvd + 6] != b"CD001":
                raise SystemExit(f"verify: ISO9660 fixture of {part.name} is malformed")


def build_partitioned(args: argparse.Namespace) -> int:
    """`--table gpt|mbr` 的路径：分区表 + 每个分区的载荷。"""
    primaries = [parse_part_spec(text, False) for text in args.part]
    logicals = [parse_part_spec(text, True) for text in args.logical]
    if not primaries:
        raise SystemExit("--table gpt|mbr needs at least one --part")
    if args.table == "gpt" and any(p.type_field == "extended" for p in primaries):
        raise SystemExit("extended containers are an MBR concept; GPT has no equivalent here")
    payloads = {}
    for text in args.payload:
        if "=" not in text:
            raise SystemExit(f"bad --payload spec: {text!r} (want name=path)")
        name, path = text.split("=", 1)
        payloads[name.strip()] = Path(path.strip())

    disk_sectors = layout(primaries, logicals, args.table == "gpt")
    if args.size:
        requested = parse_size(args.size)
        if requested < disk_sectors:
            raise SystemExit(
                f"--size {args.size} is too small: the layout needs {disk_sectors} sectors"
            )
        disk_sectors = requested

    tree = load_tree(Path(args.src)) if args.src else DirNode("/")
    for part in primaries + logicals:
        if part.type_field == "extended":
            continue
        part.payload = make_payload(part, tree, payloads)

    if args.table == "gpt":
        raw = render_gpt(primaries, disk_sectors, args.corrupt)
    else:
        raw = render_mbr(primaries, logicals, disk_sectors, args.corrupt)

    # 载荷写进各自的分区
    image = bytearray(raw)
    for part in primaries + logicals:
        if not part.payload:
            continue
        offset = part.start * SECTOR
        image[offset : offset + len(part.payload)] = part.payload
    raw = bytes(image)

    verify_tables(raw, args.table, primaries, logicals, args.corrupt)

    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_bytes(raw)
    parts_text = " ".join(p.summary() for p in primaries + logicals)
    print(
        f"mkfs: {out} table={args.table.upper()} sectors={disk_sectors} "
        f"partitions={len(primaries) + len(logicals)} corrupt={args.corrupt or 'none'} "
        f"({len(raw)} bytes)\n  {parts_text}"
    )
    return 0


def choose_geometry(fat_type: int) -> tuple[int, int, int]:
    """返回（每簇扇区数, 保留扇区数, 根目录项数）。

    取值要让「按簇数自检出的 FAT 类型」与请求的类型一致（见 `detected_type`）：
    - FAT32 需要 ≥ 65525 簇 → 64 MiB / 512 字节簇 = 129k 簇；
    - FAT16 需要 4085..65524 → 8 MiB / 512 字节簇；
    - FAT12 需要 < 4085 → 4 MiB / 2048 字节簇。
    """
    return {32: (1, 32, 0), 16: (1, 1, 512), 12: (4, 1, 512)}[fat_type]


def main() -> int:
    parser = argparse.ArgumentParser(description="Cerlesse mkfs (FAT12/16/32 + GPT/MBR 分区夹具)")
    parser.add_argument("--out", required=True, help="输出镜像路径")
    parser.add_argument("--fat", type=int, default=32, choices=(12, 16, 32), help="FAT 类型（默认 32）")
    parser.add_argument("--size", default=None, help="镜像大小，如 64M / 4M（默认按 FAT 类型算；分区模式为整盘大小）")
    parser.add_argument("--from", dest="src", default=None, help="把该目录树写进镜像根目录（或 efi-esp 分区）")
    # CSS-1：分区表镜像（见 docs/storage.md 第 9 节）
    parser.add_argument("--table", choices=("none", "gpt", "mbr"), default="none",
                        help="none = 超级软盘（现状）；gpt/mbr = 生成分区表镜像")
    parser.add_argument("--part", action="append", default=[], metavar="NAME:SIZE[:FS[:TYPE]]",
                        help="主分区（可重复）；TYPE 可取 efi/linux/ms-data/bios-boot、0xNN，或 extended")
    parser.add_argument("--logical", action="append", default=[], metavar="NAME:SIZE[:FS[:TYPE]]",
                        help="扩展分区里的逻辑分区（可重复，须有 fs/type 为 extended 的 --part）")
    parser.add_argument("--payload", action="append", default=[], metavar="NAME=PATH",
                        help="把预置文件系统镜像填进该分区（ext2/ext4 夹具由宿主 mkfs.ext* 生成）")
    parser.add_argument("--corrupt", choices=CORRUPTIONS, default=None,
                        help="故意损坏（用于断言 degraded/invalid 路径）：" + "/".join(CORRUPTIONS))
    args = parser.parse_args()

    if args.table != "none":
        return build_partitioned(args)

    total_sectors = parse_size(args.size or {32: "64M", 16: "8M", 12: "4M"}[args.fat])
    spc, reserved, root_entries = choose_geometry(args.fat)
    image = FatImage(args.fat, total_sectors, spc, reserved, root_entries)
    image.build(load_tree(Path(args.src)) if args.src else DirNode("/"))

    raw = image.render()
    image.verify(raw)

    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_bytes(raw)
    print(
        f"mkfs: {out} fat={args.fat} sectors={total_sectors} spc={spc} reserved={reserved} "
        f"fat_sectors={image.fat_size} clusters={image.clusters} files={image.files} "
        f"dirs={image.dirs} ({len(raw)} bytes)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
