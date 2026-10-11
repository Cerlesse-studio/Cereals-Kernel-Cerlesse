#!/usr/bin/env python3
"""键盘/交互式输入集成测试（v0.4 键盘验收 + v0.9 Shell 交互验收）。

流程：
1. 启动 QEMU（OVMF + 磁盘镜像），串口输出到管道，监控台开在 unix socket；
2. 等待串口出现就绪标记（`--ready-marker`，默认内核的 ``KBIRQ enabled``）：
   - 键盘回显（`make test-keyboard`）等早期标记即可；
   - 交互式 Shell（`make test-shell`）必须等到 ``user: program=/bin/sh``，
     否则按键会落在 Shell 还没开始跑的时刻（打进内核启动阶段的行缓冲）；
3. 通过 QEMU monitor 的 ``sendkey`` 注入真实按键（`--key` 可重复）；
4. 断言串口日志包含全部 ``--expect`` 字符串（字面子串匹配），
   并通过 ``--expect-re`` 追加**正则**断言。后者在整份日志上做 ``re.search``
   （``re.DOTALL``：``.`` 也能跨行），因此可以用反向引用把同一份日志里
   **先捕获的值**与后面的输出对齐 —— 例如
   ``--expect-re "user: fork parent=3 -> child pid=([0-9]+).*?user: exec pid=\1"``
   断言「execve 之后 PID 不变」。PID 的具体数值依赖内核启动顺序
   （调度器自测会先消耗几个号），所以断言应当绑定**一致性**而不是某个常数。

v0.9 起回显协议是**终端式原样回显**（驱动器把字符直接送到串口、Enter 换行并打印
`kbd: line ready len=N`），因此断言的应是 `kbd: line ready ...` 而不是旧的 `KB <字符>`
——理由见 ``kernel/src/driver/keyboard.rs`` 模块文档里的回显协议表。

用法：
  python3 tests/test_keyboard.py --disk build/disk.img \\
      --serial-log build/serial-kb.log --key a --key b --key ret \\
      --expect "KBIRQ ENTRY" --expect "kbd: line ready len=3" --expect "KBIRQ EXIT"
"""

import argparse
import re
import shutil
import socket
import subprocess
import sys
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BUILD = ROOT / "build"
OVMF_CODE = Path("/usr/share/OVMF/OVMF_CODE.fd")
OVMF_VARS_SRC = Path("/usr/share/OVMF/OVMF_VARS.fd")
TIMEOUT_SECONDS = 45
# 默认就绪标记：v0.4 的键盘 IRQ 武装点（Shell 用 --ready-marker 换成 `user: program=/bin/sh`）。
READY_MARKER = b"KBIRQ enabled"
# 默认按键与断言：a/b + Enter → 行缓冲应当交付 3 字节（`ab` + 行尾 \n）。
DEFAULT_KEYS = ["a", "b", "ret"]
DEFAULT_EXPECTS = ["KBIRQ ENTRY", "kbd: line ready len=3", "KBIRQ EXIT"]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Cerlesse keyboard serial-log test")
    parser.add_argument("--disk", default=str(BUILD / "disk-kb.img"), help="磁盘镜像路径")
    parser.add_argument(
        "--expect",
        action="append",
        default=None,
        help="串口必须出现的字符串（可重复）",
    )
    parser.add_argument(
        "--expect-re",
        action="append",
        default=None,
        help="串口日志必须匹配的正则（可重复；整份日志上 re.search，DOTALL；可用 \\1 反向引用前面的捕获组）",
    )
    parser.add_argument(
        "--key",
        action="append",
        default=None,
        help="通过 QEMU monitor 注入的按键名（可重复），默认 a/b/ret",
    )
    parser.add_argument(
        "--ready-marker",
        default=None,
        help=f"注入按键前必须在串口看到的标记（默认 {READY_MARKER.decode()!r}）",
    )
    parser.add_argument("--serial-log", default=None, help="串口日志输出路径")
    parser.add_argument("--timeout", type=int, default=TIMEOUT_SECONDS, help="等待秒数")
    parser.add_argument(
        "--ovmf-vars",
        default=None,
        help="OVMF_VARS 副本路径（并行编排时按场景隔离，默认 build/OVMF_VARS.fd）",
    )
    parser.add_argument(
        "--monitor",
        default=None,
        help="QEMU monitor socket 路径（默认 build/qemu-monitor.sock）",
    )
    return parser.parse_args()


class SerialCapture:
    """在后台线程里持续收集 QEMU 的串口输出。"""

    def __init__(self, stream) -> None:
        self._stream = stream
        self._buf = bytearray()
        self._lock = threading.Lock()
        self._thread = threading.Thread(target=self._pump, daemon=True)
        self._thread.start()

    def _pump(self) -> None:
        while True:
            chunk = self._stream.readline()
            if not chunk:
                return
            with self._lock:
                self._buf.extend(chunk)

    def snapshot(self) -> bytes:
        with self._lock:
            return bytes(self._buf)

    def wait_for(self, marker: bytes, timeout: float) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if marker in self.snapshot():
                return True
            time.sleep(0.2)
        return marker in self.snapshot()


