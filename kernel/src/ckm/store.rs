//! CKM 状态区（CKM-2）：**可写持久化**的第一块地基。
//!
//! 权威文档：[`docs/ckm.md`](../../../docs/ckm.md) 第 12 节开放问题 1 与 9 节的 CKM-2 行。
//!
//! ## 定夺：为什么是「极小日志式区」而不是「给 FAT 加写支持」
//!
//! 开放问题 1 给了两条路：(a) 给 FAT 后端加写支持 + 原子替换；(b) 新增一个极小的
//! 日志式区（双槽 A/B + CRC）专供 CKM 状态/回执/store。CKM-2 取 **(b)**，理由：
//!
//! 1. **要的语义不同**。CKM 要的是「**原子状态区**」：一次写入要么整体可见、
//!    要么完全不可见，且掉电后仍能读回上一次成功的值。FAT 的写语义（FAT 表 +
//!    目录项 + 簇链三处更新）不天然提供这一点；而 **文件语义写入**（簇分配、
//!    可写卷）是 CSS-4 的目标，验收标准也不同（写-重启-读回逐字节一致 vs.
//!    回执可恢复）—— 两者不该互相绑架（`docs/ckm.md` 第 12 节末尾那段就是讲这个）；
//! 2. **(a) 会先落地 CSS-4 的开放问题**（存储侧的写入语义评审），而 (b) 完全自洽：
//!    它不碰任何文件系统，因此不阻塞、也不被别的期阻塞；
//! 3. **CSS 的字母表/卷标偏好只需要键值持久化**（[`docs/storage.md`](../../../docs/storage.md) 6.6
//!    的命名空间 `storage/letters`、`storage/labels`），不需要 FAT 写；
//! 4. FAT 写仍是 CSS-4 的第一件事（对用户价值更大），本模块不替代它。
//!
//! ## 介质与布局
//!
//! 状态区是**系统盘（块设备 0 = 启动盘）尾部的 1 MiB 原始扇区**
//! （`tools/image-builder/build_image.py` 的 `RESERVED_BLOCKS`：先在 63 MiB 上
//! `mformat`，再把保留区追加回去 —— 于是它在 FAT 之外，不与用户文件争簇，
//! 设备容量与分区识别都不变）。
//!
//! ```text
//! 偏移 0     槽 A（4 KiB = 8 扇区）
//! 偏移 4096  槽 B（4 KiB = 8 扇区）
//! 其余       保留（CKM-3 的 store / 回执，`docs/ckm.md` 8.3）
//! ```
//!
//! 每个槽自描述，**没有单独的「当前槽」指针** —— 指针本身就是一次非原子写：
//!
//! ```text
//! [0..4)   magic      "KMS1"
//! [4..8)   version    1（布局版本，与 CKM_ABI_VERSION 无关）
//! [8..16)  seq        u64（单调递增；读者取「有效且 seq 最大」的槽）
//! [16..20) payload_len u32
//! [20..24) crc32      payload 的 CRC32
//! [24..)   payload    KV 表（见 [`encode`]）
//! ```
//!
//! 写入顺序：**先写非活动槽的整个 4 KiB（含 CRC），成功后再把内存里的视图切过去**。
//! 掉电发生在写槽的中途时，那个槽的 CRC 对不上、于是仍然无效，上一个槽完好 ——
//! 这就是「双槽 A/B」的全部机制，也是它比「FAT 三处更新」更容易论证的原因。
//!
//! ## 与审计环的关系
//!
//! 审计环仍然只在内存（重启即丢，`docs/ckm.md` 第 12 节开放问题 7）；本模块只提供
//! 键值持久化的**机制**，CKM-3 的回执/事务与 CKM-6 的审计落盘都建在它上面。

use alloc::string::String;
use alloc::vec::Vec;

use crate::driver::block::{self, SECTOR_SIZE};
use crate::driver::serial;

/// 状态区大小（扇区）—— **必须**与 `tools/image-builder/build_image.py` 的
/// `RESERVED_BLOCKS`（1 MiB）一致：内核按「设备尾部这么多扇区」定位它。
pub const AREA_SECTORS: u64 = 2048;

