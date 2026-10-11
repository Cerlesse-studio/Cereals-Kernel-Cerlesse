//! 内核构建脚本。
//!
//! v0.1：把 `link.ld` 交给内核二进制链接（绝对路径避免 rustc 工作目录差异）。
//!
//! v0.6 追加**用户态脚手架**的构建；v0.7 追加 ELF 校验与内嵌：
//!
//! 1. 用嵌套 cargo 构建每个内嵌用户程序（`user/hello`、`user/shell`，见 workspace `Cargo.toml`）；
//! 2. **校验**它们的 ELF（链接基址 / 段窗口 / 页对齐 / 无可装载重定位），
//!    这些断言属于「错了就必然跑不起来」的前置条件，放在构建期比运行期报错更早、
//!    信息更全；
//! 3. 把 ELF **原样**写入 `$OUT_DIR/<name>.elf`，内核用 `include_bytes!` 内嵌，
//!    在启动时写进 RAMFS（`/init`、`/bin/sh`），再由内核自己的 ELF Loader 装载
//!    （`syscall/elf.rs`）—— v0.6 的「构建期摊平成连续映像」在 v0.7 已被替换。
//!
//! 为什么在 build.rs 里构建而不是交给 Makefile：这样 `cargo build -p kernel`
//! 自身就是完整的（不需要「先 make user 再 make kernel」的隐式顺序），
//! 也不会出现「用户程序改了但内核没重编，于是装载了旧映像」的偏差 ——
//! 用户程序源码通过 `cargo:rerun-if-changed` 参与依赖跟踪。

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// 用户程序链接/装载基址：PML4 下标 1。
///
/// 必须与 `kernel/src/syscall/address_space.rs::USER_VIRT_BASE` 一致 ——
/// ELF 的每个 `PT_LOAD` 都必须落在 `[USER_IMAGE_BASE, +USER_IMAGE_MAX_BYTES)`。
const USER_IMAGE_BASE: u64 = 0x0000_0080_0000_0000;
/// 用户映像窗口大小（必须与 `address_space.rs::USER_IMAGE_MAX` 一致：16 页 = 64 KiB）。
const USER_IMAGE_MAX_BYTES: u64 = 16 * 4096;

/// 内嵌的用户程序：`(cargo 包名, $OUT_DIR 下的 ELF 文件名)`。
///
/// 每个程序在 RAMFS 上的落点与运行时机见 `syscall/user.rs`：
/// - `user-hello` → `/init`：v0.7 起的验收基准（文件 syscall / 用户堆 / 退出码 42）；
/// - `user-shell` → `/bin/sh`：v0.9 的交互式 Shell（键盘行缓冲 → `stdin` → 内建命令）；
/// - `user-hello-bin` → `/bin/hello`：v0.9 第三步的**外部程序** —— 它不由内核直接运行，
///   而是被 Shell 用 `fork` + `execve` 起来（`make test-shell` 断言这条路径）。
const USER_PROGRAMS: [(&str, &str); 3] = [
    ("user-hello", "user_hello.elf"),
    ("user-shell", "user_shell.elf"),
    ("user-hello-bin", "user_hello_bin.elf"),
];
/// 用户程序使用的 cargo profile（workspace 根定义；`lto = false`，见其注释）。
const USER_PROFILE: &str = "user-release";
/// 用户程序必须用 large 代码模型：链接地址 2^39 超出 small 模型 ±2GiB 的
/// 绝对寻址范围（否则链接期就报 R_X86_64_32S out of range）。
/// 该标志与 `Makefile` 的 `user` 目标成对维护，改一处要同步另一处。
const USER_RUSTFLAGS: &str = "-C relocation-model=static -C code-model=large";

