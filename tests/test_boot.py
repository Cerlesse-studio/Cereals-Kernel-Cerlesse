#!/usr/bin/env python3
"""QEMU 启动集成测试（v0.1 实现，v0.2 支持多断言与异常注入镜像）。

启动 QEMU（OVMF + 磁盘镜像），收集串口输出，断言所有 --expect 字符串出现。
超时后杀掉 QEMU，以断言结果决定成败。

用法：
  python3 tests/test_boot.py                          # 默认：disk.img，断言 "Kernel started!"
  python3 tests/test_boot.py --disk build/disk-exception.img \
      --expect "Kernel started!" --expect "EXCEPTION: divide error"
"""

import argparse
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BUILD = ROOT / "build"
OVMF_CODE = Path("/usr/share/OVMF/OVMF_CODE.fd")
OVMF_VARS_SRC = Path("/usr/share/OVMF/OVMF_VARS.fd")
TIMEOUT_SECONDS = 40


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Cerlesse QEMU serial-log test")
    parser.add_argument("--disk", default=str(BUILD / "disk.img"), help="磁盘镜像路径")
    parser.add_argument(
        "--disk2",
        default=None,
        help="第二块 virtio-blk 盘（v0.8 test-fat：mkfs 生成的 FAT 测试盘，内核挂到 /mnt/fat）",
    )
    parser.add_argument(
        "--expect",
        action="append",
        default=None,
        help="串口必须出现的字符串（可重复）；默认断言 'Kernel started!'",
    )
    parser.add_argument("--serial-log", default=None, help="串口日志输出路径")
    parser.add_argument(
        "--ovmf-vars",
        default=None,
        help="OVMF_VARS 副本路径（并行编排时按场景隔离，默认 build/OVMF_VARS.fd）",
    )
    parser.add_argument(
        "--timeout",
        type=int,
        default=TIMEOUT_SECONDS,
        help="QEMU 观察窗口秒数（默认 40）",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    disk = Path(args.disk)
    disk2 = Path(args.disk2) if args.disk2 else None
    expects = [s.encode() for s in (args.expect or ["Kernel started!"])]
    serial_log = Path(args.serial_log) if args.serial_log else BUILD / "serial.log"
    timeout = args.timeout

    if shutil.which("qemu-system-x86_64") is None:
        print("FAIL: missing tool qemu-system-x86_64")
        return 2
    if not disk.is_file():
        print(f"FAIL: disk image not found: {disk} (run `make image` first)")
        return 2
    if disk2 is not None and not disk2.is_file():
        print(f"FAIL: second disk image not found: {disk2} (run `make fat-image` first)")
        return 2

    BUILD.mkdir(parents=True, exist_ok=True)
    vars_copy = Path(args.ovmf_vars) if args.ovmf_vars else BUILD / "OVMF_VARS.fd"
    vars_copy.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(OVMF_VARS_SRC, vars_copy)

    cmd = [
        "qemu-system-x86_64",
        "-machine", "q35",
        "-m", "512M",
        "-drive", f"if=pflash,format=raw,readonly=on,file={OVMF_CODE}",
        "-drive", f"if=pflash,format=raw,file={vars_copy}",
        # snapshot=on：磁盘以临时写时复制方式打开，不占用镜像写锁，
        # 多个并行场景可共享同一磁盘镜像（H-04 编排器），且不污染原始镜像。
        #
        # v0.8：启动盘挂在 **virtio-blk（legacy）** 上，因为内核里的块驱动就驱动它
        #（kernel/src/driver/virtio_blk.rs）——「镜像能启动」与「驱动能读写这块盘」
        # 因此是同一台设备上的同一条路径。disable-modern=on 强制设备暴露 legacy
        # 接口（vendor 1af4 / device 0x1001 / BAR0 是 I/O 窗口）。
        "-drive", f"if=none,id=blk0,format=raw,file={disk},snapshot=on",
        "-device", "virtio-blk-pci,drive=blk0,disable-modern=on",
        "-serial", "stdio",
        "-display", "none",
        "-no-reboot",
    ]

    if disk2 is not None:
        # 第二块盘：排在 blk0 之后 → PCI 上就是下一个 function，块设备号 = 1。
        # 内核按「探测顺序」注册设备，因此 blk0 仍是 `/disk`（启动盘）、blk1 是 `/mnt/fat`。
        cmd[cmd.index("-serial"):cmd.index("-serial")] = [
            "-drive", f"if=none,id=blk1,format=raw,file={disk2},snapshot=on",
            "-device", "virtio-blk-pci,drive=blk1,disable-modern=on",
        ]

    print(f"running: {' '.join(cmd)}")
    timed_out = False
    try:
        result = subprocess.run(
            cmd, capture_output=True, timeout=timeout, check=False
        )
        output = result.stdout + result.stderr
    except subprocess.TimeoutExpired as expired:
        timed_out = True
        output = (expired.stdout or b"") + (expired.stderr or b"")

    serial_log.write_bytes(output)

    missing = [exp for exp in expects if exp not in output]
    if not missing:
        print(f"PASS: all {len(expects)} expectation(s) found on serial (log: {serial_log})")
        return 0

    for exp in missing:
        print(f"FAIL: {exp!r} not found on serial (log: {serial_log})")
    if timed_out:
        print(f"  (qemu was killed after {timeout}s)")
    tail = output[-4000:]
    print("--- serial tail ---")
    sys.stdout.buffer.write(tail)
    sys.stdout.buffer.flush()
    print()
    return 1


if __name__ == "__main__":
    sys.exit(main())
