#!/usr/bin/env python3
"""CKM-2 持久化验收：**写 → 重启 → 读回一致**（`docs/ckm.md` 9 节的 CKM-2 出口）。

CSS-3 起它还顺带验收**第二个消费者**：盘符映射（`storage/letters`）与 CKM-2 的
自测键住在**同一个状态区**里（`docs/storage.md` 6.6 的命名空间 `storage/*`）。
证据是可对齐的两行：启动 #1 把 `Z` 写进去（`put key=storage/letters`），
启动 #2 **不重写**、而是把盘上的映射装载成 `letter=Z`。

为什么需要这个脚本而不是再给 `make test-ckm` 多加几条 `--expect`：验收要的是
「**跨重启**一致」，而 `tests/test_boot.py` 一次只启一次 QEMU。这个脚本把两件事接起来：

1. 把 `build/disk.img` 复制成一块**独占**的镜像（`build/disk-ckm-store.img`），
   并把尾部 1 MiB（CKM 状态区，见 `tools/image-builder/build_image.py`）**清零** ——
   否则「上一次 `make run` 留下的状态」会让证据行变成 `state=loaded`，
   验收就失去了确定性；
2. 第一次启动：`--persist`（blk0 不带 `snapshot=on`）→ 内核初始化状态区（`state=fresh`，
   两个槽 A/B 各写一次）→ 断言「写」的证据；
3. 第二次启动**同一份**镜像：内核装载上次写下的槽（`state=loaded … keys=3`）→
   读回**逐字节相同**的值，且**没有重写**（证据行紧跟 `state=loaded`，中间没有 `put`）。

只读断言都在 `make test-ckm` 里（覆盖层与不依赖持久化的证据），这里只管持久化。
用法：

    python3 tests/test_ckm_store.py                     # 默认镜像与日志路径
    python3 tests/test_ckm_store.py --timeout 20         # 缩短观察窗口
    python3 tests/test_ckm_store.py --serial-log-prefix build/orch/ckm-store/serial
"""

from __future__ import annotations

import argparse
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BUILD = ROOT / "build"

# CKM 状态区大小（字节）—— 必须与 kernel/src/ckm/store.rs 的 AREA_SECTORS（2048 扇区）一致。
STATE_AREA_BYTES = 2048 * 512

# 启动 #1（全新区）：状态区两个槽各写一次，A/B 交替因此是**可断言**的。
# CSS-3 的盘符探针是第四次写入（seq=4 → 槽 B），键值是 `Z=<vol_id>`（13 字节）。
EXPECT_A = [
    "Kernel started!",
    "ckm: store area dev=virtio-blk0 lba=129024 slots=2 slot_bytes=4096 state=fresh",
    "ckm: store put key=selftest len=21 seq=1 slot=A",
    "ckm: store put key=msg len=10 seq=2 slot=B",
    "ckm: store put key=storage/letters len=13 seq=4 slot=B",
    "ckm: store verify key=selftest value=cerlesse-ckm-store-v1 ok=1",
    "ckm: store verify key=msg value=persist-ok ok=1",
    "storage: letters saved custom=1",
]

# 启动 #2（同一份镜像）：装载上次的槽（seq=6 = 启动 #1 的最后一写；四个键里含
# `storage/letters`）。
EXPECT_B = [
    "Kernel started!",
    "ckm: store area dev=virtio-blk0 lba=129024 slots=2 slot_bytes=4096 "
    "state=loaded slot=B seq=6 keys=4",
    "ckm: store verify key=selftest value=cerlesse-ckm-store-v1 ok=1",
    "ckm: store verify key=msg value=persist-ok ok=1",
    "storage: letters load n=1 source=store keys=storage/letters",
    "storage: letters reloaded from store probe=Z vol=",
]

# CSS-3：盘符是**从盘上装载**的 —— 启动 #2 的卷行里 `letter=Z`（缺省规则只会给 `C`，
# 因此它只可能来自状态区），且**没有**重新分配（中间没有 `storage: letter … custom=1`）。
EXPECT_RE_LETTERS_B = [
    r"storage: letters load n=1 source=store keys=storage/letters\r?\n"
    r"storage: disk index=0 .*\r?\n"
    r"storage: letters table n=2 present=2 first=C\r?\n"
    r"storage: vol id=[^ ]+ disk=0 part=0 fs=fat16 label=[^ ]+ letter=C mounts=0.*?"
    r"storage: vol id=(fs:[0-9a-f]+) disk=1 part=0 fs=fat32 label=[^ ]+ letter=Z mounts=0.*?"
    r"storage: letters reloaded from store probe=Z vol=\1",
]

# 启动 #1：探针打的是**非系统卷**（系统卷的 `C` 属于缺省规则，自测不碰它）。
EXPECT_RE_LETTERS_A = [
    r"storage: letters table n=2 present=2 first=C\r?\n"
    r"storage: vol id=[^ ]+ disk=0 part=0 fs=fat16 label=[^ ]+ letter=C mounts=0.*?"
    r"storage: vol id=(fs:[0-9a-f]+) disk=1 part=0 fs=fat32 label=[^ ]+ letter=D mounts=0.*?"
    r"storage: letters persisted probe=Z vol=\1",
]

