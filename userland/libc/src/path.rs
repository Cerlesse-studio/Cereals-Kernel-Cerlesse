//! Windows 式路径 → POSIX 路径（**CSS-3**，`docs/storage.md` 4.2）
//!
//! 内核的 syscall **只接受 POSIX 绝对路径**；`C:\Projects\demo.rs` 这种写法的解析
//! 落在**用户态库**里，流程是固定的：
//!
//! ```text
//! C:\KERNEL.ELF
//!    ↓ 识别「<字母>:」（本模块）
//!    ↓ 查卷表（storage_volumes，**不硬编码字母**）
//!    ↓ 得到该卷的默认挂载点：/volumes/<label>（卷标为空则 /volumes/<vol_id>）
//! /volumes/NO_NAME/KERNEL.ELF
//!    ↓ VFS 最长前缀路由（fs/mount.rs，内核侧）
//! 后端（FAT/…）访问文件
//! ```
//!
//! 三条理由（写下来避免以后又把盘符塞回内核）：
//!
//! 1. **syscall ABI 不变**：路径参数仍是内核认识的绝对路径，Windows 式写法只是库的便利层；
//! 2. **不破坏 POSIX 视图**：`/volumes/…` 直接可用，不依赖任何 Windows 语法；
//! 3. **字母表是内核维护、库只查询**：这里不缓存硬编码映射，`storage_volumes` 是唯一来源
//!    （内核改了字母，下一次调用立刻生效）。
//!
//! 现状（CSS-3）：系统卷（启动盘）的盘符是 `C`，它的默认挂载点在 `docs/storage.md` 4.3 里
//! 是 `/`（保留），但**当前根是 RAMFS、不是卷**，因此本模块对所有卷统一用
//! `/volumes/<label|vol_id>` —— 启动盘在 `/disk` 之外也会被挂到自己的 `/volumes/<label>`
//! （由 `volumes demo` / `storage_mount` 显式建立）。这条约定写在 `storage.md` 的 CSS-3 落地清单里。

use crate::syscall::{
    storage_volumes, volume_id, volume_label, volume_letter, VOLUME_RECORD_SIZE,
};

/// 卷表缓冲容量（内核 `storage::MAX_VOLUMES` = 16 条，一次调用装下整张表）。
const TABLE_BYTES: usize = 16 * VOLUME_RECORD_SIZE;

/// `-EINVAL`：不是 Windows 式路径，或输出缓冲装不下。
pub const EINVAL: isize = -22;
/// `-ENOENT`：那个盘符没有对应的卷（字母未分配 / 卷不在表里）。
pub const ENOENT: isize = -2;

/// 是否是 Windows 式路径（`<字母>:` 开头）。
///
/// 只认**一个字母 + 冒号**，与 `docs/storage.md` 4.2 一致；`::`、`C`（没有冒号）、
/// `CC:` 都不算 —— 「看起来像但不是」必须明确拒绝，而不是猜一个含义。
pub fn is_windows(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// 把 Windows 式路径解析成 POSIX 路径，写进 `out`，返回长度。
///
/// 规则（必须确定性）：
///
/// - `C:` → `/volumes/<label|vol_id>`（挂载点本身）；
/// - `C:\a\b` / `C:/a/b` → `/volumes/<label|vol_id>/a/b`（分隔符统一成 `/`，
///   重复分隔符折叠）；
/// - 字母大小写不敏感（`c:` == `C:`），卷表里存的是大写；
/// - 该字母没有对应卷 → [`ENOENT`]；不是 Windows 式路径或缓冲太小 → [`EINVAL`]。
///
/// 为什么用**默认挂载点规则**而不是查询「这个卷现在挂在哪」：挂载点是内核的运行时状态，
/// 而用户态库只有一个只读的卷表（4.2 的第三条理由）；4.3 规定了默认挂载点怎么算，
/// 因此规则本身就是可复述的、不依赖任一时刻的挂载表。
pub fn resolve(windows: &str, out: &mut [u8]) -> Result<usize, isize> {
    if !is_windows(windows) {
        return Err(EINVAL);
    }
    let mut bytes = windows.as_bytes();
    let letter = bytes[0].to_ascii_uppercase();
    bytes = &bytes[2..];

    // 卷表（只读快照；本函数不做缓存 —— 字母随时可能被管理员改）
    let mut table = [0u8; TABLE_BYTES];
    let count = storage_volumes(&mut table, None);
    if count < 0 {
        return Err(count);
    }
    let mut base: Option<(usize, usize)> = None; // (记录下标, …)
    for i in 0..count as usize {
        if volume_letter(&table, i) == letter {
            base = Some((i, 0));
            break;
        }
    }
    let Some((index, _)) = base else {
        return Err(ENOENT);
    };

    // `/volumes/` + 卷标（为空则退化到 vol_id —— 4.3 的退化规则）
    let label = volume_label(&table, index);
    let name = if label.is_empty() {
        volume_id(&table, index)
    } else {
        label
    };
    if name.is_empty() {
        return Err(ENOENT);
    }

    let mut written = 0usize;
    let name = name.as_bytes();
    append(b"/volumes/", out, &mut written)?;
    append(name, out, &mut written)?;
    // 剩余部分：分隔符统一，重复分隔符折叠（与 `vfs::normalize` 同口径的宽松输入）
    let mut rest = bytes;
    while let Some((&first, tail)) = rest.split_first() {
        if first == b'\\' || first == b'/' {
            rest = tail;
        } else {
            break;
        }
    }
    if !rest.is_empty() {
        append(b"/", out, &mut written)?;
        let start = written;
        for &byte in rest {
            let byte = if byte == b'\\' { b'/' } else { byte };
            append(&[byte], out, &mut written)?;
        }
        // 尾部分隔符/重复分隔符折叠：`C:\a\\` → `/volumes/X/a`（与 `vfs::normalize`
        // 同口径：把宽松的输入规范成一条路径，而不是原样交给内核去猜）
        let mut end = written;
        while end > start + 1 && out[end - 1] == b'/' {
            end -= 1;
        }
        if end > start && out[end - 1] == b'/' {
            end -= 1;
        }
        written = end;
    }
    Ok(written)
}

/// 便捷形式：解析成 `&str` 视图（缓冲由调用方提供）。
pub fn resolve_str<'a>(windows: &str, buf: &'a mut [u8]) -> Result<&'a str, isize> {
    let len = resolve(windows, buf)?;
    core::str::from_utf8(&buf[..len]).map_err(|_| EINVAL)
}

/// 往输出缓冲追加一段字节（装不下就是 [`EINVAL`] —— 不截断路径）。
fn append(text: &[u8], out: &mut [u8], written: &mut usize) -> Result<(), isize> {
    if *written + text.len() > out.len() {
        return Err(EINVAL);
    }
    out[*written..*written + text.len()].copy_from_slice(text);
    *written += text.len();
    Ok(())
}
