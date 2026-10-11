#!/usr/bin/env python3
"""CSS-1 存储夹具的宿主侧门禁（不启 QEMU，见 docs/storage.md 第 9 节）。

## 它在验什么

`make storage-images` 用 `tools/mkfs/mkfs.py` 生成 GPT/MBR 夹具。本脚本用**另一套解析器**
（`zlib.crc32` + 直接读字节，与内核的逐位 CRC、mkfs 的写出路径都不是同一份代码）断言
「每个夹具就是它声称的那个东西」——尤其是那些**故意坏掉**的变体：坏要坏在对的地方，
否则内核侧断言「degraded/invalid」时根本不知道是因为夹具坏，还是因为内核看错了。

三方互查（tools/README.md 的工具表）：

| 夹具 | 第三方实现 | 本脚本做什么 |
| ---- | ---- | ---- |
| GPT 表 | `partx -s`（实测能枚举 GPT） | 主/备表头与表项 CRC、表项起止/类型 GUID、主备一致性 |
| MBR 表 | `partx -s`（含 EBR 链） | 4 个主表项 + EBR 链逐层校验 |
| FAT 载荷 | `mtools`（`mdir -i img@@offset`） | 校验载荷可被独立实现读取 |
| ext4 载荷 | `dumpe2fs` | 校验超级块魔数、UUID、卷标（超 11 字符的截断用例） |

## 与 QEMU 验收的分工

本脚本只看**文件**。内核看到什么由 `make test-storage` / `test-storage-mbr` 断言，两者
**不重合**：实测 OVMF 会在设备扫描阶段自愈 GPT（主表头坏、备表头好时把备份写回主位置），
所以「主表头坏的盘」进内核时已经好了 —— 那种路径由内核内存盘自测（`storage: parse selftest`）覆盖。

用法：`python3 tests/test_storage.py`（退出码 0 通过、1 断言失败、2 缺工具/缺夹具）。
"""

from __future__ import annotations

import shutil
import struct
import subprocess
import sys
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
STORAGE = ROOT / "build" / "storage"
SECTOR = 512
GPT_ENTRY_SIZE = 128
GPT_ENTRIES = 128
GPT_BACKUP_SECTORS = 33  # 备表项（32 扇区）+ 备表头（1 扇区）

ESP_GUID = bytes.fromhex("28732ac11ff8d211ba4b00a0c93ec93b")
LINUX_FS_GUID = bytes.fromhex("af3dc60f838472478e793d69d8477de4")
MS_DATA_GUID = bytes.fromhex("a2a0d0ebe5b9334487c068b6b72699c7")
UNKNOWN_GUID = bytes.fromhex("ad0bad0bad0bad0b0bad0bad0bad0bad")

failures: list[str] = []
checks = 0


def check(what: str, cond: bool, detail: str = "") -> None:
    """记一项断言（失败不中断：一次跑完能看出几张盘坏了）。"""
    global checks
    checks += 1
    if not cond:
        failures.append(f"{what}{(': ' + detail) if detail else ''}")


# ---- 独立的小解析器（第二套实现）----


def crc32(data: bytes) -> int:
    return zlib.crc32(data) & 0xFFFF_FFFF


def read(path: Path) -> bytes:
    return path.read_bytes()


def gpt_header_ok(block: bytes) -> tuple[bool, dict[str, int]]:
    """校验一个 GPT 表头（含自校验 CRC），返回（是否有效, 字段）。"""
    fields: dict[str, int] = {}
    if block[0:8] != b"EFI PART":
        return False, fields
    size = struct.unpack_from("<I", block, 12)[0]
    stored = struct.unpack_from("<I", block, 16)[0]
    probe = bytearray(block[:size])
    probe[16:20] = b"\0\0\0\0"
    for name, off, fmt in (
        ("current", 24, "<Q"),
        ("backup", 32, "<Q"),
        ("first_usable", 40, "<Q"),
        ("last_usable", 48, "<Q"),
        ("entries_lba", 72, "<Q"),
        ("count", 80, "<I"),
        ("entry_size", 84, "<I"),
        ("entries_crc", 88, "<I"),
    ):
        fields[name] = struct.unpack_from(fmt, block, off)[0]
    return crc32(bytes(probe)) == stored, fields


