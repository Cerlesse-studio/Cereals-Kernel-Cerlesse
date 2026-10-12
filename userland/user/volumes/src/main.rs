//! `volumes` —— 卷表与盘符的消费者（CSS-1 基础程序，**CSS-3 扩成存储管理命令**；
//! RAMFS 上落在 `/bin/volumes`）
//!
//! 它只需要内核的三类能力：`storage_volumes`（261，只读）、`storage_mount`/`storage_unmount`
//! （262/263）与 `storage_assign_letter`（264，都需要 `CAP_STORAGE_ADMIN`，见
//! `docs/ckm.md` 3.4 判定点 8）。与 `ps` 是同一套做法：记录定长（64 字节/条），
//! 布局的权威定义在内核（`kernel/src/storage/volume.rs::write_record`），libc 提供
//! `volume_*` 解析器。
//!
//! 为什么值得单独做一个程序：**记录布局是 ABI**。内核自测只能证「两次快照一模一样」，
//! 证明不了「消费者按这个布局读得到东西」——`volumes` 就是那个消费者：偏移一旦读错
//! （比如把 `fs_kind` 当成 `read_only`、把盘符当成卷标），输出字段立刻对不上。
//!
//! 子命令（CSS-3；无参数时保持 CSS-1 的行为：打印卷表）：
//!
//! | 命令 | 做什么 | 需要的能力 |
//! | ---- | ---- | ---- |
//! | `volumes` | 打印卷表（含 `letter=` 一列） | 无（只读） |
//! | `volumes demo` | 把 `C:` 那个卷挂到它的默认挂载点，用**两个入口**读同一段字节，再卸载 | `CAP_STORAGE_ADMIN` |
//! | `volumes mount <C:> <point>` | `storage_mount`（受控动作） | `CAP_STORAGE_ADMIN` |
//! | `volumes unmount <point>` | `storage_unmount`（受控动作） | `CAP_STORAGE_ADMIN` |
//! | `volumes letter <C:> <A-Z\|0>` | 分配/取消盘符（写 CKM 状态区） | `CAP_STORAGE_ADMIN` |
//!
//! 输出形状（字段顺序 = 记录里的顺序，便于逐项比对；空卷标打 `-`）：
//!
//! ```text
//! volumes: id disk part fs label label_trunc letter ro health bytes free_known free mounts
//! volumes: fs:1a2b3c4d disk=0 part=0 fs=fat16 label=NO_NAME label_trunc=0 letter=C ro=ro-backend health=ok bytes=66060288 free_known=0 free=0 mounts=0
//! volumes: 1 volume(s) (total=1)
//! ```
//!
//! 刻意保留的几处区分（`docs/storage.md` 3.5/3.6）：
//!
//! - `fs=unsupported`（没认出格式）与 `health=unsupported`（没有后端）不是一回事，
//!   因此 `fs=ext4 health=unsupported` 是合法组合；
//! - `label_trunc=1` 表示 11 字节装不下、卷标被截过（不许静默截断）；
//! - `free_known=0` 表示可用容量**未知** —— 不用 0 冒充「盘满」；
//! - `letter=-` 表示**没有盘符**：卷在、能挂，只是没有字母（与「卷不在」区分）。

#![no_std]
#![no_main]

use libc::{
    arg_str, close, open, println, read, resolve_str, storage_assign_letter, storage_mount,
    storage_unmount, storage_volumes, volume_disk, volume_free_bytes, volume_fs, volume_health,
    volume_id, volume_label, volume_label_truncated, volume_letter, volume_letter_str,
    volume_mounts, volume_part, volume_read_only, volume_total_bytes, MAX_VOLUMES, O_RDONLY,
    STORAGE_LETTER_NONE, VOLUME_RECORD_SIZE,
};

/// 卷表容量（内核 `storage::MAX_VOLUMES` = 16）—— 一次调用就能装下整张表。
const TABLE_BYTES: usize = MAX_VOLUMES * VOLUME_RECORD_SIZE;
/// 短名字缓冲（挂载点 `/volumes/<11 字节卷标>` / 卷标识 15 字节）。
const NAME_MAX: usize = 32;
/// 路径重写缓冲（`/volumes/<name>/<剩余部分>`）。
const PATH_MAX: usize = 128;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match arg_str(1) {
        None => list(),
        Some("demo") => demo(),
        Some("mount") => mount_cmd(),
        Some("unmount") => unmount_cmd(),
        Some("letter") => letter_cmd(),
        Some(other) => {
            println!("volumes: unknown subcommand '{}'", other);
            usage();
            1
        }
    }
}