/// 单个槽的扇区数（4 KiB：一个扇区放头，其余放 payload；留足 CKM-3 的回执）。
const SLOT_SECTORS: u64 = 8;

/// 单个槽的字节数。
const SLOT_BYTES: usize = SLOT_SECTORS as usize * SECTOR_SIZE as usize;

/// 槽头长度（见模块头的布局表）。
const HEADER_LEN: usize = 24;

/// 槽魔数（"KMS1"：Cerlesse Kernel Manager Store v1）。
const MAGIC: u32 = 0x4B4D5331;

/// 槽布局版本。
const VERSION: u32 = 1;

/// 一次能放的键值对上限（固定容量 + 明确拒绝，与状态表/挂载表同风格）。
pub const ENTRY_MAX: usize = 16;

/// 键长上限（字节）。
pub const KEY_MAX: usize = 32;

/// 值长上限（字节）—— 状态区是「极小」的：放偏好与回执，不放文件。
pub const VALUE_MAX: usize = 255;

/// 状态区不可用时的错误（设备缺失/太小时调用方只记录 sk.
pub const ENOENT: i64 = -2;
/// 参数非法（键/值超长、空键）。
pub const EINVAL: i64 = -22;
/// 空间不足（槽位满 / payload 超过槽容量）。
pub const ENOSPC: i64 = -28;

/// 一个键值对。
struct Entry {
    key: String,
    value: Vec<u8>,
}

/// 状态区在内存里的视图（静态量：内核单线程，与卷表/挂载表同一套契约）。
struct Store {
    /// 已定位到状态区（`init` 之后才可能为真）
    enabled: bool,
    /// 设备名（证据行用）
    device: &'static str,
    /// 状态区起始 LBA
    base_lba: u64,
    /// 已装载过一个有效槽
    valid: bool,
    /// 上次写入的槽下标（0 = A，1 = B）
    slot: u8,
    /// 有效槽的序号（读者取 seq 最大者）
    seq: u64,
    /// KV 表
    entries: Vec<Entry>,
}

impl Store {
    const fn new() -> Self {
        Self {
            enabled: false,
            device: "none",
            base_lba: 0,
            valid: false,
            slot: 0,
            seq: 0,
            entries: Vec::new(),
        }
    }
}

// SAFETY: 内核单线程；`store()` 是唯一的访问点，调用期间独占（与 `fs::mount::table` 同）。
static mut STORE: Store = Store::new();

/// 槽读写缓冲（4 KiB，静态而非栈上：启动路径的栈很小，没必要为一次搬运冒这个险）。
static mut SLOT_BUF: [u8; SLOT_BYTES] = [0; SLOT_BYTES];

fn store() -> &'static mut Store {
    // `&raw mut` 与 `fs::mount::table` 一致：避开 `static_mut_refs` lint，语义不变。
    unsafe { &mut *(&raw mut STORE) }
}

fn slot_buf() -> &'static mut [u8; SLOT_BYTES] {
    unsafe { &mut *(&raw mut SLOT_BUF) }
}

/// 定位状态区：系统盘（块设备 0）尾部的 [`AREA_SECTORS`] 个扇区。
///
/// 返回 `(设备下标, 起始 LBA)`；设备缺失或容量太小时返回 `None`
/// （那时状态区整个不可用，调用方把它当成「没有持久化」而不是「空的持久化」）。
fn locate() -> Option<(usize, u64)> {
    let sectors = block::get(0)?.sectors();
    if sectors < AREA_SECTORS + 64 {
        return None;
    }
    Some((0, sectors - AREA_SECTORS))
}

