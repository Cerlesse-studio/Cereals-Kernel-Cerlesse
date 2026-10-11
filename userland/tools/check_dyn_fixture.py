#!/usr/bin/env python3
"""动态链接夹具的结构自检（`userland/user/dynhello/`）。

这一组夹具是 `as`/`ld` 现编的（`userland/Makefile` 的 `dyn` 目标），内核按
`PT_INTERP` / `DT_NEEDED` 里的**名字**去 VFS 找解释器与共享库。链接命令一旦改坏，
症状会一直拖到 QEMU 里才暴露，因此在这里就把结构断言掉：

| 断言 | 为什么 |
| ---- | ---- |
| `\\x7fELF` 魔数 | 产物真的是 ELF |
| `e_type == ET_DYN (3)` | 必须是 PIE/共享对象（缺 `-pie` 时是 ET_EXEC，内核的 ET_DYN 分流不会走） |
| 有 `PT_INTERP` | 缺 `--dynamic-linker=` 时内核解析不到解释器路径 |
| `.dynamic` 里有 `DT_NEEDED` | 缺 `-lgreet` 时没有共享库依赖，跨对象重定位不会发生 |

纯标准库、只读文件、**不启动任何东西**：这是构建期检查，运行期证据由
`make test-dyn`（QEMU）与 `make dyn-hostcheck`（宿主机 glibc）给出。
"""

from __future__ import annotations

import sys
from pathlib import Path

ET_DYN = 3
PT_INTERP = 3
PT_DYNAMIC = 2
DT_NEEDED = 1


def u16(buf: bytes, off: int) -> int:
    return int.from_bytes(buf[off : off + 2], "little")


def u32(buf: bytes, off: int) -> int:
    return int.from_bytes(buf[off : off + 4], "little")


def u64(buf: bytes, off: int) -> int:
    return int.from_bytes(buf[off : off + 8], "little")


def program_headers(elf: bytes):
    phoff = u64(elf, 32)
    phentsize = u16(elf, 54)
    phnum = u16(elf, 56)
    for i in range(phnum):
        yield phoff + i * phentsize


def has_program_header(elf: bytes, want: int) -> bool:
    return any(u32(elf, ph) == want for ph in program_headers(elf))


def has_dynamic_tag(elf: bytes, want: int) -> bool:
    for ph in program_headers(elf):
        if u32(elf, ph) != PT_DYNAMIC:
            continue
        off = u64(elf, ph + 8)
        size = u64(elf, ph + 32)
        at = off
        while at + 16 <= off + size:
            tag = u64(elf, at)
            if tag == want:
                return True
            if tag == 0:
                break
            at += 16
    return False


def check(path: Path) -> list[str]:
    elf = path.read_bytes()
    problems = []
    if len(elf) <= 64 or elf[:4] != b"\x7fELF":
        return [f"{path}: 不是 ELF 文件（{len(elf)} 字节）"]
    if u16(elf, 16) != ET_DYN:
        problems.append(f"{path}: e_type={u16(elf, 16)}，应为 ET_DYN(3)（链接命令缺 -pie？）")
    if not has_program_header(elf, PT_INTERP):
        problems.append(f"{path}: 缺 PT_INTERP（链接命令缺 --dynamic-linker=？）")
    if not has_dynamic_tag(elf, DT_NEEDED):
        problems.append(f"{path}: .dynamic 里没有 DT_NEEDED（链接命令缺 -lgreet？）")
    return problems


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print(__doc__)
        return 2
    path = Path(argv[1])
    if not path.is_file():
        print(f"check_dyn_fixture: 找不到 {path}（先跑 `make dyn`）")
        return 2
    problems = check(path)
    if problems:
        print("check_dyn_fixture FAIL")
        for p in problems:
            print(f"  - {p}")
        return 1
    print(f"check_dyn_fixture PASS: {path.name} ET_DYN + PT_INTERP + DT_NEEDED")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