def gpt_entries(raw: bytes, fields: dict[str, int]) -> list[dict[str, object]]:
    """按表头给的偏移解析表项（非空表项才有意义）。"""
    start = fields["entries_lba"] * SECTOR
    size = fields["entry_size"]
    out: list[dict[str, object]] = []
    for index in range(fields["count"]):
        entry = raw[start + index * size : start + (index + 1) * size]
        if entry[:16] == bytes(16):
            continue
        out.append(
            {
                "index": index + 1,
                "type": entry[:16],
                "start": struct.unpack_from("<Q", entry, 0x20)[0],
                "end": struct.unpack_from("<Q", entry, 0x28)[0],
                "name": entry[0x38:0x38 + 72].decode("utf-16-le").rstrip("\0"),
            }
        )
    return out


def mbr_entries(sector: bytes) -> list[tuple[int, int, int]]:
    """返回 4 个主表项（类型, 起始 LBA, 扇区数）。"""
    out = []
    for index in range(4):
        off = 0x1BE + index * 16
        out.append((sector[off + 4], struct.unpack_from("<I", sector, off + 8)[0],
                    struct.unpack_from("<I", sector, off + 12)[0]))
    return out


def ebr_chain(raw: bytes, first: int, limit: int = 16) -> tuple[list[dict[str, int]], bool]:
    """沿 EBR 链枚举逻辑分区，返回（分区列表, 是否遇到坏链/越界）。"""
    logicals: list[dict[str, int]] = []
    lba = first
    seen: set[int] = set()
    broken = False
    for _ in range(limit):
        if lba in seen or (lba + 1) * SECTOR > len(raw):
            broken = True
            break
        seen.add(lba)
        ebr = raw[lba * SECTOR : (lba + 1) * SECTOR]
        if ebr[510:512] != b"\x55\xaa":
            broken = True
            break
        ptype = ebr[0x1BE + 4]
        rel = struct.unpack_from("<I", ebr, 0x1BE + 8)[0]
        sectors = struct.unpack_from("<I", ebr, 0x1BE + 12)[0]
        if ptype != 0 and sectors != 0:
            logicals.append({"ebr": lba, "type": ptype, "start": lba + rel, "sectors": sectors})
        next_type = ebr[0x1BE + 16 + 4]
        next_rel = struct.unpack_from("<I", ebr, 0x1BE + 16 + 8)[0]
        if next_type == 0 or next_rel == 0:
            break
        if lba + next_rel > len(raw) // SECTOR:
            broken = True
            break
        lba += next_rel
    return logicals, broken


def partx_rows(image: Path) -> list[list[str]] | None:
    """`partx -s`（第三方实现）的行；缺工具返回 None。"""
    if shutil.which("partx") is None:
        return None
    result = subprocess.run(["partx", "-s", str(image)], capture_output=True, text=True)
    rows: list[list[str]] = []
    for line in result.stdout.splitlines()[1:]:
        parts = line.split()
        if len(parts) >= 5 and parts[0].isdigit():
            rows.append(parts)
    return rows


def mdir_output(image: Path, offset_sectors: int) -> str | None:
    """用 mtools 读指定偏移上的 FAT 载荷；缺工具返回 None。"""
    if shutil.which("mdir") is None:
        return None
    result = subprocess.run(
        ["mdir", "-i", f"{image}@@{offset_sectors * SECTOR}", "::"],
        capture_output=True, text=True,
    )
    return result.stdout if result.returncode == 0 else ""


def dumpe2fs(path: Path) -> str | None:
    if shutil.which("dumpe2fs") is None:
        return None
    result = subprocess.run(["dumpe2fs", "-h", str(path)], capture_output=True, text=True)
    return result.stdout if result.returncode == 0 else ""


# ---- 每个夹具的断言 ----


