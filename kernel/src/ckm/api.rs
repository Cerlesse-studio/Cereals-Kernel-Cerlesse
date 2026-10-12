//! 给 `syscall/` 用的唯一入口（`docs/ckm.md` 4.1）
//!
//! 依赖方向是**单向**的：`syscall/* → ckm::api → {policy, cred, state, audit}`。
//! 别的内核模块（`fs/` 只取前缀常量、`process/` 只存一个 `Cred` 字段）
//! 不碰这里的内部类型 —— 「判定点在哪」由调用方按 `docs/ckm.md` 3.4 的落点表决定，
//! 「判定怎么判」只在这里实现一次。

use crate::ckm::audit::{self, Record, AUDIT_RECORD_SIZE};
use crate::ckm::cred;
use crate::ckm::policy::{self, Op, RESULT_ALLOW, RESULT_DENY};
use crate::ckm::state::{self, COMPONENT_RECORD_SIZE};
use crate::security::Cred;

/// 初始化（幂等）：见 [`crate::ckm::init`]。
pub fn init() {
    crate::ckm::init();
}

/// `ckm_policy_query`（257）的结果：errno + 需要的位 + 结果码。
pub struct QueryOut {
    /// `0` = 允许，`-EPERM` = 拒绝
    pub errno: i64,
    /// 该 op 需要的 capability 位（deny 时非 0）
    pub need: u32,
    /// 结果码（[`RESULT_ALLOW`] / [`RESULT_DENY`]）
    pub result: u8,
}

/// 查「某个 op 在当前进程凭证下的判定」：**写审计 + 打证据行**（与真实判定同一路径）。
///
/// 为什么查询也要审计：`docs/ckm.md` 4.5 要求判定行 allow/deny 都打。
/// 若查询走一条「不算数」的旁路，就会出现「日志里没有这条判定」的漏洞，
/// 而 `ckm_policy_query` 恰恰是最容易被滥用的调用（它可以被用来探测权限）。
pub fn policy_query(op: Op, arg: u64) -> QueryOut {
    let c = cred::current();
    let need = op.need();
    let result = policy::check(op, &c, arg);
    QueryOut {
        errno: match result {
            Ok(()) => 0,
            Err(errno) => errno,
        },
        need,
        result: if result.is_ok() { RESULT_ALLOW } else { RESULT_DENY },
    }
}

/// 组装 `ckm_policy_query` 返回给用户态的 48 字节记录。
///
/// **复用审计记录的布局**（`docs/ckm.md` 4.4）而不是新造一种：消费者（libc / CLI）
/// 因此只需要一套解析器。`seq = 0` 明确表示「这不是一条审计事件」。
/// 这是对规格空白的补齐，写进了 `docs/ckm.md` 第 13 节修订记录。
pub fn policy_query_record(out: &mut [u8], op: Op, arg: u64, q: &QueryOut) {
    let c = cred::current();
    let rec = Record {
        seq: 0,
        pid: cred::current_pid(),
        uid: c.uid,
        gid: c.gid,
        op: op.code(),
        result: q.result,
        need: if q.result == RESULT_DENY { q.need } else { 0 },
        detail: arg,
    };
    rec.write(out);
}

/// `ckm_audit_read`（258）的数据源：把 `seq > since_seq` 的记录拷进 `out`。
pub fn audit_read(since_seq: u64, out: &mut [u8]) -> usize {
    audit::read(since_seq, out)
}

/// 已记录条数（`ckm_audit_read` 的 `out_count` 用）。
pub fn audit_len() -> usize {
    audit::len()
}

/// 已分配的最大审计序号（`ckm_audit_read` 的 `since_seq` 用）。
pub fn audit_last_seq() -> u64 {
    audit::last_seq()
}

/// `ckm_component_state`（259）：按**确定性顺序**导出组件状态记录，返回条数。
///
/// 调用方必须保证 `out.len() >= count * COMPONENT_RECORD_SIZE`（否则返回 0 并
/// 由 syscall 层报 `ENOSPC`）—— 与固定容量风格一致：**要么写完整记录，要么明确失败**，
/// 不做「半条记录」的截断。
pub fn component_state(out: &mut [u8]) -> usize {
    let count = state::count();
    if out.len() < count * COMPONENT_RECORD_SIZE {
        return 0;
    }
    for i in 0..count {
        let Some(slot) = state::ordered(i) else {
            return i;
        };
        let start = i * COMPONENT_RECORD_SIZE;
        slot.write_record(&mut out[start..start + COMPONENT_RECORD_SIZE]);
    }
    count
}

