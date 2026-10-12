//! 存储类系统调用（CSS-1：`storage_volumes` = 261、`storage_rescan` = 265）
//!
//! 这与 `getprocs`/`getdents` 是同一套做法：**定长记录 + 一次调用写满**，
//! 记录布局的权威定义在内核（这里是 `storage::volume`），libc 提供解析器。
//!
//! - [`sys_storage_volumes`] 是**只读**查询，不需要任何能力位（CKM 的只读面
//!   同样如此，见 `docs/storage.md` 6.2）；写入的条数受 `buf_len` 限制，装不下
//!   就截断（每条记录仍然完整）——`out_count_ptr` 用来问「总共有多少条」；
//! - [`sys_storage_rescan`] 是**受控动作**：CKM-1 起要求
//!   `CAP_STORAGE_ADMIN`（`docs/ckm.md` 3.4 判定点 8），拒绝时返回 `-EPERM`
//!   并留下审计 + 串口证据。挂载/卸载/盘符（262/263/264）的行为归 CSS-3，
//!   它们现在仍落在 ENOSYS 桩上，因此那三个动作的判定点 8 也在 CSS-3 才接上
//!   （本文件的 [`sys_storage_rescan`] 是这条判定点已经生效的那一半）。

use crate::ckm::api as ckm;
use crate::ckm::policy::Op;
use crate::driver::serial;
use crate::storage::registry;
use crate::storage::volume::VOLUME_RECORD_SIZE;
use crate::syscall::address_space::UserSpace;

/// `-EINVAL`：缓冲区/参数非法。
const EINVAL: i64 = -22;
/// `-EIO`：扫描/读取设备失败。
const EIO: i64 = -5;

/// 系统调用 `storage_volumes(buf_ptr, buf_len, out_count_ptr, 0, 0)`。
///
/// 按确定性顺序（磁盘升序 → 分区升序 → `vol_id` 升序）把卷记录写进用户缓冲，
/// 返回**实际写入的记录数**；`out_count_ptr != 0` 时同一时刻把**总卷数**写成
/// 一个 LE `u64`（便于调用方先问大小、再决定缓冲）。
///
/// 返回负数 = `-errno`；`buf_len < 64` 返回 `-EINVAL`（至少要装下一条记录）。
pub unsafe fn sys_storage_volumes(args: &[u64; 5], space: *const UserSpace) -> i64 {
    let (buf_ptr, buf_len, count_ptr) = (args[0], args[1], args[2]);

    if (buf_len as usize) < VOLUME_RECORD_SIZE {
        return EINVAL;
    }
    let out = match unsafe {
        crate::syscall::file::user_bytes_mut(space, buf_ptr, buf_len, "storage_volumes")
    } {
        Ok(out) => out,
        Err(errno) => return errno,
    };

    // 快照到内核侧定长缓冲（卷表容量固定，且这个调用不是热路径）
    let mut scratch = [0u8; crate::storage::MAX_VOLUMES * VOLUME_RECORD_SIZE];
    let total = match registry::snapshot(&mut scratch) {
        Ok(n) => n,
        Err(_) => return EIO,
    };

    let cap = out.len() / VOLUME_RECORD_SIZE;
    let written = total.min(cap);
    for i in 0..written {
        let src = &scratch[i * VOLUME_RECORD_SIZE..(i + 1) * VOLUME_RECORD_SIZE];
        out[i * VOLUME_RECORD_SIZE..(i + 1) * VOLUME_RECORD_SIZE].copy_from_slice(src);
    }

    if count_ptr != 0 {
        let out_count = match unsafe {
            crate::syscall::file::user_bytes_mut(space, count_ptr, 8, "storage_volumes")
        } {
            Ok(out) => out,
            Err(errno) => return errno,
        };
        out_count.copy_from_slice(&(total as u64).to_le_bytes());
    }

    serial::print("user: storage_volumes count=");
    serial::print_dec(written as u64);
    serial::print(" total=");
    serial::print_dec(total as u64);
    serial::println("");
    written as i64
}

/// 系统调用 `storage_rescan(disk_index, 0, 0, 0, 0)`。
///
/// `disk_index == u32::MAX`（`0xFFFF_FFFF`）表示重扫全部设备；否则是块设备注册表
/// 序号。重扫只做**只读扫描**：先摘掉该磁盘的旧卷记录，再按当前内容重建并打印
/// 证据行。返回重扫后的卷数；设备不存在返回 `-ENODEV`。
pub unsafe fn sys_storage_rescan(args: &[u64; 5], _space: *const UserSpace) -> i64 {
    let index = args[0];
    if index > u32::MAX as u64 {
        return EINVAL;
    }
    let index = index as u32;

    // 判定点 8（`docs/ckm.md` 3.4）：重扫设备是受控动作，需要 `CAP_STORAGE_ADMIN`。
    // 参数校验之后、动任何设备状态之前判定 —— 拒绝必须发生在副作用之前。
    let errno = ckm::enforce(Op::StorageRescan, index as u64);
    if errno != 0 {
        serial::print("user: storage_rescan disk=");
        serial::print_dec(index as u64);
        serial::println(" -> EPERM (need CAP_STORAGE_ADMIN)");
        return errno;
    }

    if index == u32::MAX {
        return match registry::scan_all(true) {
            Ok(count) => {
                serial::print("user: storage_rescan disk=all volumes=");
                serial::print_dec(count as u64);
                serial::println("");
                count as i64
            }
            Err(_) => EIO,
        };
    }

    if index as usize >= crate::driver::block::count() {
        serial::print("user: storage_rescan disk=");
        serial::print_dec(index as u64);
        serial::println(" -> ENODEV");
        return crate::fs::vfs::ENODEV;
    }
    match registry::rescan_disk(index as usize) {
        Ok(count) => {
            serial::print("user: storage_rescan disk=");
            serial::print_dec(index as u64);
            serial::print(" volumes=");
            serial::print_dec(count as u64);
            serial::println("");
            count as i64
        }
        Err(_) => EIO,
    }
}