def check_gpt_ok() -> None:
    path = STORAGE / "gpt.img"
    raw = read(path)
    sectors = len(raw) // SECTOR
    check("gpt.img: 保护性 MBR", raw[0x1BE + 4] == 0xEE and raw[510:512] == b"\x55\xaa")

    primary_ok, primary = gpt_header_ok(raw[SECTOR : 2 * SECTOR])
    last = (sectors - 1) * SECTOR
    backup_ok, backup = gpt_header_ok(raw[last : last + SECTOR])
    check("gpt.img: 主表头自校验", primary_ok)
    check("gpt.img: 备表头自校验", backup_ok)
    check("gpt.img: 主表头指向自己", primary.get("current") == 1)
    check("gpt.img: 主表指向备表", primary.get("backup") == sectors - 1)
    check("gpt.img: 第一个可用 LBA = 34", primary.get("first_usable") == 34)
    check("gpt.img: 表项区在 LBA 2", primary.get("entries_lba") == 2)
    check("gpt.img: 128 × 128 字节表项", primary.get("count") == 128 and primary.get("entry_size") == 128)

    entries = gpt_entries(raw, primary)
    check("gpt.img: 表项数", len(entries) == 7, str(len(entries)))
    check("gpt.img: 表项 CRC", crc32(raw[2 * SECTOR : 2 * SECTOR + GPT_ENTRIES * GPT_ENTRY_SIZE])
          == primary.get("entries_crc"))
    backup_at = (sectors - GPT_BACKUP_SECTORS) * SECTOR
    check("gpt.img: 主备表项逐字节相同",
          raw[2 * SECTOR : 2 * SECTOR + GPT_ENTRIES * GPT_ENTRY_SIZE]
          == raw[backup_at : backup_at + GPT_ENTRIES * GPT_ENTRY_SIZE])

    expected = [
        ("efi-esp", ESP_GUID, 2048, 16384),
        ("root", LINUX_FS_GUID, 18432, 16384),
        ("probe-exfat", MS_DATA_GUID, 34816, 8192),
        ("probe-iso", UNKNOWN_GUID, 43008, 2048),
        ("gpt-blank", UNKNOWN_GUID, 45056, 2048),
        ("clone-a", MS_DATA_GUID, 47104, 4096),
        ("clone-b", MS_DATA_GUID, 51200, 4096),
    ]
    for entry, (name, guid, start, sectors_) in zip(entries, expected):
        check(f"gpt.img: 表项 {entry['index']} 名字", entry["name"] == name, str(entry["name"]))
        check(f"gpt.img: 表项 {entry['index']} 类型 GUID", entry["type"] == guid, entry["type"].hex())
        check(f"gpt.img: 表项 {entry['index']} 起始 LBA", entry["start"] == start)
        check(f"gpt.img: 表项 {entry['index']} 扇区数", entry["end"] - entry["start"] + 1 == sectors_)

    # 三个分区的载荷魔数（内核探测读的就是这几个字节）
    check("gpt.img: probe-exfat 载荷魔数", raw[34816 * SECTOR + 3 : 34816 * SECTOR + 11] == b"EXFAT   ")
    check("gpt.img: probe-iso 载荷魔数", raw[43008 * SECTOR + 16 * SECTOR + 1 : 43008 * SECTOR + 16 * SECTOR + 6] == b"CD001")
    check("gpt.img: efi-esp 是 FAT", raw[2048 * SECTOR + 510 : 2048 * SECTOR + 512] == b"\x55\xaa")
    check("gpt.img: clone-a/clone-b 序列号相同（故意）",
          raw[47104 * SECTOR + 0x27 : 47104 * SECTOR + 0x2B]
          == raw[51200 * SECTOR + 0x27 : 51200 * SECTOR + 0x2B])

    # 第三方实现互查
    rows = partx_rows(path)
    if rows is None:
        print("  SKIP: 没有 partx，跳过 GPT 的第三方互查")
    else:
        check("gpt.img: partx 行数", len(rows) == 7, str(len(rows)))
        for row, (name, _, start, sectors_) in zip(rows, expected):
            check(f"gpt.img: partx {name} 起始", int(row[1]) == start, row[1])
            check(f"gpt.img: partx {name} 扇区数", int(row[3]) == sectors_, row[3])
            check(f"gpt.img: partx {name} 名字", row[5] == name, row[5])

    mdir = mdir_output(path, 2048)
    if mdir is None:
        print("  SKIP: 没有 mtools，跳过 FAT 载荷互查")
    else:
        check("gpt.img: mtools 能读 efi-esp 载荷", "hello    txt" in mdir, mdir[:120])

    # ext4 载荷（宿主 mkfs.ext4 生成）
    ext4 = dumpe2fs(STORAGE / "ext4-root.img")
    if ext4 is None:
        print("  SKIP: 没有 dumpe2fs，跳过 ext4 载荷互查")
    else:
        check("ext4-root.img: dumpe2fs 认出 ext4", "Filesystem features" in ext4 or "ext4" in ext4)
        check("ext4-root.img: 卷标（会被内核截到 11 字符）",
              "Filesystem volume name:   cerlesse-ext4-lo" in ext4, ext4[:200])
        check("ext4-root.img: UUID", "11111111-2222-3333-4444-555555555555" in ext4)
    raw_ext4 = read(STORAGE / "ext4-root.img")
    check("ext4-root.img: 超级块魔数 0xEF53", raw_ext4[1024 + 0x38 : 1024 + 0x3A] == b"\x53\xef")
    check("ext4-root.img: 大小与分区一致", len(raw_ext4) == 16384 * SECTOR)