/// 初始化：定位 + 装载有效槽（**幂等**）。
///
/// 必须在块设备注册之后调用（`kernel_main` 里在 `storage::init` 之后）。
/// 证据行三态：`state=fresh`（两个槽都无效 → 首次使用）、`state=loaded`（装载成功）、
/// `state=absent`（没有可用设备）。
pub fn init() {
    let s = store();
    if s.enabled || s.device != "none" {
        return; // 幂等：已经初始化过
    }
    let Some((index, base)) = locate() else {
        serial::println("ckm: store area absent (no usable block device); persistence off");
        return;
    };
    let name = block::get(index).map(|dev| dev.name()).unwrap_or("none");
    s.enabled = true;
    s.device = name;
    s.base_lba = base;

    // 两个槽都读一遍，取「有效且 seq 最大」者（没有活动指针，见模块头）。
    let mut best: Option<(u8, u64, Vec<Entry>)> = None;
    for slot in 0..2u8 {
        if let Some((seq, entries)) = read_slot(base, slot) {
            let better = match &best {
                None => true,
                Some((_, best_seq, _)) => seq > *best_seq,
            };
            if better {
                best = Some((slot, seq, entries));
            }
        }
    }

    match best {
        Some((slot, seq, entries)) => {
            s.valid = true;
            s.slot = slot;
            s.seq = seq;
            s.entries = entries;
            serial::print("ckm: store area dev=");
            serial::print(s.device);
            serial::print(" lba=");
            serial::print_dec(base);
            serial::print(" slots=2 slot_bytes=");
            serial::print_dec(SLOT_BYTES as u64);
            serial::print(" state=loaded slot=");
            serial::print(slot_name(slot));
            serial::print(" seq=");
            serial::print_dec(seq);
            serial::print(" keys=");
            serial::print_dec(s.entries.len() as u64);
            serial::println("");
        }
        None => {
            s.valid = false;
            s.seq = 0;
            s.entries.clear();
            serial::print("ckm: store area dev=");
            serial::print(s.device);
            serial::print(" lba=");
            serial::print_dec(base);
            serial::print(" slots=2 slot_bytes=");
            serial::print_dec(SLOT_BYTES as u64);
            serial::println(" state=fresh");
        }
    }
}

/// 状态区是否可用（`make run` 之外的纯内存场景也可能没有块设备）。
pub fn enabled() -> bool {
    store().enabled
}

/// 读一个键（**内存视图**：`init`/`put` 之后都已与盘上一致）。
pub fn get(key: &str) -> Option<&'static [u8]> {
    let s: &'static Store = store();
    s.entries
        .iter()
        .find(|entry| entry.key == key)
        .map(|entry| entry.value.as_slice())
}

/// 键值对数量。
pub fn key_count() -> usize {
    store().entries.len()
}

/// 写一个键：更新内存视图 → 序列化 → 写**非活动槽** → 切换视图。
///
/// 失败时**不改变**内存视图（否则「写失败但读得到」会掩盖掉盘上的不一致）。
pub fn put(key: &str, value: &[u8]) -> Result<(), i64> {
    if !store().enabled {
        return Err(ENOENT);
    }
    if key.is_empty() || key.len() > KEY_MAX || value.len() > VALUE_MAX {
        return Err(EINVAL);
    }
    let s = store();

    // 先在**副本**上改（原表在失败时保持不动）
    let mut next: Vec<Entry> = Vec::with_capacity(s.entries.len() + 1);
    let mut replaced = false;
    for entry in &s.entries {
        if entry.key == key {
            next.push(Entry {
                key: entry.key.clone(),
                value: value.to_vec(),
            });
            replaced = true;
        } else {
            next.push(Entry {
                key: entry.key.clone(),
                value: entry.value.clone(),
            });
        }
    }
    if !replaced {
        if next.len() >= ENTRY_MAX {
            return Err(ENOSPC);
        }
        next.push(Entry {
            key: String::from(key),
            value: value.to_vec(),
        });
    }

    let payload_len = encoded_len(&next);
    if payload_len > SLOT_BYTES - HEADER_LEN {
        return Err(ENOSPC);
    }

    let seq = s.seq + 1;
    let slot = if s.valid { 1 - s.slot } else { 0 };
    if !write_slot(s.base_lba, slot, seq, &next) {
        return Err(vfs_eio());
    }

    s.entries = next;
    s.valid = true;
    s.slot = slot;
    s.seq = seq;

    serial::print("ckm: store put key=");
    serial::print(key);
    serial::print(" len=");
    serial::print_dec(value.len() as u64);
    serial::print(" seq=");
    serial::print_dec(seq);
    serial::print(" slot=");
    serial::println(slot_name(slot));
    Ok(())
}

/// `-EIO`：块设备读写失败（与 `fs::vfs::EIO` 同一个值，这里不引入 `fs` 依赖）。
const fn vfs_eio() -> i64 {
    -5
}