def send_keys(sock_path: Path, keys: list[str]) -> None:
    """连接 QEMU monitor 并逐个注入按键。"""
    last_error: OSError | None = None
    for _ in range(50):  # 等待 monitor socket 就绪
        try:
            conn = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            conn.connect(str(sock_path))
            break
        except OSError as err:  # monitor 尚未监听
            last_error = err
            time.sleep(0.2)
    else:
        raise RuntimeError(f"cannot connect to QEMU monitor: {last_error}")

    with conn:
        conn.settimeout(5.0)
        try:
            conn.recv(4096)  # monitor 欢迎信息
        except OSError:
            pass
        for key in keys:
            conn.sendall(f"sendkey {key}\n".encode())
            time.sleep(0.2)
        time.sleep(0.5)


def main() -> int:
    args = parse_args()
    disk = Path(args.disk)
    keys = args.key or DEFAULT_KEYS
    # 只给了 --expect-re 时不要退回到键盘默认断言（那些是 `make test-keyboard` 的）。
    expects = args.expect if args.expect is not None else ([] if args.expect_re else DEFAULT_EXPECTS)
    regexes = args.expect_re or []
    ready_marker = (args.ready_marker or READY_MARKER.decode()).encode()
    serial_log = Path(args.serial_log) if args.serial_log else BUILD / "serial-kb.log"

    if shutil.which("qemu-system-x86_64") is None:
        print("FAIL: missing tool qemu-system-x86_64")
        return 2
    if not disk.is_file():
        print(f"FAIL: disk image not found: {disk} (run `make image` first)")
        return 2

    BUILD.mkdir(parents=True, exist_ok=True)
    vars_copy = Path(args.ovmf_vars) if args.ovmf_vars else BUILD / "OVMF_VARS.fd"
    vars_copy.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(OVMF_VARS_SRC, vars_copy)

    monitor = Path(args.monitor) if args.monitor else BUILD / "qemu-monitor.sock"
    if monitor.exists():
        monitor.unlink()

    cmd = [
        "qemu-system-x86_64",
        "-machine", "q35",
        "-m", "512M",
        "-drive", f"if=pflash,format=raw,readonly=on,file={OVMF_CODE}",
        "-drive", f"if=pflash,format=raw,file={vars_copy}",
        # snapshot=on：磁盘以临时写时复制方式打开，不占用镜像写锁，
        # 多个并行场景可共享同一磁盘镜像（H-04 编排器），且不污染原始镜像。
        # v0.8：启动盘 = virtio-blk（legacy），与 kernel/src/driver/virtio_blk.rs 对应。
        "-drive", f"if=none,id=blk0,format=raw,file={disk},snapshot=on",
        "-device", "virtio-blk-pci,drive=blk0,disable-modern=on",
        "-serial", "stdio",
        "-monitor", f"unix:{monitor},server,nowait",
        "-display", "none",
        "-no-reboot",
    ]

    print(f"running: {' '.join(cmd)}")
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    capture = SerialCapture(proc.stdout)

    injection_error: str | None = None
    try:
        if capture.wait_for(ready_marker, args.timeout):
            send_keys(monitor, keys)
        else:
            injection_error = f"serial never reached {ready_marker!r}"
    except Exception as err:  # noqa: BLE001 - 报告为测试失败原因
        injection_error = f"key injection failed: {err}"

    # 给内核一点时间处理并回显
    time.sleep(1.0)

    proc.terminate()
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=10)

    output = capture.snapshot()
    serial_log.write_bytes(output)

    missing = [exp for exp in expects if exp.encode() not in output]

    # 正则断言：在整份日志上 re.search（DOTALL），因此既能跨行，也能用反向引用
    # 把「同一个值」对齐到多处输出（例如 PID 在 fork/exec/wait4 三处必须一致）。
    text = output.decode("utf-8", "replace")
    missing_re: list[str] = []
    for pattern in regexes:
        try:
            if re.search(pattern, text, re.DOTALL) is None:
                missing_re.append(pattern)
        except re.error as err:  # 正则本身写错也是测试失败，不能静默通过
            missing_re.append(f"{pattern} (invalid regex: {err})")

    if not missing and not missing_re and injection_error is None:
        print(
            f"PASS: all {len(expects)} expectation(s) and {len(regexes)} regex(es) "
            f"found on serial (log: {serial_log})"
        )
        return 0

    if injection_error:
        print(f"FAIL: {injection_error}")
    for exp in missing:
        print(f"FAIL: {exp!r} not found on serial (log: {serial_log})")
    for pattern in missing_re:
        print(f"FAIL: /{pattern}/ did not match serial (log: {serial_log})")
    tail = output[-4000:]
    print("--- serial tail ---")
    sys.stdout.buffer.write(tail)
    sys.stdout.buffer.flush()
    print()
    return 1


if __name__ == "__main__":
    sys.exit(main())