fn main() {
    let dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    println!("cargo:rustc-link-arg-bins=-T{dir}/link.ld");
    println!("cargo:rerun-if-changed=link.ld");

    let root = Path::new(&dir)
        .parent()
        .expect("kernel/ 应位于 workspace 根之下")
        .to_path_buf();
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));

    for (package, elf_file) in USER_PROGRAMS {
        build_user_program(&root, &out_dir, package);

        let elf_path = out_dir
            .join("user-target")
            .join("x86_64-unknown-none")
            .join(USER_PROFILE)
            .join(package);
        let elf = fs::read(&elf_path)
            .unwrap_or_else(|err| panic!("读取用户程序 ELF 失败 {}: {err}", elf_path.display()));

        let image = validate_image(&elf, package);
        // 布局事实只用于把「校验过了什么」写进构建脚本日志（`cargo build -vv` 可见），
        // 不打印 warning：它会随每次构建重复出现，而运行期串口已有更完整的轨迹
        // （`user: program=/init source=elf ... segments=4 image_pages=4`）。
        assert!(image.segments >= 1, "{package}: 没有可装载段");
        assert!(
            image.end > USER_IMAGE_BASE && image.end <= USER_IMAGE_BASE + USER_IMAGE_MAX_BYTES,
            "{package}: 映像结束地址 {:#x} 超出用户映像窗口",
            image.end
        );

        let elf_out = out_dir.join(elf_file);
        fs::write(&elf_out, &elf)
            .unwrap_or_else(|err| panic!("写入用户程序 ELF 失败 {}: {err}", elf_out.display()));
    }

    // 用户程序源码变化时重跑本脚本（否则内核会装载旧映像）。
    for watched in [
        "Cargo.toml",
        "libc/Cargo.toml",
        "libc/src",
        "user/hello/Cargo.toml",
        "user/hello/src",
        "user/hello/user.ld",
        "user/shell/Cargo.toml",
        "user/shell/src",
        "user/shell/user.ld",
        "user/hello-bin/Cargo.toml",
        "user/hello-bin/src",
        "user/hello-bin/user.ld",
    ] {
        println!("cargo:rerun-if-changed={}", root.join(watched).display());
    }
}

/// 构建一个用户程序（独立 target 目录，避免与外层构建抢 target 锁）。
fn build_user_program(root: &Path, out_dir: &Path, package: &str) {
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let status = Command::new(cargo)
        .current_dir(root)
        .args([
            "build",
            "-p",
            package,
            "--target",
            "x86_64-unknown-none",
            "--profile",
            USER_PROFILE,
            // 用户程序及其依赖全在 workspace 内（无 registry 依赖），离线可确定地构建
            "--offline",
        ])
        .arg("--target-dir")
        .arg(out_dir.join("user-target"))
        .env("RUSTFLAGS", USER_RUSTFLAGS)
        // 外层 cargo 会把「本包自己的 rustflags」以 CARGO_ENCODED_RUSTFLAGS 注入
        // build script 的环境，而它的优先级**高于** RUSTFLAGS —— 不移除就会用
        // 内核的 rustflags（无 code-model=large）去编用户程序，链接期必然报
        // R_X86_64_32S out of range。
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .status()
        .expect("无法启动 cargo 构建用户程序");
    assert!(
        status.success(),
        "用户程序构建失败（{package}，profile {USER_PROFILE}）"
    );
}

/// 校验结果（打印用）。
struct ImageInfo {
    segments: usize,
    end: u64,
}