/// 启动自测：`make test-ckm` 的断言来源。
///
/// 它同时是**跨重启一致性**的证据来源（`tests/test_ckm_store.py` 跑两次启动）：
/// 首次启动走 `put`（`state=fresh` → 两个槽各写一次，证明 A/B 交替），
/// 第二次启动走 `state=loaded` + 读回**逐字节相同**的值，且**不写盘**。
pub fn selftest() -> Result<(), &'static str> {
    if !enabled() {
        serial::println("ckm: store selftest skipped (no state area)");
        return Ok(());
    }
    let mut checks = 0usize;

    // 1. 两个键：一个证明「写」，一个证明「表不是单键特例」；
    //    已存在且值相同时**不重写**（这正是「读回上次启动写下的内容」的证据）。
    const SELFTEST_KEY: &str = "selftest";
    const SELFTEST_VALUE: &[u8] = b"cerlesse-ckm-store-v1";
    const MSG_KEY: &str = "msg";
    const MSG_VALUE: &[u8] = b"persist-ok";

    let have_selftest = get(SELFTEST_KEY) == Some(SELFTEST_VALUE);
    if !have_selftest {
        put(SELFTEST_KEY, SELFTEST_VALUE).map_err(|_| "ckm: store put(selftest) failed")?;
    }
    let have_msg = get(MSG_KEY) == Some(MSG_VALUE);
    if !have_msg {
        put(MSG_KEY, MSG_VALUE).map_err(|_| "ckm: store put(msg) failed")?;
    }
    checks += 1;

    // 2. 读回逐字节一致（这两行是**两次启动之间**可对齐的断言）
    for (key, expected) in [(SELFTEST_KEY, SELFTEST_VALUE), (MSG_KEY, MSG_VALUE)] {
        let got = get(key).ok_or("ckm: store selftest lost a key")?;
        if got != expected {
            return Err("ckm: store selftest value mismatch");
        }
        serial::print("ckm: store verify key=");
        serial::print(key);
        serial::print(" value=");
        serial::put_bytes(got, got.len());
        serial::println(" ok=1");
        checks += 1;
    }

    // 3. 序号单调 + 槽位在 A/B 之间交替（同一槽连写两次会让「双槽」名不副实）
    let before = store().seq;
    put("seq-probe", b"1").map_err(|_| "ckm: store put(seq-probe) failed")?;
    let after = store().seq;
    if after != before + 1 {
        return Err("ckm: store sequence did not advance by one");
    }
    serial::print("ckm: store keys=");
    serial::print_dec(key_count() as u64);
    serial::print(" seq=");
    serial::print_dec(after);
    serial::println("");
    checks += 1;

    // 4. 容量与参数的反例（「固定容量 + 明确拒绝」必须真的会拒绝）
    if put("", b"x") != Err(EINVAL) {
        return Err("ckm: store accepted an empty key");
    }
    let long = [0u8; VALUE_MAX + 1];
    if put("too-long", &long) != Err(EINVAL) {
        return Err("ckm: store accepted an oversized value");
    }
    checks += 2;

    serial::print("ckm: store selftest PASS (checks=");
    serial::print_dec(checks as u64);
    serial::println(")");
    Ok(())
}

/// 读一个槽：`Some((seq, entries))` 表示槽有效（magic/version/长度/CRC 全对）。
fn read_slot(base_lba: u64, slot: u8) -> Option<(u64, Vec<Entry>)> {
    let dev = block::get(0)?;
    let lba = base_lba + slot as u64 * SLOT_SECTORS;
    let buf = slot_buf();
    let read = dev.read(lba, buf).ok()?;
    if read != SLOT_BYTES {
        return None;
    }
    if u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) != MAGIC {
        return None;
    }
    if u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]) != VERSION {
        return None;
    }
    let seq = u64::from_le_bytes([
        buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15],
    ]);
    let len = u32::from_le_bytes([buf[16], buf[17], buf[18], buf[19]]) as usize;
    let crc = u32::from_le_bytes([buf[20], buf[21], buf[22], buf[23]]);
    if len > SLOT_BYTES - HEADER_LEN {
        return None;
    }
    if crc32(&buf[HEADER_LEN..HEADER_LEN + len]) != crc {
        return None;
    }
    let entries = decode(&buf[HEADER_LEN..HEADER_LEN + len])?;
    Some((seq, entries))
}

