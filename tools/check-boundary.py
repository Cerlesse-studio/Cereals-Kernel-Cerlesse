#!/usr/bin/env python3
"""内核 / 用户态边界门禁（见 docs/pure-kernel.md 阶段二）。

**为什么需要它**：v0.9 之前，内核构建期会用嵌套 cargo 构建 7 个用户程序、
再用 `include_bytes!` 把它们的 ELF 内嵌进内核映像（`kernel/build.rs` 约 300 行），
于是「删掉 `libc/`、`user/` 之后内核就编不出来」。本脚本把「内核不依赖用户态」
从一句约定变成**可执行的检查**：任何一次提交只要把这种依赖加回来，CI 就会红。

检查项（全部针对内核侧；`userland/` 里的东西不在此列）：

| 编号 | 断言 | 为什么 |
| ---- | ---- | ---- |
| K1 | `kernel/build.rs` 不出现 `Command::new(` | 内核构建不得再调用 cargo / as / ld |
| K2 | `kernel/src/**/*.rs` 不出现 `include_bytes!` | 程序字节只能通过生成的清单 `include!` 进来（空清单也合法） |
| K3 | 内核源码与 `kernel/build.rs` 的**代码**里不出现 `user/`、`libc/`、`../libc` 路径（注释/文档里的说明允许） | 内核代码不引用用户态源码位置 |
| K4 | `kernel/Cargo.toml` 的依赖只有 `shared` / `abi` | 内核不得依赖用户态 crate |
| K5 | 根 `Cargo.toml` 的 `members` 不含 `libc`、`user/*`，且 `exclude` 声明了 `userland` | 内核 workspace 必须能独立加载 |
| K6 | 内核侧不出现用户态包名与 profile（`user-hello` … `user-ps`、`user-release`、`user-target`） | 内核不构建用户态包 |

用法：

```bash
python3 tools/check-boundary.py            # 检查当前仓库，违规即退出码 1
python3 tools/check-boundary.py --selftest # 反向自检：注入 6 类违规样本，必须全部被抓到
```

`--selftest` 保证门禁自身不是「永远 PASS」的空壳 —— 与 `tools/mkfs` 用 mtools 复核、
`dyn-hostcheck` 用 glibc 复核是同一个思路。
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import sys
import tempfile
from pathlib import Path

# 内核侧允许的依赖（见 docs/pure-kernel.md 2 节目标结构）
ALLOWED_KERNEL_DEPS = {"shared", "abi"}
ALLOWED_ROOT_MEMBERS = {"shared", "boot", "abi", "kernel"}

USERLAND_PATH_PATTERNS = [
    re.compile(r"(?<![\w./-])\.\./libc\b"),
    re.compile(r"(?<![\w./-])user/"),
    re.compile(r"(?<![\w./-])libc/"),
]
# 用户态包名 / profile / 构建目录（精确列出：`user-mode run`、`usr-mode` 这类不是包名）
USERLAND_PACKAGES = (
    "user-hello",
    "user-shell",
    "user-hello-bin",
    "user-ls",
    "user-cat",
    "user-echo",
    "user-ps",
    "user-release",
    "user-target",
)


def is_comment(line: str) -> bool:
    """注释行：`//`（含 `//!`/`///`）、`#`（build.rs 的 cargo: 注释）、块注释续行。"""
    s = line.strip()
    return s.startswith(("//", "#", "*", "/*"))


def kernel_sources(root: Path) -> list[Path]:
    src = root / "kernel" / "src"
    return sorted(p for p in src.rglob("*.rs")) if src.is_dir() else []


def _read(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return ""


def check_k1_build_script_no_commands(root: Path) -> list[str]:
    build = root / "kernel" / "build.rs"
    if not build.is_file():
        return []
    text = _read(build)
    return [
        f"kernel/build.rs:{n}: 内核构建脚本调用了外部命令（Command::new）：{line.strip()}"
        for n, line in enumerate(text.splitlines(), 1)
        if "Command::new(" in line
    ]


def check_k2_no_embedded_bytes(root: Path) -> list[str]:
    out = []
    for path in kernel_sources(root):
        for n, line in enumerate(_read(path).splitlines(), 1):
            if "include_bytes!" in line:
                out.append(
                    f"{path.relative_to(root)}:{n}: 内核源码直接内嵌字节（include_bytes!）：{line.strip()}"
                )
    return out


def check_k3_no_userland_paths(root: Path) -> list[str]:
    """只看**代码**：注释里写「见 user/hello」是文档引用，不是构建依赖。"""
    out = []
    targets = kernel_sources(root) + [root / "kernel" / "build.rs"]
    for path in targets:
        if not path.is_file():
            continue
        for n, line in enumerate(_read(path).splitlines(), 1):
            if is_comment(line):
                continue
            for pat in USERLAND_PATH_PATTERNS:
                if pat.search(line):
                    out.append(
                        f"{path.relative_to(root)}:{n}: 内核代码里出现用户态路径（{pat.pattern}）：{line.strip()}"
                    )
                    break
    return out


def _toml_section(text: str, name: str) -> list[str]:
    """返回 `[name]` 段到下一个 `[` 段之间的行（够用即可，不引第三方解析器）。"""
    lines, inside = [], False
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            inside = stripped[1:-1] == name
            continue
        if inside:
            lines.append(line)
    return lines


def check_k4_kernel_deps(root: Path) -> list[str]:
    manifest = root / "kernel" / "Cargo.toml"
    if not manifest.is_file():
        return []
    deps = set()
    for line in _toml_section(_read(manifest), "dependencies"):
        stripped = line.strip()
        if stripped and not stripped.startswith("#") and "=" in stripped:
            deps.add(stripped.split("=", 1)[0].strip())
    extra = sorted(d for d in deps if d not in ALLOWED_KERNEL_DEPS)
    return [f"kernel/Cargo.toml: 依赖 {e}（只允许 {sorted(ALLOWED_KERNEL_DEPS)}）" for e in extra]


def check_k5_root_workspace(root: Path) -> list[str]:
    manifest = root / "Cargo.toml"
    if not manifest.is_file():
        return []
    text = _read(manifest)
    members = _array_items(text, "members")
    out = []
    for m in members:
        base = m.split("/")[0]
        if base not in ALLOWED_ROOT_MEMBERS:
            out.append(f"Cargo.toml: members 含 {m!r}（内核 workspace 只允许 {sorted(ALLOWED_ROOT_MEMBERS)}）")
    if "userland" not in _array_items(text, "exclude"):
        out.append("Cargo.toml: exclude 必须包含 \"userland\"（用户态是独立 workspace）")
    return out


def _array_items(text: str, key: str) -> list[str]:
    """取 `key = [ ... ]` 里的字符串项（单行或多行）。"""
    match = re.search(rf"^\s*{key}\s*=\s*\[(.*?)\]", text, re.S | re.M)
    if not match:
        return []
    return re.findall(r'"([^"]+)"', match.group(1))


def check_k6_no_userland_packages(root: Path) -> list[str]:
    out = []
    for path in kernel_sources(root) + [root / "kernel" / "build.rs", root / "kernel" / "Cargo.toml"]:
        if not path.is_file():
            continue
        for n, line in enumerate(_read(path).splitlines(), 1):
            if is_comment(line):
                continue
            for name in USERLAND_PACKAGES:
                if name in line:
                    out.append(
                        f"{path.relative_to(root)}:{n}: 内核代码里出现用户态包名 {name!r}：{line.strip()}"
                    )
                    break
    return out


CHECKS = [
    ("K1 内核构建脚本不调用外部命令", check_k1_build_script_no_commands),
    ("K2 内核源码不直接内嵌字节", check_k2_no_embedded_bytes),
    ("K3 内核侧不出现用户态路径", check_k3_no_userland_paths),
    ("K4 内核依赖只有 shared/abi", check_k4_kernel_deps),
    ("K5 根 workspace 不含用户态成员", check_k5_root_workspace),
    ("K6 内核侧不出现用户态包名", check_k6_no_userland_packages),
]


def run(root: Path) -> int:
    failed = 0
    for name, fn in CHECKS:
        violations = fn(root)
        if violations:
            failed += 1
            print(f"FAIL  {name}")
            for v in violations:
                print(f"      {v}")
        else:
            print(f"PASS  {name}")
    print("")
    if failed:
        print(f"check-boundary FAIL（{failed}/{len(CHECKS)} 项违规）")
        print("内核只提供机制：用户态由 userland/ 独立构建，见 docs/pure-kernel.md")
        return 1
    print(f"check-boundary PASS（{len(CHECKS)}/{len(CHECKS)} 项）")
    return 0


# --------------------------------------------------------------------------
# --selftest：注入违规样本，6 项检查必须全部报错（否则门禁是空的）
# --------------------------------------------------------------------------

SAMPLES = {
    "kernel/build.rs": (
        "fn main() {\n"
        "    let status = std::process::Command::new(\"cargo\").status().unwrap();\n"
        "    let _path = \"user/hello\";\n"
        "    let _pkg = \"user-hello\";\n"
        "}\n"
    ),
    "kernel/src/selftest_violation.rs": (
        "static X: &[u8] = include_bytes!(\"../user/hello/target/user-hello\");\n"
        "const P: &str = \"../../libc/src/lib.rs\";\n"
    ),
    "kernel/Cargo.toml": "[package]\nname = \"kernel\"\n\n[dependencies]\nshared = { path = \"../shared\" }\nlibc = { path = \"../libc\" }\n",
    "Cargo.toml": "[workspace]\nmembers = [\"shared\", \"boot\", \"kernel\", \"libc\", \"user/hello\"]\n",
}


def selftest() -> int:
    tmp = Path(tempfile.mkdtemp(prefix="cerlesse-boundary-"))
    try:
        for rel, content in SAMPLES.items():
            path = tmp / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content, encoding="utf-8")
        caught, missed = [], []
        for name, fn in CHECKS:
            (caught if fn(tmp) else missed).append(name)
        print(f"selftest root: {tmp}")
        for name in caught:
            print(f"  caught  {name}")
        for name in missed:
            print(f"  MISSED  {name}  <-- 该检查对违规样本没有反应")
        if missed:
            print(f"\ncheck-boundary --selftest FAIL（{len(missed)} 项检查抓不到违规样本）")
            return 1
        print(f"\ncheck-boundary --selftest PASS（{len(caught)}/{len(CHECKS)} 项检查全部抓到违规）")
        return 0
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def main() -> int:
    parser = argparse.ArgumentParser(description="内核 / 用户态边界门禁")
    parser.add_argument("--root", default=None, help="仓库根（默认取脚本所在仓库）")
    parser.add_argument("--selftest", action="store_true", help="反向自检：注入违规样本，必须全部被抓到")
    args = parser.parse_args()

    if args.selftest:
        return selftest()
    root = Path(args.root).resolve() if args.root else Path(__file__).resolve().parent.parent
    print(f"check-boundary: root={root}")
    return run(root)


if __name__ == "__main__":
    sys.exit(main())
