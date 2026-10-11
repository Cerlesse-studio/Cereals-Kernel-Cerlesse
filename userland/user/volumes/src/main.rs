//! `volumes` —— 列出内核卷表（CSS-1 的基础用户程序，RAMFS 上落在 `/bin/volumes`）
//!
//! 它只需要一条内核能力：**`storage_volumes`**（CSS-1 新增的 Cerlesse 扩展系统调用，
//! 编号 261）。与 `ps` 是同一套做法：记录定长（64 字节/条），布局的权威定义在内核
//! （`kernel/src/storage/volume.rs::write_record`），libc 提供 `volume_*` 解析器。
//!
//! 为什么值得单独做一个程序：**记录布局是 ABI**。内核自测只能证「两次快照一模一样」，
//! 证明不了「消费者按这个布局读得到东西」——`volumes` 就是那个消费者：偏移一旦读错
//! （比如把 `fs_kind` 当成 `read_only`），输出字段立刻对不上。
//!
//! 输出形状（字段顺序 = 记录里的顺序，便于逐项比对；空卷标打 `-`）：
//!
//! ```text
//! volumes: id disk part fs label label_trunc ro health bytes free_known free mounts
//! volumes: fs:1a2b3c4d disk=0 part=0 fs=fat16 label=NO_NAME label_trunc=0 ro=ro-backend health=ok bytes=67108864 free_known=0 free=0 mounts=0
//! volumes: 1 volume(s) (total=1)
//! ```
//!
//! 刻意保留的几处区分（`docs/storage.md` 3.5/3.6）：
//!
//! - `fs=unsupported`（没认出格式）与 `health=unsupported`（没有后端）不是一回事，
//!   因此 `fs=ext4 health=unsupported` 是合法组合；
//! - `label_trunc=1` 表示 11 字节装不下、卷标被截过（不许静默截断）；
//! - `free_known=0` 表示可用容量**未知** —— 不用 0 冒充「盘满」。

#![no_std]
#![no_main]

use libc::{
    println, storage_volumes, volume_disk, volume_free_bytes, volume_fs, volume_health, volume_id,
    volume_label, volume_label_truncated, volume_mounts, volume_part, volume_read_only,
    volume_total_bytes, MAX_VOLUMES, VOLUME_RECORD_SIZE,
};

/// 卷表容量（内核 `storage::MAX_VOLUMES` = 16）—— 一次调用就能装下整张表。
const BUF: usize = MAX_VOLUMES * VOLUME_RECORD_SIZE;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let mut buf = [0u8; BUF];
    let mut total = 0u64;
    let n = storage_volumes(&mut buf, Some(&mut total));
    if n < 0 {
        println!("volumes: storage_volumes failed (errno {})", n);
        return 1;
    }

    println!("volumes: id disk part fs label label_trunc ro health bytes free_known free mounts");
    for i in 0..n as usize {
        let label = volume_label(&buf, i);
        let (free_known, free) = match volume_free_bytes(&buf, i) {
            Some(bytes) => (1u8, bytes),
            None => (0u8, 0u64),
        };
        println!(
            "volumes: {} disk={} part={} fs={} label={} label_trunc={} ro={} health={} \
             bytes={} free_known={} free={} mounts={}",
            volume_id(&buf, i),
            volume_disk(&buf, i),
            volume_part(&buf, i),
            volume_fs(&buf, i),
            if label.is_empty() { "-" } else { label },
            volume_label_truncated(&buf, i) as u8,
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
