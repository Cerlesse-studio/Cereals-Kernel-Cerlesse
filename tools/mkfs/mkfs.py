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
"""

from __future__ import annotations

import argparse
import math
import sys
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
                 reserved: int, root_entries: int) -> None:
        self.fat_type = fat_type
        self.total_sectors = total_sectors
        self.spc = spc
        self.reserved = reserved
        self.root_entries = root_entries
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
        boot[id_offset : id_offset + 4] = (0x20261011).to_bytes(4, "little")
        boot[id_offset + 4 : id_offset + 15] = b"NO NAME    "
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


def choose_geometry(fat_type: int) -> tuple[int, int, int]:
    """返回（每簇扇区数, 保留扇区数, 根目录项数）。

    取值要让「按簇数自检出的 FAT 类型」与请求的类型一致（见 `detected_type`）：
    - FAT32 需要 ≥ 65525 簇 → 64 MiB / 512 字节簇 = 129k 簇；
    - FAT16 需要 4085..65524 → 8 MiB / 512 字节簇；
    - FAT12 需要 < 4085 → 4 MiB / 2048 字节簇。
    """
    return {32: (1, 32, 0), 16: (1, 1, 512), 12: (4, 1, 512)}[fat_type]


def main() -> int:
    parser = argparse.ArgumentParser(description="Cerlesse mkfs (FAT12/16/32)")
    parser.add_argument("--out", required=True, help="输出镜像路径")
    parser.add_argument("--fat", type=int, default=32, choices=(12, 16, 32), help="FAT 类型（默认 32）")
    parser.add_argument("--size", default=None, help="镜像大小，如 64M / 4M（默认按 FAT 类型选）")
    parser.add_argument("--from", dest="src", default=None, help="把该目录树写进镜像根目录")
    args = parser.parse_args()

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