/// 用法（未知子命令打它）。
fn usage() {
    println!(
        "volumes: usage: volumes [demo | mount <C:> <point> | unmount <point> | letter <C:> <A-Z|0>]"
    );
}

/// 无参数：打印卷表（CSS-1 的行为 + CSS-3 的 `letter=` 列）。
fn list() -> i32 {
    let mut buf = [0u8; TABLE_BYTES];
    let mut total = 0u64;
    let n = storage_volumes(&mut buf, Some(&mut total));
    if n < 0 {
        println!("volumes: storage_volumes failed (errno {})", n);
        return 1;
    }

    println!(
        "volumes: id disk part fs label label_trunc letter ro health bytes free_known free mounts"
    );
    for i in 0..n as usize {
        let label = volume_label(&buf, i);
        let (free_known, free) = match volume_free_bytes(&buf, i) {
            Some(bytes) => (1u8, bytes),
            None => (0u8, 0u64),
        };
        println!(
            "volumes: {} disk={} part={} fs={} label={} label_trunc={} letter={} ro={} health={} \
             bytes={} free_known={} free={} mounts={}",
            volume_id(&buf, i),
            volume_disk(&buf, i),
            volume_part(&buf, i),
            volume_fs(&buf, i),
            if label.is_empty() { "-" } else { label },
            volume_label_truncated(&buf, i) as u8,
            volume_letter_str(&buf, i),
            volume_read_only(&buf, i),
            volume_health(&buf, i),
            volume_total_bytes(&buf, i),
            free_known,
            free,
            volume_mounts(&buf, i)
        );
    }
    println!("volumes: {} volume(s) (total={})", n, total);
    0
}

/// `volumes demo`：**两个入口是同一份数据**的端到端演示（CSS-3 的验收动作）。
///
/// 步骤固定（每一步都有一行可断言证据）：
///
/// 1. 从卷表里取出盘符 `C` 的卷（**不硬编码任何序列号**：字母来自记录偏移 24）；
/// 2. 把它挂到默认挂载点（`storage_mount`，受控动作：走判定点 8）；
/// 3. 用 **Windows 式路径** `C:\KERNEL.ELF` 与 **POSIX 路径**
///    `/volumes/<label>/KERNEL.ELF` 各读一段，字节必须相同 —— 前者由 libc 的
///    路径重写（`docs/storage.md` 4.2）变成后者，因此这条断言同时证明了重写规则；
/// 4. 卸载（受控动作），并断言卸载之后**新** `open` 拿不到路径。
fn demo() -> i32 {
    let Some((vol_id, point)) = boot_like_volume() else {
        println!("volumes: demo: no volume with drive letter C");
        return 1;
    };
    println!("volumes: demo vol={} point={}", vol_id.as_str(), point.as_str());

    let mounted = storage_mount(vol_id.as_str(), point.as_str(), 0);
    println!("volumes: demo storage_mount -> {}", mounted);
    if mounted != 0 {
        return 1;
    }

    let windows = "C:\\KERNEL.ELF";
    let mut pathbuf = [0u8; PATH_MAX];
    let linux = match resolve_str(windows, &mut pathbuf) {
        Ok(path) => path,
        Err(errno) => {
            println!("volumes: demo path rewrite failed errno={}", errno);
            return 1;
        }
    };
    println!("volumes: demo win={} linux={}", windows, linux);

    let mut via_win = [0u8; 16];
    let n_win = read_head(windows, &mut via_win);
    let mut via_linux = [0u8; 16];
    let n_linux = read_head(linux, &mut via_linux);
    if n_win < 0 || n_linux < 0 {
        println!("volumes: demo open failed win={} linux={}", n_win, n_linux);
        return 1;
    }
    let equal = n_win == n_linux && via_win[..n_win as usize] == via_linux[..n_linux as usize];
    println!(
        "volumes: demo read win_bytes={} linux_bytes={} equal={} first=0x{:02x}{:02x}{:02x}{:02x}",
        n_win, n_linux, equal as u8, via_win[0], via_win[1], via_win[2], via_win[3]
    );

    let unmounted = storage_unmount(point.as_str());
    println!("volumes: demo storage_unmount -> {}", unmounted);
    // 卸载之后**新** open 必须拿不到路径（否则「卸载」只是打了条日志）
    let after = read_head(linux, &mut via_linux);
    println!("volumes: demo after unmount open -> {}", after);
    if equal && unmounted == 0 && after < 0 {
        0
    } else {
        1
    }
}