# 第二次启动必须**只读**：装载了四个键（含 CSS-3 的盘符映射），且不再写入。
# 「不重写」由下面的 Python 侧断言（日志里不得出现 put selftest/msg/storage/letters）——
# 字面量只能证明行存在，证明不了「第二次没写」，而后者才是「读回的是盘上的字节」
# 这条验收的实质（两个启动都打印 put 也能「通过」，那时验收就是空话）。
# 注意 `\r?\n`：串口是 CRLF，日志里保留了 `\r`（只在 `\n` 上锚定会匹配不到）。
EXPECT_RE_B = [r"state=loaded slot=B seq=6 keys=4\r?\n"]

# 第二次启动**不写**的键（第一次启动写过它们，见 EXPECT_A 与下面的 Python 断言）。
NEVER_REWRITTEN_B = [
    "ckm: store put key=selftest",
    "ckm: store put key=msg",
    "ckm: store put key=storage/letters",
]

# 两次启动都成立的通用断言（自测本身跑过）。
EXPECT_BOTH_RE = [r"ckm: store selftest PASS \(checks=[0-9]+\)"]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="CKM-2 persistence (write / restart / read back)")
    parser.add_argument("--disk", default=str(BUILD / "disk.img"), help="源镜像（只读）")
    parser.add_argument(
        "--out",
        default=str(BUILD / "disk-ckm-store.img"),
        help="独占副本路径（会被清零尾部状态区后反复启动）",
    )
    parser.add_argument(
        "--disk2",
        default=str(BUILD / "fat.img"),
        help="第二块盘（`make fat-image` 生成）：CSS-3 的盘符探针需要一个**非系统卷**",
    )
    parser.add_argument(
        "--serial-log-prefix",
        default=str(BUILD / "serial-ckm-store"),
        help="串口日志前缀（产出 <prefix>-a.log / <prefix>-b.log）",
    )
    parser.add_argument(
        "--ovmf-vars",
        default=None,
        help="OVMF_VARS 副本（并行编排时按场景隔离；透传给两次启动）",
    )
    parser.add_argument("--timeout", type=int, default=25, help="每次启动的观察窗口（秒）")
    return parser.parse_args()


def boot(args: argparse.Namespace, log: Path, expects: list[str], regexes: list[str]) -> bool:
    """跑一次启动（走 tests/test_boot.py 的同一条路径，参数只是透传）。"""
    cmd = [
        sys.executable,
        str(ROOT / "tests" / "test_boot.py"),
        "--disk",
        args.out,
        "--disk2",
        args.disk2,
        "--persist",
        "--serial-log",
        str(log),
        "--timeout",
        str(args.timeout),
    ]
    if args.ovmf_vars:
        cmd += ["--ovmf-vars", args.ovmf_vars]
    for expect in expects:
        cmd += ["--expect", expect]
    for pattern in regexes:
        cmd += ["--expect-re", pattern]
    result = subprocess.run(cmd, cwd=ROOT, check=False)
    return result.returncode == 0


def main() -> int:
    args = parse_args()
    source = Path(args.disk)
    out = Path(args.out)
    if not source.is_file():
        print(f"FAIL: disk image not found: {source} (run `make image` first)")
        return 2
    if not shutil.which("qemu-system-x86_64"):
        print("FAIL: missing tool qemu-system-x86_64")
        return 2

    # 1. 独占副本 + 状态区清零（确定性：第一次启动必然是 `state=fresh`）
    out.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, out)
    size = out.stat().st_size
    if size < STATE_AREA_BYTES:
        print(f"FAIL: image too small for the CKM state area: {size} bytes")
        return 2
    with out.open("r+b") as f:
        f.seek(size - STATE_AREA_BYTES)
        f.write(b"\x00" * STATE_AREA_BYTES)
    print(f"prepared: {out} ({size} bytes, wiped state area = {STATE_AREA_BYTES} bytes)")

    log_a = Path(f"{args.serial_log_prefix}-a.log")
    log_b = Path(f"{args.serial_log_prefix}-b.log")
    log_a.parent.mkdir(parents=True, exist_ok=True)

    # 2. 启动 #1：写（fresh + A/B 交替）
    print("boot 1/2: write (fresh state area)")
    if not boot(args, log_a, EXPECT_A, EXPECT_BOTH_RE + EXPECT_RE_LETTERS_A):
        print(f"FAIL: boot 1/2 (write) failed (log: {log_a})")
        return 1

    # 3. 启动 #2：同一份镜像 → 读回
    print("boot 2/2: restart and read back")
    if not boot(args, log_b, EXPECT_B, EXPECT_BOTH_RE + EXPECT_RE_B + EXPECT_RE_LETTERS_B):
        print(f"FAIL: boot 2/2 (read back) failed (log: {log_b})")
        return 1

    # 4. 跨启动的**只读**证据：第一次真的写了、第二次真的没写。
    text_a = log_a.read_text(errors="replace")
    text_b = log_b.read_text(errors="replace")
    for key in ("selftest", "msg", "storage/letters"):
        marker = f"ckm: store put key={key}"
        if marker not in text_a:
            print(f"FAIL: boot 1/2 never wrote '{key}' (log: {log_a})")
            return 1
    for marker in NEVER_REWRITTEN_B:
        if marker in text_b:
            print(f"FAIL: boot 2/2 rewrote '{marker}' (log: {log_b})")
            return 1
    if "storage: letters saved" in text_b:
        print(f"FAIL: boot 2/2 re-saved the drive-letter mapping (log: {log_b})")
        return 1

    print(
        "PASS: CKM-2 persistence (state=fresh → slot A/B writes → restart → "
        "state=loaded read back byte-identical) + CSS-3 drive letters "
        f"(storage/letters written once, reloaded as letter=Z; logs: {log_a}, {log_b})"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
