#!/usr/bin/env python3
"""Cerlesse 磁盘镜像构建脚本（v0.1 实现）。

把 UEFI bootloader（boot.efi → \\EFI\\BOOT\\BOOTX64.EFI）与内核 ELF
（kernel → \\KERNEL.ELF）组装为 QEMU/OVMF 可启动的 FAT 磁盘镜像。

依赖：mtools（mformat / mmd / mcopy），无需 root 挂载。
"""

import argparse
import shutil
import subprocess
import sys
from pathlib import Path

BLOCK_SIZE = 1024 * 1024  # 1 MiB（镜像容量的单位）
IMAGE_BLOCKS = 64  # 设备总容量 64 MiB（内核看到的是 131072 个 512 字节扇区）
# CKM-2：镜像尾部保留 1 MiB 作 **CKM 状态区**（`docs/ckm.md` 第 12 节开放问题 1 的定夺
# ＝「极小的日志式区：双槽 A/B + CRC」，内核侧实现在 `kernel/src/ckm/store.rs`）。
#
# 为什么用「先按 63 MiB 格式化、再追加」这个形状：mtools 的 FAT 几何是按**文件大小**
# 算出来的，先在 63 MiB 的文件上 `mformat`，再把保留区追加回去，文件系统就**看不到**
# 尾部这 1 MiB —— 状态区在 FAT 之外，不需要内核去理解 FAT 的分配表，也不会与用户文件
# 争簇。设备容量（64 MiB）与分区识别（`table=none partitions=1`）都因此保持不变。
RESERVED_BLOCKS = 1


def run(cmd: list[str]) -> None:
    result = subprocess.run(cmd, capture_output=True, text=True)
    if result.returncode != 0:
        sys.stderr.write(f"command failed: {' '.join(cmd)}\n")
        sys.stderr.write(result.stderr)
        raise SystemExit(1)


def main() -> None:
    parser = argparse.ArgumentParser(description="build Cerlesse bootable FAT image")
    parser.add_argument("--boot", required=True, help="path to boot.efi")
    parser.add_argument("--kernel", required=True, help="path to kernel ELF")
    parser.add_argument("--out", required=True, help="output disk image path")
    args = parser.parse_args()

    boot = Path(args.boot)
    kernel = Path(args.kernel)
    out = Path(args.out)

    for tool in ("mformat", "mmd", "mcopy"):
        if shutil.which(tool) is None:
            raise SystemExit(f"missing tool: {tool} (install mtools)")

    if not boot.is_file():
        raise SystemExit(f"bootloader not found: {boot} (run `make boot` first)")
    if not kernel.is_file():
        raise SystemExit(f"kernel not found: {kernel} (run `make kernel` first)")

    out.parent.mkdir(parents=True, exist_ok=True)
    fat_bytes = BLOCK_SIZE * (IMAGE_BLOCKS - RESERVED_BLOCKS)
    with out.open("wb") as f:
        f.write(b"\x00" * fat_bytes)

    img = str(out)
    run(["mformat", "-i", img, "::"])
    run(["mmd", "-i", img, "::/EFI"])
    run(["mmd", "-i", img, "::/EFI/BOOT"])
    run(["mcopy", "-i", img, str(boot), "::/EFI/BOOT/BOOTX64.EFI"])
    run(["mcopy", "-i", img, str(kernel), "::/KERNEL.ELF"])

    # 保留区：追加 1 MiB 零扇区，把镜像补回 64 MiB。FAT 与固件都看不到它。
    with out.open("ab") as f:
        f.write(b"\x00" * (BLOCK_SIZE * RESERVED_BLOCKS))

    print(
        f"image ready: {out} ({out.stat().st_size} bytes; "
        f"fat={fat_bytes} bytes, ckm state area={BLOCK_SIZE * RESERVED_BLOCKS} bytes)"
    )


if __name__ == "__main__":
    main()