/// `volumes mount <vol_id|C:> <point>`。
fn mount_cmd() -> i32 {
    let (Some(selector), Some(point)) = (arg_str(2), arg_str(3)) else {
        usage();
        return 2;
    };
    let Some(vol_id) = resolve_selector(selector) else {
        println!("volumes: mount: no volume for '{}'", selector);
        return 1;
    };
    let r = storage_mount(vol_id.as_str(), point, 0);
    println!(
        "volumes: mount vol={} point={} -> {}",
        vol_id.as_str(),
        point,
        r
    );
    if r == 0 { 0 } else { 1 }
}

/// `volumes unmount <point>`。
fn unmount_cmd() -> i32 {
    let Some(point) = arg_str(2) else {
        usage();
        return 2;
    };
    let r = storage_unmount(point);
    println!("volumes: unmount point={} -> {}", point, r);
    if r == 0 { 0 } else { 1 }
}

/// `volumes letter <vol_id|C:> <A-Z|0>`：分配/取消盘符（写进 CKM 状态区）。
fn letter_cmd() -> i32 {
    let (Some(selector), Some(letter)) = (arg_str(2), arg_str(3)) else {
        usage();
        return 2;
    };
    let Some(vol_id) = resolve_selector(selector) else {
        println!("volumes: letter: no volume for '{}'", selector);
        return 1;
    };
    let value = match letter.as_bytes() {
        [b'0'] => STORAGE_LETTER_NONE,
        [one] if one.is_ascii_alphabetic() => one.to_ascii_uppercase(),
        _ => {
            println!("volumes: letter: invalid letter '{}' (want A-Z or 0)", letter);
            return 2;
        }
    };
    let r = storage_assign_letter(vol_id.as_str(), value);
    println!(
        "volumes: letter vol={} arg={} -> {}",
        vol_id.as_str(),
        letter,
        r
    );
    if r == 0 { 0 } else { 1 }
}

/// 卷表里盘符为 `C` 的那个卷的 `(vol_id, 默认挂载点)`。
///
/// 默认挂载点按 `docs/storage.md` 4.3 的退化规则：`/volumes/<label>`，
/// 卷标为空则 `/volumes/<vol_id>` —— 与 libc 的路径重写（`libc::resolve_str`）同一条规则。
fn boot_like_volume() -> Option<(ShortText, ShortText)> {
    let mut buf = [0u8; TABLE_BYTES];
    let n = storage_volumes(&mut buf, None);
    if n <= 0 {
        return None;
    }
    for i in 0..n as usize {
        if volume_letter(&buf, i) != b'C' {
            continue;
        }
        let id = volume_id(&buf, i);
        let label = volume_label(&buf, i);
        let name = if label.is_empty() { id } else { label };
        let mut point = ShortText::from_str("/volumes/");
        point.push_str(name);
        return Some((ShortText::from_str(id), point));
    }
    None
}

/// 把 `<vol_id 或 盘符:>` 解析成卷标识（盘符查卷表，**不硬编码字母表**）。
fn resolve_selector(selector: &str) -> Option<ShortText> {
    let bytes = selector.as_bytes();
    if bytes.len() == 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        let want = bytes[0].to_ascii_uppercase();
        let mut buf = [0u8; TABLE_BYTES];
        let n = storage_volumes(&mut buf, None);
        if n <= 0 {
            return None;
        }
        for i in 0..n as usize {
            if volume_letter(&buf, i) == want {
                return Some(ShortText::from_str(volume_id(&buf, i)));
            }
        }
        return None;
    }
    Some(ShortText::from_str(selector))
}

/// `open` + `read` 前 16 字节（返回字节数或负 errno）。
fn read_head(path: &str, out: &mut [u8]) -> isize {
    let fd = open(path, O_RDONLY);
    if fd < 0 {
        return fd;
    }
    let n = read(fd as i32, out);
    let _ = close(fd as i32);
    n
}

/// 定长字符串（用户态没有动态分配的自由：路径拼接这种小事不该依赖 `mmap` 堆，
/// 见 `docs/syscall.md` 的用户态约束）。
struct ShortText {
    buf: [u8; NAME_MAX],
    len: usize,
}

impl ShortText {
    fn empty() -> Self {
        Self {
            buf: [0u8; NAME_MAX],
            len: 0,
        }
    }

    fn from_str(text: &str) -> Self {
        let mut out = Self::empty();
        out.push_str(text);
        out
    }

    /// 追加（超出容量就截断 —— 调用方用的是固定短名：挂载点/卷标识）。
    fn push_str(&mut self, text: &str) {
        let bytes = text.as_bytes();
        let take = bytes.len().min(NAME_MAX - self.len);
        self.buf[self.len..self.len + take].copy_from_slice(&bytes[..take]);
        self.len += take;
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("?")
    }
}