def check_gpt_dirty() -> None:
    """两张脏盘：`gpt-degraded` 表项 CRC 坏；`gpt-invalid` 两张表头都坏。"""
    raw = read(STORAGE / "gpt-degraded.img")
    sectors = len(raw) // SECTOR
    primary_ok, primary = gpt_header_ok(raw[SECTOR : 2 * SECTOR])
    backup_ok, _ = gpt_header_ok(raw[(sectors - 1) * SECTOR :])
    check("gpt-degraded.img: 两张表头本身是好的", primary_ok and backup_ok)
    stored = primary.get("entries_crc")
    actual = crc32(raw[2 * SECTOR : 2 * SECTOR + GPT_ENTRIES * GPT_ENTRY_SIZE])
    check("gpt-degraded.img: 表项 CRC 确实对不上（声称的 != 实际）", stored != actual)
    check("gpt-degraded.img: 表项仍然可解析出 2 个分区",
          len(gpt_entries(raw, primary)) == 2)
    rows = partx_rows(STORAGE / "gpt-degraded.img")
    if rows is not None:
        check("gpt-degraded.img: partx 也因 CRC 不过而拒绝（独立实现的旁证）", rows == [], str(rows))

    raw = read(STORAGE / "gpt-invalid.img")
    sectors = len(raw) // SECTOR
    primary_ok, _ = gpt_header_ok(raw[SECTOR : 2 * SECTOR])
    backup_ok, _ = gpt_header_ok(raw[(sectors - 1) * SECTOR :])
    check("gpt-invalid.img: 主表头 CRC 坏", not primary_ok)
    check("gpt-invalid.img: 备表头 CRC 坏", not backup_ok)
    check("gpt-invalid.img: 签名还在（所以是「CRC 坏」而不是「没有表」）",
          raw[SECTOR : SECTOR + 8] == b"EFI PART" and raw[-SECTOR:-SECTOR + 8] == b"EFI PART")


def check_gpt_primary_bad() -> None:
    """主表头坏的盘：固件会拿备表头自愈，所以宿主侧要证明「坏得刚刚好」。"""
    path = STORAGE / "gpt-primary-bad.img"
    raw = read(path)
    sectors = len(raw) // SECTOR
    primary_ok, _ = gpt_header_ok(raw[SECTOR : 2 * SECTOR])
    backup_ok, backup = gpt_header_ok(raw[(sectors - 1) * SECTOR :])
    check("gpt-primary-bad.img: 主表头 CRC 坏", not primary_ok)
    check("gpt-primary-bad.img: 备表头完好", backup_ok)
    check("gpt-primary-bad.img: 备表头能解出 2 个分区", len(gpt_entries(raw, backup)) == 2)
    rows = partx_rows(path)
    if rows is not None:
        check("gpt-primary-bad.img: partx 会用备表头列出 2 个分区（第三方旁证）",
              len(rows) == 2, str(rows))