/// 校验用户程序 ELF：段窗口、入口、页对齐、无可装载重定位。
///
/// 这些条件在 v0.7 由运行期装载器（`syscall/elf.rs`）**同时**检查（拒绝时打印
/// `user: ELF rejected: ...`）。构建期再断言一次的理由：这些错误是「链接脚本被改坏」
/// 一类的问题，在编译输出里报出来远比等到 QEMU 里发现「用户程序没跑」直观。
fn validate_image(elf: &[u8], package: &str) -> ImageInfo {
    assert!(elf.len() > 64 && &elf[..4] == b"\x7fELF", "{package}: 不是 ELF 文件");
    // ELF64 头：e_type = u16@16（ET_EXEC = 2），e_machine = u16@18（EM_X86_64 = 62）
    assert_eq!(u16_at(elf, 16), 2, "{package}: 不是可执行 ELF（ET_EXEC）");
    assert_eq!(u16_at(elf, 18), 62, "{package}: 不是 x86_64 ELF");

    let entry = u64_at(elf, 24);
    let phoff = u64_at(elf, 32) as usize;
    let phentsize = u16_at(elf, 54) as usize;
    let phnum = u16_at(elf, 56) as usize;

    // (vaddr, memsz, flags)
    let mut loads: Vec<(u64, u64, u32)> = Vec::new();
    for i in 0..phnum {
        let ph = phoff + i * phentsize;
        if u32_at(elf, ph) != 1 {
            continue; // 只要 PT_LOAD
        }
        loads.push((
            u64_at(elf, ph + 16), // p_vaddr
            u64_at(elf, ph + 40), // p_memsz
            u32_at(elf, ph + 4),  // p_flags
        ));
    }
    assert!(!loads.is_empty(), "{package}: 没有 PT_LOAD 段");

    let base = loads.iter().map(|seg| seg.0).min().expect("PT_LOAD");
    assert_eq!(
        base, USER_IMAGE_BASE,
        "{package}: 链接基址 {base:#x} != USER_IMAGE_BASE {USER_IMAGE_BASE:#x}（检查 user.ld）"
    );
    let end = loads.iter().map(|seg| seg.0 + seg.1).max().expect("PT_LOAD");
    assert!(
        end <= USER_IMAGE_BASE + USER_IMAGE_MAX_BYTES,
        "{package}: 映像 {:#x} 超出窗口（{} 字节，见 address_space.rs::USER_IMAGE_MAX）",
        end,
        USER_IMAGE_MAX_BYTES
    );
    assert!(
        entry >= base && entry < end,
        "{package}: 入口 {entry:#x} 不在映像范围内 [{base:#x}, {end:#x})"
    );

    // 段页对齐：ELF Loader 按页设置权限，同页既 W 又 X 会让 W^X 退化，
    // 运行期会直接拒绝（`user: ELF rejected: W and X segments share a page`）。
    // 起始地址页对齐（`memsz` 不必对齐：`.data`+`.bss` 合并成一个 RW 段时尾部
    // 会落在半页上，装载器按 `align_up` 映射并把余下部分置 0）。
    for (vaddr, memsz, _flags) in &loads {
        assert_eq!(
            vaddr % 4096,
            0,
            "{package}: PT_LOAD {vaddr:#x} 未页对齐（user.ld 里每个段需要 ALIGN(0x1000)）"
        );
        assert!(*memsz > 0, "{package}: PT_LOAD {vaddr:#x} 的 memsz 为 0");
    }
    // 同一页不允许同时出现 W 与 X 段
    for (i, &(vaddr, memsz, flags)) in loads.iter().enumerate() {
        if flags & 0x2 == 0 {
            continue;
        }
        let stop = vaddr + memsz;
        for &(other_vaddr, other_memsz, other_flags) in loads.iter().skip(i + 1) {
            if other_flags & 0x1 == 0 {
                continue;
            }
            let o_stop = other_vaddr + other_memsz;
            assert!(
                !(vaddr < o_stop && other_vaddr < stop),
                "{package}: W 段与 X 段落在同一页（{vaddr:#x}/{other_vaddr:#x}）：ELF Loader 会拒绝"
            );
        }
    }

    assert_alloc_free(elf, package);

    ImageInfo {
        segments: loads.len(),
        end,
    }
}

/// 断言 ELF 中没有「可装载段里的重定位」。
///
/// 内核不做加载时重定位（`ET_EXEC` + 固定链接地址），带 `SHF_ALLOC` 的重定位段
/// 一旦出现，程序里的地址就会停留在链接期之外的值上，运行期表现是不可预期的崩溃。
/// 运行期装载器同样会拒绝（`user: ELF rejected: loadable relocations present`）。
fn assert_alloc_free(elf: &[u8], package: &str) {
    let shoff = u64_at(elf, 40) as usize;
    let shentsize = u16_at(elf, 58) as usize;
    let shnum = u16_at(elf, 60) as usize;
    for i in 0..shnum {
        let sh = shoff + i * shentsize;
        let sh_type = u32_at(elf, sh + 4);
        let sh_flags = u64_at(elf, sh + 8);
        if (sh_type == 4 || sh_type == 9) && sh_flags & 0x2 != 0 {
            panic!(
                "{package}: 含有可装载重定位（SHT_RELA/SHT_REL + SHF_ALLOC）：内核不做加载时重定位"
            );
        }
    }
}

fn u16_at(buf: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([buf[off], buf[off + 1]])
}

fn u32_at(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

fn u64_at(buf: &[u8], off: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&buf[off..off + 8]);
    u64::from_le_bytes(bytes)
}