/// 已登记组件数。
pub fn component_count() -> usize {
    state::count()
}

/// 当前进程的凭证（`syscall/` 里的判定点落点用；`ckm::cred::current` 的唯一对外路径）。
pub fn current_cred() -> Cred {
    cred::current()
}

/// 受保护前缀判定（`fs/` 与 `syscall/file.rs` 用；返回值是前缀下标，给 `detail`）。
pub fn protected_index(path: &str) -> Option<usize> {
    policy::protected_index(path)
}

/// 判定并返回 errno（`0` 或 `-EPERM`）：判定点落点的统一入口。
pub fn enforce(op: Op, arg: u64) -> i64 {
    match policy::check(op, &cred::current(), arg) {
        Ok(()) => 0,
        Err(errno) => errno,
    }
}

/// `execve` 之后的凭证（组件表驱动，不做能力并集）。
pub fn exec_cred(path: &str, parent: Cred) -> Cred {
    cred::exec_cred(path, parent)
}

/// 内核直接运行的顶层程序的初始凭证。
pub fn program_cred(path: &str) -> Cred {
    cred::program_cred(path)
}

/// 内核侧自测（启动时跑一次，`make test-ckm` 断言 `ckm: selftest PASS (checks=N)`）。
///
/// 覆盖的**机制**（每一条都能失败，不是走过场）：
///
/// 1. 引导组件表就位且枚举**逐字节确定**（两次快照比较）；
/// 2. 受保护前缀的**边界**判定（`/etcetera`、`/binx` 不得命中）；
/// 3. 能力矩阵：`plain` 全拒、`admin` 恰好 5 位、`init` 只有系统写入；
/// 4. 状态机：非法转换 `EINVAL` 且**状态不变**；
/// 5. 审计可读：`since_seq` 过滤、序号从 1 单调；
/// 6. 审计**回绕**：写满后再写仍然可读、最新的是最后一条；
/// 7. 凭证切换**不提权**：`/bin/hello` 从管理员 exec 之后是 uid 2000 且无能力位；
/// 8. 登记校验拒绝 `kind=4`、非法 `id`、重复 target。
///
/// 第 6 条会往环里灌满合成事件（静默），跑完**清空**审计环，让后面的真实事件从
/// `seq=1` 重新开始 —— 否则用户态断言到的第一段审计里会混进 65 条自测数据。
/// 回绕本身仍留有 `ckm: audit ring wrapped capacity=64` 这行证据。
pub fn selftest() -> Result<(), &'static str> {
    let mut checks = 0usize;

    // 1) 引导组件表 + 枚举确定性
    if state::count() != 3 {
        return Err("ckm: expected 3 bootstrap components in the state table");
    }
    checks += 1;
    let ordered_count = {
        let mut a = [u8::MAX; crate::ckm::STATE_SLOTS];
        let mut b = [u8::MAX; crate::ckm::STATE_SLOTS];
        let n = state::ordered_indices(&mut a);
        let m = state::ordered_indices(&mut b);
        if n != m || a != b {
            return Err("ckm: component enumeration order is not stable");
        }
        if n != 3 {
            return Err("ckm: expected 3 ordered components");
        }
        for i in 0..n {
            let s1 = state::ordered(i).ok_or("ckm: ordered() out of range")?;
            let s2 = state::ordered(i).ok_or("ckm: ordered() out of range")?;
            let mut r1 = [0u8; COMPONENT_RECORD_SIZE];
            let mut r2 = [0u8; COMPONENT_RECORD_SIZE];
            s1.write_record(&mut r1);
            s2.write_record(&mut r2);
            if r1 != r2 {
                return Err("ckm: component record is not byte-stable across snapshots");
            }
            if i == 0 && s1.id_str() != "hello" {
                return Err("ckm: expected 'hello' first (kind asc, then id asc)");
            }
            if i == 2 && s1.id_str() != "init" {
                return Err("ckm: expected 'init' last (kind=2 after kind=1)");
            }
        }
        n
    };
    let _ = ordered_count;
    checks += 1;

    // 2) 受保护前缀边界
    let positive = ["/etc", "/etc/motd", "/bin/sh", "/lib/ld.so", "/init"];
    let negative = ["/etcetera", "/binx", "/tmp/x", "/", "/initextra"];
    for p in positive {
        if !policy::is_protected(p) {
            return Err("ckm: protected prefix misclassified (expected protected)");
        }
        checks += 1;
    }
    for p in negative {
        if policy::is_protected(p) {
            return Err("ckm: protected prefix misclassified (expected unprotected)");
        }
        checks += 1;
    }

    // 3) 能力矩阵
    let plain = Cred::plain();
    for op in [Op::FsWrite, Op::StateWrite, Op::PolicySet, Op::AuditRead, Op::StorageRescan, Op::StorageFormat] {
        if policy::allowed(op, &plain) {
            return Err("ckm: plain credentials must not hold any capability");
        }
        checks += 1;
    }
    let admin = Cred::admin();
    for op in [Op::FsWrite, Op::PolicySet, Op::AuditRead, Op::StorageRescan, Op::StorageFormat] {
        if !policy::allowed(op, &admin) {
            return Err("ckm: admin credentials must hold the admin capability set");
        }
        checks += 1;
    }
    if policy::allowed(Op::StateWrite, &admin) {
        return Err("ckm: admin must NOT hold CAP_CKM_SERVICE (that is ckmd)");
    }
    checks += 1;
    let init_cred = Cred::init();
    if !policy::allowed(Op::FsWrite, &init_cred) || policy::allowed(Op::AuditRead, &init_cred) {
        return Err("ckm: /init must hold CAP_FS_SYSTEM_WRITE but not CAP_AUDIT_READ");
    }
    checks += 1;
    if !policy::allowed(Op::AuditRead, &Cred::service()) || policy::allowed(Op::PolicySet, &Cred::service())
    {
        return Err("ckm: ckmd must hold CAP_AUDIT_READ but not CAP_CKM_ADMIN");
    }
    checks += 1;

    // 4) 状态机：非法转换被拒且状态不变
    let before = state::ordered(0).ok_or("ckm: no component")?;
    if state::set_state(0, state::State::Installed).is_ok() {
        return Err("ckm: active -> installed must be rejected (illegal transition)");
    }
    let after = state::ordered(0).ok_or("ckm: no component")?;
    if before.state != after.state {
        return Err("ckm: rejected transition changed component state");
    }
    checks += 2;
    // 合法转换走一遍再走回来（evidence 行也因此出现在启动日志里）
    let slot_index = index_of(before.id_str()).ok_or("ckm: bootstrap component lost")?;
    state::set_state(slot_index, state::State::Disabled).map_err(|_| "ckm: active -> disabled must be legal")?;
    state::set_state(slot_index, state::State::Active).map_err(|_| "ckm: disabled -> active must be legal")?;
    checks += 2;

    // 5) 审计：真实判定产生记录，since_seq 可过滤，序号从 1 单调
    let _ = policy::check(Op::FsWrite, &plain, 0); // deny
    let _ = policy::check(Op::PolicyQuery, &plain, 0); // allow
    let last = audit::last_seq();
    if last < 2 {
        return Err("ckm: policy checks did not produce audit records");
    }
    checks += 1;
    let mut buf = [0u8; AUDIT_RECORD_SIZE * 8];
    let n = audit::read(0, &mut buf);
    if n < 2 {
        return Err("ckm: audit_read returned too few records");
    }
    checks += 1;
    let mut prev = 0u64;
    for i in 0..n {
        let seq = u64::from_le_bytes(
            buf[i * AUDIT_RECORD_SIZE..i * AUDIT_RECORD_SIZE + 8]
                .try_into()
                .expect("8 字节"),
        );
        if seq <= prev {
            return Err("ckm: audit sequence is not monotonic");
        }
        prev = seq;
    }
    checks += 1;
    let n_tail = audit::read(last - 1, &mut buf);
    if n_tail != 1 {
        return Err("ckm: audit_read with since_seq must return exactly the newest record");
    }
    checks += 1;

    // 6) 审计回绕：写满 + 再写仍然可读，最新一条在最后
    for _ in 0..(crate::ckm::AUDIT_CAPACITY + 1) {
        audit::record_silent(0, 0, 0, Op::FsWrite, RESULT_DENY, 0, 0);
    }
    if !audit::wrapped() {
        return Err("ckm: audit ring did not report wrapped after exceeding capacity");
    }
    if audit::len() != crate::ckm::AUDIT_CAPACITY {
        return Err("ckm: audit ring length must saturate at capacity");
    }
    checks += 2;
    let mut ring_buf = [0u8; AUDIT_RECORD_SIZE * crate::ckm::AUDIT_CAPACITY];
    let n_wrapped = audit::read(0, &mut ring_buf);
    if n_wrapped != crate::ckm::AUDIT_CAPACITY {
        return Err("ckm: audit_read must still return the full ring after wrapping");
    }
    checks += 1;
    let oldest = u64::from_le_bytes(ring_buf[0..8].try_into().expect("8 字节"));
    let newest_off = (n_wrapped - 1) * AUDIT_RECORD_SIZE;
    let newest = u64::from_le_bytes(
        ring_buf[newest_off..newest_off + 8].try_into().expect("8 字节"),
    );
    if newest <= oldest || newest != audit::last_seq() {
        return Err("ckm: wrapped ring must retain the newest records in order");
    }
    checks += 1;
    // 自测的合成事件不留进真实审计（见本函数文档）
    audit::reset();
    if audit::wrapped() || audit::len() != 0 || audit::last_seq() != 0 {
        return Err("ckm: audit reset after wrap selftest failed");
    }
    checks += 1;

    // 7) 凭证切换不提权
    let switched = cred::exec_cred("/bin/hello", Cred::admin());
    if switched.uid != crate::security::credential::PLAIN_UID || switched.caps != 0 {
        return Err("ckm: execve of a registered component must not inherit parent capabilities");
    }
    checks += 1;
    let inherited = cred::exec_cred("/bin/not-a-component", Cred::admin());
    if inherited != Cred::admin() {
        return Err("ckm: execve of an unregistered program must inherit the parent credentials");
    }
    checks += 1;
    let init_prog = cred::program_cred("/init");
    if init_prog.uid != 0 || init_prog.caps != crate::security::capability::INIT {
        return Err("ckm: /init must start as uid 0 with CAP_FS_SYSTEM_WRITE only");
    }
    checks += 1;

    // 8) 登记校验：kind=4 / 非法 id / 重复 target 都必须被拒
    if state::register(
        "kmod",
        state::Kind::KernelModule,
        0,
        crate::ckm::CKM_ABI_VERSION,
        crate::ckm::CKM_ABI_VERSION,
        0,
        0,
        0,
        "/kmod",
    )
    .is_ok()
    {
        return Err("ckm: kind=4 (kernel module) must be rejected before phase 7");
    }
    checks += 1;
    if state::register(
        "bad id",
        state::Kind::Userland,
        0,
        crate::ckm::CKM_ABI_VERSION,
        crate::ckm::CKM_ABI_VERSION,
        0,
        0,
        0,
        "/bad",
    )
    .is_ok()
    {
        return Err("ckm: invalid component id must be rejected");
    }
    checks += 1;
    if state::register(
        "dupe",
        state::Kind::Userland,
        0,
        crate::ckm::CKM_ABI_VERSION,
        crate::ckm::CKM_ABI_VERSION,
        0,
        0,
        0,
        "/bin/sh",
    )
    .is_ok()
    {
        return Err("ckm: duplicate component target must be rejected");
    }
    checks += 1;
    if state::count() != 3 {
        return Err("ckm: rejected registrations must not change the state table");
    }
    checks += 1;

    crate::driver::serial::print("ckm: selftest PASS (checks=");
    crate::driver::serial::print_dec(checks as u64);
    crate::driver::serial::println(")");
    Ok(())
}

/// 按 `id` 找槽位下标（自测用；`component_state` 的顺序与槽位下标不是一回事）。
fn index_of(id: &str) -> Option<usize> {
    for i in 0..crate::ckm::STATE_SLOTS {
        if let Some(slot) = state::read_slot(i) {
            if slot.id_str() == id {
                return Some(i);
            }
        }
    }
    None
}