def check_mbr() -> None:
    path = STORAGE / "mbr.img"
    raw = read(path)
    boot = raw[:SECTOR]
    entries = mbr_entries(boot)
    check("mbr.img: 引导签名", boot[510:512] == b"\x55\xaa")
    check("mbr.img: 主分区 1（FAT12）", entries[0] == (0x01, 2048, 8192), str(entries[0]))
    check("mbr.img: 扩展分区（0x0F）", entries[1][0] == 0x0F and entries[1][1] == 10240, str(entries[1]))
    check("mbr.img: 后两个表项为空", entries[2] == (0, 0, 0) and entries[3] == (0, 0, 0))

    logicals, broken = ebr_chain(raw, entries[1][1])
    check("mbr.img: EBR 链完整", not broken)
    check("mbr.img: 逻辑分区数 = 2", len(logicals) == 2, str(logicals))
    check("mbr.img: 逻辑分区 5 起始 10241", logicals[0]["start"] == 10241)
    check("mbr.img: 逻辑分区 6 起始 14338", logicals[1]["start"] == 14338)
    check("mbr.img: 扩展分区覆盖逻辑分区",
          entries[1][1] + entries[1][2] >= logicals[1]["start"] + logicals[1]["sectors"])

    check("mbr.img: efi-esp 是 FAT12 载荷", raw[2048 * SECTOR + 510 : 2048 * SECTOR + 512] == b"\x55\xaa")
    check("mbr.img: probe-ntfs 载荷魔数", raw[10241 * SECTOR + 3 : 10241 * SECTOR + 11] == b"NTFS    ")

    rows = partx_rows(path)
    if rows is None:
        print("  SKIP: 没有 partx，跳过 MBR 的第三方互查")
    else:
        numbers = [int(row[0]) for row in rows]
        check("mbr.img: partx 列出 1/2/5/6", numbers == [1, 2, 5, 6], str(numbers))
        check("mbr.img: partx 与内核看到的逻辑分区一致",
              {"start": int(rows[2][1]), "sectors": int(rows[2][3])}
              == {"start": logicals[0]["start"], "sectors": logicals[0]["sectors"]})

    mdir = mdir_output(path, 2048)
    if mdir is None:
        print("  SKIP: 没有 mtools，跳过 FAT12 载荷互查")
    else:
        check("mbr.img: mtools 能读 efi-esp 载荷", "hello    txt" in mdir, mdir[:120])


def check_mbr_broken() -> None:
    path = STORAGE / "mbr-broken.img"
    raw = read(path)
    entries = mbr_entries(raw[:SECTOR])
    check("mbr-broken.img: 只有一个扩展分区表项", entries[0][0] == 0x0F and entries[1][0] == 0)
    logicals, broken = ebr_chain(raw, entries[0][1])
    check("mbr-broken.img: 断链被独立实现看出来", broken)
    check("mbr-broken.img: 断链前仍有 2 个逻辑分区", len(logicals) == 2, str(logicals))


def main() -> int:
    missing = [f.name for f in (
        STORAGE / "gpt.img", STORAGE / "gpt-degraded.img", STORAGE / "gpt-primary-bad.img",
        STORAGE / "gpt-invalid.img", STORAGE / "mbr.img", STORAGE / "mbr-broken.img",
        STORAGE / "ext4-root.img",
    ) if not f.is_file()]
    if missing:
        print(f"FAIL: 夹具不存在：{', '.join(missing)}（先跑 `make storage-images`）")
        return 2

    print("storage fixtures: 宿主侧复核（第二套解析器 + partx/mtools/dumpe2fs）")
    check_gpt_ok()
    check_gpt_dirty()
    check_gpt_primary_bad()
    check_mbr()
    check_mbr_broken()

    if failures:
        for item in failures:
            print(f"FAIL: {item}")
        print(f"\n{len(failures)}/{checks} 项断言失败")
        return 1
    print(f"PASS: {checks} 项断言全部通过（{len({p.name for p in STORAGE.glob('*.img')})} 个镜像）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