/// 写一个槽：整槽 4 KiB（头 + payload + 零填充），**最后才**是 CRC 本身已经在头里 ——
/// 掉电在任何扇区都可能，因此「头里的 CRC 与 payload 不匹配」是唯一需要检出的状态。
fn write_slot(base_lba: u64, slot: u8, seq: u64, entries: &[Entry]) -> bool {
    let buf = slot_buf();
    buf.fill(0);
    let payload_len = encode(entries, &mut buf[HEADER_LEN..]);
    buf[0..4].copy_from_slice(&MAGIC.to_le_bytes());
    buf[4..8].copy_from_slice(&VERSION.to_le_bytes());
    buf[8..16].copy_from_slice(&seq.to_le_bytes());
    buf[16..20].copy_from_slice(&(payload_len as u32).to_le_bytes());
    let crc = crc32(&buf[HEADER_LEN..HEADER_LEN + payload_len]);
    buf[20..24].copy_from_slice(&crc.to_le_bytes());
    let Some(dev) = block::get(0) else {
        return false;
    };
    let lba = base_lba + slot as u64 * SLOT_SECTORS;
    matches!(dev.write(lba, buf), Ok(written) if written == SLOT_BYTES)
}

/// 序列化 KV 表：`count: u16` + 逐条 `klen: u8` / `vlen: u16` / key / value。
fn encode(entries: &[Entry], out: &mut [u8]) -> usize {
    let mut at = 0;
    out[at..at + 2].copy_from_slice(&(entries.len() as u16).to_le_bytes());
    at += 2;
    for entry in entries {
        out[at] = entry.key.len() as u8;
        at += 1;
        out[at..at + 2].copy_from_slice(&(entry.value.len() as u16).to_le_bytes());
        at += 2;
        out[at..at + entry.key.len()].copy_from_slice(entry.key.as_bytes());
        at += entry.key.len();
        out[at..at + entry.value.len()].copy_from_slice(&entry.value);
        at += entry.value.len();
    }
    at
}

/// 序列化后的字节数（不需要真的写盘时用它做容量检查）。
fn encoded_len(entries: &[Entry]) -> usize {
    2 + entries
        .iter()
        .map(|entry| 3 + entry.key.len() + entry.value.len())
        .sum::<usize>()
}

/// 反序列化：任何自相矛盾（键过长、越界）都返回 `None`（槽无效），不 panic。
fn decode(payload: &[u8]) -> Option<Vec<Entry>> {
    if payload.len() < 2 {
        return None;
    }
    let count = u16::from_le_bytes([payload[0], payload[1]]) as usize;
    if count > ENTRY_MAX {
        return None;
    }
    let mut at = 2usize;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        if at + 3 > payload.len() {
            return None;
        }
        let klen = payload[at] as usize;
        let vlen = u16::from_le_bytes([payload[at + 1], payload[at + 2]]) as usize;
        at += 3;
        if klen == 0 || klen > KEY_MAX || vlen > VALUE_MAX || at + klen + vlen > payload.len() {
            return None;
        }
        let key = core::str::from_utf8(&payload[at..at + klen]).ok()?;
        at += klen;
        let value = payload[at..at + vlen].to_vec();
        at += vlen;
        entries.push(Entry {
            key: String::from(key),
            value,
        });
    }
    Some(entries)
}

/// CRC32（IEEE 802.3，与 GPT 表用的同一个多项式）。
///
/// 逐位实现而不是查表：payload 最多 4 KiB，省下 1 KiB 的表是划算的
/// （`storage/gpt.rs` 有自己的表版本，两处各自独立 —— 一个在存卷表、一个在存状态，
/// 不为了少 20 行代码把 CKM 与 storage 绑在一起）。
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in data {
        crc ^= *byte as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

/// 槽名（证据行用）：0 = A、1 = B。
fn slot_name(slot: u8) -> &'static str {
    if slot == 0 {
        "A"
    } else {
        "B"
    }
}
