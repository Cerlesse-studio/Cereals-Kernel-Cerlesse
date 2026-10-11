//! 内核侧**最小动态链接器**（v0.9 动态链接闭环）
//!
//! 定位（先读边界，再读实现）：`docs/architecture.md` 第 5 节把「完整 ELF 动态链接」
//! 列为**非目标**。v0.9 要的是**最小可验证闭环** —— 让一条真实存在的动态链接路径
//! 在 ring3 里跑通，而不是做一个能跑 glibc 的用户态 `ld.so`。因此本模块的职责是：
//!
//! ```text
//! 主程序 ELF (ET_DYN / PIE)
//!   ├── PT_INTERP  ──→ /lib/ld.so      解析路径 → 从 VFS 读入 → 装载（像任何对象一样重定位）
//!   ├── PT_DYNAMIC ──→ DT_NEEDED ...   每个 .so → /lib/<name> → 读入 → 装载
//!   └── .rela/.rela.plt               R_X86_64_RELATIVE / GLOB_DAT / JUMP_SLOT
//!        │
//!        ▼
//!   与 v0.7 ELF Loader 的集成：ET_DYN 在 `syscall/elf.rs::load` 入口分流到这里；
//!   ET_EXEC 的静态用户程序路径**一行未改**。
//! ```
//!
//! ## 与真实 `ld.so` 的差异（刻意，且是本版的边界）
//!
//! 1. **链接由内核做**：真实系统里 `PT_INTERP` 指向的解释器自己解析 `.dynamic`、
//!    自己 `mmap` 文件、自己查符号。本版把这一步放在内核里，`PT_INTERP` 指向的
//!    解释器**会被装载和重定位，但不被进入**（见 `user/dynhello/ldso.s` 的说明）。
//!    这足以验证 PT_INTERP 解析 + 共享库装载 + 符号重定位，同时避开「用户态链接器
//!    需要一个能 `mmap` 文件的内核」这个鸡生蛋问题（本内核的 `mmap` 只支持匿名页）。
//! 2. **单层 `DT_NEEDED` + 无版本符号**：只按名字线性扫描 `.dynsym`，
//!    不处理 `DT_SONAME` 查找路径、`DT_RPATH/RUNPATH`、版本节（`.gnu.version*`）、
//!    `DT_REL`（x86_64 上都是 `RELA`）、`RELR`、`INIT/FINI`、TLS。
//! 3. **固定的库搜索路径**：`DT_NEEDED` 的名字一律拼成 `/lib/<name>`（本内核没有
//!    `LD_LIBRARY_PATH`/`rpath` 的概念）。
//!
//! ## 地址布局
//!
//! - 主程序（PIE）固定装载在 [`USER_VIRT_BASE`]（映像窗口，与 v0.7 的静态程序同址，
//!   因此 `e_entry`、`argv` 栈、`/init` 等既有路径完全不变）；
//! - 解释器与每个 `.so` 依次串排在[共享库窗口](crate::syscall::address_space::USER_DYN_BASE)，
//!   每个对象拿一段**页对齐**的连续虚拟地址，装载基址 = 链接基址（0）+ 该段起点。
//!
//! ## 为什么读符号/重定位都走「物理地址」
//!
//! 装载发生在**内核 CR3**、切换用户 CR3 之前（与 v0.7 的 ELF Loader 一致）。
//! 此时用户页已经映射好，但当前 CR3 是内核的，所以要访问用户虚拟地址只能经
//! [`UserSpace::phys_of`] 反查物理页（内核恒等映射物理内存）。`write` 重定位目标
//! 同理 —— 这也是为什么这一段代码看起来比用户态链接器“啰嗦”。

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::driver::serial;
use crate::fs::vfs;
use crate::fs::vfs::{ENOENT, ENOEXEC};
use crate::memory::page::{align_down, align_up, PAGE_SIZE};
use crate::syscall::address_space::{
    LoadStats, UserSpace, USER_DYN_BASE, USER_DYN_MAX, USER_DYN_PAGES, USER_IMAGE_MAX,
    USER_VIRT_BASE,
};
use crate::syscall::elf::{u16_at, u32_at, u64_at, EHDR_SIZE, PHDR_SIZE};

// ---- 程序头类型 ----
const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_INTERP: u32 = 3;
// ---- p_flags ----
const PF_X: u32 = 1;
const PF_W: u32 = 2;
// ---- .dynamic 标签（只列本模块会用到的） ----
const DT_NULL: u64 = 0;
const DT_NEEDED: u64 = 1;
const DT_PLTRELSZ: u64 = 2;
const DT_HASH: u64 = 4;
const DT_STRTAB: u64 = 5;
const DT_SYMTAB: u64 = 6;
const DT_RELA: u64 = 7;
const DT_RELASZ: u64 = 8;
const DT_RELAENT: u64 = 9;
const DT_SYMENT: u64 = 11;
const DT_PLTREL: u64 = 0x14;
const DT_JMPREL: u64 = 0x17;
/// `DT_PLTREL` 的取值：`REL`（不支持，遇到即拒绝，而不是静默跳过）。
const DT_REL_KIND: u64 = 17;
// ---- x86_64 重定位类型 ----
const R_X86_64_64: u32 = 1;
const R_X86_64_GLOB_DAT: u32 = 6;
const R_X86_64_JUMP_SLOT: u32 = 7;
const R_X86_64_RELATIVE: u32 = 8;
/// 一次装载允许的对象数上限（主程序 + 解释器 + 少量库；也挡住 `DT_NEEDED` 环）。
const MAX_OBJECTS: usize = 8;
/// 符号名长度上限（挡住损坏的 `.dynstr`，不做无界读）。
const MAX_SYM_NAME: usize = 128;
/// 默认的结构体大小（`DT_SYMENT` / `DT_RELAENT` 缺失时按 24 兜底 —— ELF64 固定值）。
const ELF64_STRUCT_SIZE: u64 = 24;

/// 一个 `PT_LOAD` 段。
#[derive(Clone, Copy)]
struct Segment {
    vaddr: u64,
    offset: u64,
    filesz: u64,
    memsz: u64,
    flags: u32,
}

/// 一个对象的 `.dynamic` 信息（未填的标签保持 0）。
#[derive(Clone, Copy, Default)]
struct DynamicInfo {
    strtab: u64,
    symtab: u64,
    syment: u64,
    rela: u64,
    relasz: u64,
    relaent: u64,
    jmprel: u64,
    pltrelsz: u64,
    pltrel: u64,
    hash: u64,
}

/// 一个已装载的对象（主程序 / 解释器 / 共享库）。
struct Object {
    /// 装载基址（链接基址 0 + 该对象窗口段的起点）
    base: u64,
    /// `.dynsym` 表项数（`DT_HASH.nchain`，无 hash 时由 `strtab-symtab` 反推）
    nsym: usize,
    /// `.dynamic` 信息
    dyn_: DynamicInfo,
    /// 日志名（主程序为 `<main>`，库为 `/lib/xxx.so`）
    name: String,
}

/// 映射窗口：主程序进映像窗口，其余对象进共享库窗口。
enum Window {
    Image,
    Dyn { cursor: usize },
}

/// ELF 头字段（本模块需要的部分）。
struct Header {
    entry: u64,
    phoff: usize,
    phentsize: usize,
    phnum: usize,
}

/// 校验 ELF 头并取出本模块需要的字段（`ET_DYN` / x86_64 / 小端）。
fn parse_header(elf: &[u8]) -> Result<Header, i64> {
    if elf.len() < EHDR_SIZE {
        return Err(reject("file shorter than ELF64 header"));
    }
    if &elf[..4] != b"\x7fELF" {
        return Err(reject("bad magic"));
    }
    if elf[4] != 2 {
        return Err(reject("not ELF64 (EI_CLASS)"));
    }
    if elf[5] != 1 {
        return Err(reject("not little-endian (EI_DATA)"));
    }
    if u16_at(elf, 18) != Some(62) {
        return Err(reject("not x86_64 (e_machine)"));
    }
    if u16_at(elf, 16) != Some(3) {
        return Err(reject("not ET_DYN (dynamic loader only handles ET_DYN)"));
    }
    let entry = u64_at(elf, 24).ok_or_else(|| reject("bad e_entry"))?;
    let phoff = u64_at(elf, 32).ok_or_else(|| reject("bad e_phoff"))? as usize;
    let phentsize = u16_at(elf, 54).ok_or_else(|| reject("bad e_phentsize"))? as usize;
    let phnum = u16_at(elf, 56).ok_or_else(|| reject("bad e_phnum"))? as usize;
    if phentsize != PHDR_SIZE {
        return Err(reject("unexpected program header size"));
    }
    if phnum == 0 || phoff + phnum * phentsize > elf.len() {
        return Err(reject("program headers outside file"));
    }
    Ok(Header {
        entry,
        phoff,
        phentsize,
        phnum,
    })
}

/// 入口：装载一个 `ET_DYN`（PIE）动态程序（由 [`crate::syscall::elf::load`] 分流进来）。
pub fn load(space: &mut UserSpace, elf: &[u8]) -> Result<LoadStats, i64> {
    let header = parse_header(elf)?;

    // 1. 解析程序头：PT_LOAD 段、PT_INTERP 路径、PT_DYNAMIC 文件偏移
    let mut segments: Vec<Segment> = Vec::new();
    let mut interp: Option<Vec<u8>> = None;
    let mut dynamic_off: Option<usize> = None;
    for i in 0..header.phnum {
        let ph = header.phoff + i * header.phentsize;
        match u32_at(elf, ph).unwrap_or(0) {
            PT_LOAD => {
                let flags = u32_at(elf, ph + 4).unwrap_or(0);
                let offset = u64_at(elf, ph + 8).unwrap_or(0);
                let vaddr = u64_at(elf, ph + 16).unwrap_or(0);
                let filesz = u64_at(elf, ph + 32).unwrap_or(0);
                let memsz = u64_at(elf, ph + 40).unwrap_or(0);
                if filesz > memsz {
                    return Err(reject("PT_LOAD p_filesz > p_memsz"));
                }
                if offset + filesz > elf.len() as u64 {
                    return Err(reject("PT_LOAD extends past end of file"));
                }
                segments.push(Segment {
                    vaddr,
                    offset,
                    filesz,
                    memsz,
                    flags,
                });
            }
            PT_INTERP => {
                let offset = u64_at(elf, ph + 8).unwrap_or(0) as usize;
                let filesz = u64_at(elf, ph + 32).unwrap_or(0) as usize;
                let bytes = elf
                    .get(offset..offset + filesz)
                    .ok_or_else(|| reject("PT_INTERP outside file"))?;
                let nul = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
                interp = Some(bytes[..nul].to_vec());
            }
            PT_DYNAMIC => {
                dynamic_off = Some(u64_at(elf, ph + 8).unwrap_or(0) as usize);
            }
            _ => {}
        }
    }
    if segments.is_empty() {
        return Err(reject("no PT_LOAD segment"));
    }
    let Some(interp) = interp else {
        return Err(reject("ET_DYN program without PT_INTERP"));
    };
    let Some(dynamic_off) = dynamic_off else {
        return Err(reject("ET_DYN program without PT_DYNAMIC"));
    };
    let interp_path = core::str::from_utf8(&interp)
        .map_err(|_| reject("PT_INTERP is not valid UTF-8"))?
        .to_string();
    serial::print("dyn: PT_INTERP=");
    serial::println(&interp_path);

    // 2. 主程序：装载在映像窗口（USER_VIRT_BASE），与静态程序同址
    let (main_base, main_pages, mut writable_pages, mut bytes, mut seg_count) =
        map_object(space, elf, &segments, Window::Image)?;
    let (main_dyn, main_needed) = parse_dynamic(elf, dynamic_off)?;
    let main_nsym = nsym_of(space, main_base, &main_dyn);
    log_object("<main>", main_base, segments.len(), main_pages);

    let mut objects: Vec<Object> = Vec::new();
    objects.push(Object {
        base: main_base,
        nsym: main_nsym,
        dyn_: main_dyn,
        name: String::from("<main>"),
    });

    // 3. 解释器 + DT_NEEDED 共享库：依次进共享库窗口
    let mut cursor = 0usize;
    load_shared(space, &interp_path, &mut cursor, &mut objects, &mut bytes, &mut seg_count, &mut writable_pages)?;
    for off in &main_needed {
        let name = read_dynstr(space, main_base, &main_dyn, *off)?;
        let path = String::from("/lib/") + name.as_str();
        serial::print("dyn: NEEDED ");
        serial::print(&name);
        serial::print(" -> ");
        serial::println(&path);
        load_shared(space, &path, &mut cursor, &mut objects, &mut bytes, &mut seg_count, &mut writable_pages)?;
    }

    // 4. 重定位：对每个对象处理 .rela.dyn（DT_RELA）与 .rela.plt（DT_JMPREL）
    let relocs = relocate_all(space, &objects)?;

    let entry = main_base + header.entry;
    let pages = space.image_pages() + space.dyn_page_count();
    serial::print("dyn: link PASS objects=");
    serial::print_dec(objects.len() as u64);
    serial::print(" relocs=");
    serial::print_dec(relocs as u64);
    serial::print(" entry=");
    serial::print_hex(entry);
    serial::println("");

    Ok(LoadStats {
        entry,
        segments: seg_count,
        bytes,
        pages,
        writable_pages,
    })
}

/// 装载一个共享对象（解释器或 `.so`）：读文件 → 解析 → 映射 → 记录 → 递归处理它的 `DT_NEEDED`。
#[allow(clippy::too_many_arguments)]
fn load_shared(
    space: &mut UserSpace,
    path: &str,
    cursor: &mut usize,
    objects: &mut Vec<Object>,
    bytes: &mut u64,
    seg_count: &mut usize,
    writable_pages: &mut usize,
) -> Result<(), i64> {
    if objects.len() >= MAX_OBJECTS {
        return Err(reject("too many dynamic objects (DT_NEEDED cycle?)"));
    }
    let elf = match vfs::read_all(path) {
        Ok(bytes) => bytes,
        Err(_) => {
            serial::print("dyn: cannot read ");
            serial::println(path);
            return Err(ENOENT);
        }
    };
    if elf.len() < EHDR_SIZE || &elf[..4] != b"\x7fELF" {
        return Err(reject("shared object is not an ELF"));
    }
    if u16_at(&elf, 16) != Some(3) {
        return Err(reject("shared object is not ET_DYN"));
    }

    // 解析程序头（只要 PT_LOAD 与 PT_DYNAMIC）
    let phoff = u64_at(&elf, 32).unwrap_or(0) as usize;
    let phentsize = u16_at(&elf, 54).unwrap_or(0) as usize;
    let phnum = u16_at(&elf, 56).unwrap_or(0) as usize;
    let mut segments: Vec<Segment> = Vec::new();
    let mut dynamic_off: Option<usize> = None;
    for i in 0..phnum {
        let ph = phoff + i * phentsize;
        match u32_at(&elf, ph).unwrap_or(0) {
            PT_LOAD => segments.push(Segment {
                vaddr: u64_at(&elf, ph + 16).unwrap_or(0),
                offset: u64_at(&elf, ph + 8).unwrap_or(0),
                filesz: u64_at(&elf, ph + 32).unwrap_or(0),
                memsz: u64_at(&elf, ph + 40).unwrap_or(0),
                flags: u32_at(&elf, ph + 4).unwrap_or(0),
            }),
            PT_DYNAMIC => dynamic_off = Some(u64_at(&elf, ph + 8).unwrap_or(0) as usize),
            _ => {}
        }
    }
    if segments.is_empty() {
        return Err(reject("shared object without PT_LOAD"));
    }

    let (base, pages, w, b, s) = map_object(space, &elf, &segments, Window::Dyn { cursor: *cursor })?;
    if *cursor + pages > USER_DYN_PAGES {
        return Err(reject("shared library window exhausted"));
    }
    *bytes += b;
    *seg_count += s;
    *writable_pages += w;

    let (dyn_info, needed) = match dynamic_off {
        Some(off) => parse_dynamic(&elf, off)?,
        None => (DynamicInfo::default(), Vec::new()),
    };
    let name = path.to_string();
    let nsym = nsym_of(space, base, &dyn_info);
    log_object(&name, base, segments.len(), pages);
    objects.push(Object {
        base,
        nsym,
        dyn_: dyn_info,
        name,
    });
    *cursor += pages;

    // 递归：库自己的 DT_NEEDED（最小闭环里 libgreet.so 没有，但语义要完整）
    for off in &needed {
        let dep = read_dynstr(space, base, &dyn_info, *off)?;
        let dep_path = String::from("/lib/") + dep.as_str();
        load_shared(space, &dep_path, cursor, objects, bytes, seg_count, writable_pages)?;
    }
    Ok(())
}

/// 把一个对象的 `PT_LOAD` 段按页映射到 `window`，返回 `(base, pages, writable_pages, bytes, segments)`。
fn map_object(
    space: &mut UserSpace,
    elf: &[u8],
    segments: &[Segment],
    window: Window,
) -> Result<(u64, usize, usize, u64, usize), i64> {
    let base = match window {
        Window::Image => USER_VIRT_BASE,
        Window::Dyn { cursor } => USER_DYN_BASE + cursor as u64 * PAGE_SIZE,
    };
    let image_limit = match window {
        Window::Image => USER_IMAGE_MAX,
        Window::Dyn { .. } => USER_DYN_MAX,
    };

    let mut max_end = 0u64;
    let mut writable = 0usize;
    let mut bytes = 0u64;
    for seg in segments {
        bytes += seg.memsz;
        let start = align_down(seg.vaddr, PAGE_SIZE);
        let end = align_up(seg.vaddr + seg.memsz, PAGE_SIZE);
        // `end` 是对象内的相对虚拟地址（链接基址 0），窗口上限也是相对量
        if end > image_limit {
            return Err(reject("segment outside the load window"));
        }
        max_end = max_end.max(end);
        let mut page = start;
        while page < end {
            let local = (page / PAGE_SIZE) as usize;
            let flags = page_flags(seg.flags);
            let phys = match window {
                Window::Image => space.map_image_page(local, flags)?,
                Window::Dyn { cursor } => space.map_dyn_page(cursor + local, flags)?,
            };
            if seg.flags & PF_W != 0 {
                writable += 1;
            }
            // 该段落在本页内的文件字节区间
            let copy_start = seg.vaddr.max(page);
            let copy_end = (seg.vaddr + seg.filesz).min(page + PAGE_SIZE);
            if copy_start < copy_end {
                let in_page = (copy_start - page) as usize;
                let file_at = (seg.offset + (copy_start - seg.vaddr)) as usize;
                let len = (copy_end - copy_start) as usize;
                // SAFETY: 物理帧由本空间拥有；内核 CR3 下物理地址恒等可写
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        elf.as_ptr().add(file_at),
                        (phys + in_page as u64) as *mut u8,
                        len,
                    );
                }
            }
            page += PAGE_SIZE;
        }
    }
    let pages = (max_end / PAGE_SIZE) as usize;
    if pages == 0 || pages > image_limit as usize / PAGE_SIZE as usize {
        return Err(reject("object size out of window"));
    }
    Ok((base, pages, writable, bytes, segments.len()))
}

/// 解析 `.dynamic` 数组（从文件字节读，`d_tag`/`d_val` 各 8 字节，`DT_NULL` 结束）。
fn parse_dynamic(elf: &[u8], off: usize) -> Result<(DynamicInfo, Vec<u32>), i64> {
    let mut info = DynamicInfo::default();
    let mut needed = Vec::new();
    let mut at = off;
    while at + 16 <= elf.len() {
        let tag = u64_at(elf, at).unwrap_or(0);
        let val = u64_at(elf, at + 8).unwrap_or(0);
        if tag == DT_NULL {
            break;
        }
        match tag {
            DT_NEEDED => needed.push(val as u32),
            DT_STRTAB => info.strtab = val,
            DT_SYMTAB => info.symtab = val,
            DT_SYMENT => info.syment = val,
            DT_RELA => info.rela = val,
            DT_RELASZ => info.relasz = val,
            DT_RELAENT => info.relaent = val,
            DT_JMPREL => info.jmprel = val,
            DT_PLTRELSZ => info.pltrelsz = val,
            DT_PLTREL => info.pltrel = val,
            DT_HASH => info.hash = val,
            _ => {}
        }
        at += 16;
    }
    // REL（`DT_PLTREL == DT_REL`）不支持：明确拒绝，避免“看起来跑通了但重定位没做”
    if info.pltrel == DT_REL_KIND {
        return Err(reject("DT_PLTREL=REL is not supported (use RELA)"));
    }
    Ok((info, needed))
}

/// `.dynsym` 表项数：优先 `DT_HASH.nchain`，否则由 `(strtab - symtab) / syment` 反推。
fn nsym_of(space: &UserSpace, base: u64, info: &DynamicInfo) -> usize {
    if info.hash != 0 {
        if let Some(n) = read_user_u32(space, base + info.hash + 4) {
            return n as usize;
        }
    }
    let syment = if info.syment == 0 { ELF64_STRUCT_SIZE } else { info.syment };
    if info.strtab > info.symtab && syment != 0 {
        ((info.strtab - info.symtab) / syment) as usize
    } else {
        0
    }
}

/// 对所有对象应用重定位，返回处理的条数。
fn relocate_all(space: &UserSpace, objects: &[Object]) -> Result<usize, i64> {
    let mut total = 0usize;
    for (oi, obj) in objects.iter().enumerate() {
        // .rela.dyn（DT_RELA）
        total += relocate_table(space, objects, oi, obj.dyn_.rela, obj.dyn_.relasz)?;
        // .rela.plt（DT_JMPREL）—— 同样按 24 字节 RELA 处理（DT_PLTREL 已在解析时校验为 RELA）
        if obj.dyn_.jmprel != 0 && obj.dyn_.pltrelsz != 0 {
            total += relocate_table(space, objects, oi, obj.dyn_.jmprel, obj.dyn_.pltrelsz)?;
        }
    }
    Ok(total)
}

/// 应用一张 RELA 表（`table` 是相对对象基址的虚拟地址）。
fn relocate_table(
    space: &UserSpace,
    objects: &[Object],
    oi: usize,
    table: u64,
    size: u64,
) -> Result<usize, i64> {
    if table == 0 || size == 0 {
        return Ok(0);
    }
    let obj = &objects[oi];
    let relaent = if obj.dyn_.relaent == 0 {
        ELF64_STRUCT_SIZE
    } else {
        obj.dyn_.relaent
    };
    let mut count = 0usize;
    let mut off = 0u64;
    while off + relaent <= size {
        let rela = obj.base + table + off;
        let r_offset = read_user_u64(space, rela).ok_or_else(|| reject("bad RELA offset"))?;
        let r_info = read_user_u64(space, rela + 8).ok_or_else(|| reject("bad RELA info"))?;
        let addend = read_user_u64(space, rela + 16).ok_or_else(|| reject("bad RELA addend"))? as i64;
        let rtype = (r_info & 0xffff_ffff) as u32;
        let sym_index = (r_info >> 32) as usize;
        let target = obj.base + r_offset;
        off += relaent;

        let value = match rtype {
            R_X86_64_RELATIVE => {
                // 链接期地址 + 装载基址
                obj.base.wrapping_add(addend as u64)
            }
            R_X86_64_GLOB_DAT | R_X86_64_JUMP_SLOT | R_X86_64_64 => {
                let name = sym_name(space, obj, sym_index)?;
                let (addr, def_index) = resolve(space, objects, &name)
                    .ok_or_else(|| {
                        serial::print("dyn: unresolved symbol ");
                        serial::println(&name);
                        reject("unresolved dynamic symbol")
                    })?;
                serial::print("dyn: reloc ");
                serial::print(rtype_name(rtype));
                serial::print(" sym=");
                serial::print(&name);
                serial::print(" -> obj=");
                serial::print(&objects[def_index].name);
                serial::println("");
                addr.wrapping_add(addend as u64)
            }
            other => {
                serial::print("dyn: skip unknown relocation type=");
                serial::print_dec(other as u64);
                serial::println("");
                continue;
            }
        };

        let phys = space
            .phys_of(target)
            .ok_or_else(|| reject("relocation target is not mapped"))?;
        // SAFETY: 目标落在已映射的用户页里；内核 CR3 下物理地址恒等可写
        unsafe { core::ptr::write_unaligned(phys as *mut u64, value) };
        count += 1;
    }
    Ok(count)
}

/// 取对象里第 `index` 个动态符号的名字（`.dynstr` 里的字符串）。
fn sym_name(space: &UserSpace, obj: &Object, index: usize) -> Result<String, i64> {
    if index == 0 || index >= obj.nsym || obj.dyn_.symtab == 0 {
        return Ok(String::new());
    }
    let entry = obj.base + obj.dyn_.symtab + index as u64 * ELF64_STRUCT_SIZE;
    let name_off = read_user_u32(space, entry).ok_or_else(|| reject("bad dynsym entry"))?;
    read_user_str(space, obj.base + obj.dyn_.strtab + name_off as u64, MAX_SYM_NAME)
}

/// 在所有对象里按名字找**已定义**的符号，返回 `(绝对地址, 定义所在对象下标)`。
fn resolve(space: &UserSpace, objects: &[Object], name: &str) -> Option<(u64, usize)> {
    if name.is_empty() {
        return None;
    }
    for (oi, obj) in objects.iter().enumerate() {
        if obj.dyn_.symtab == 0 || obj.nsym == 0 {
            continue;
        }
        for i in 1..obj.nsym {
            let entry = obj.base + obj.dyn_.symtab + i as u64 * ELF64_STRUCT_SIZE;
            let (Some(st_name), Some(st_shndx), Some(st_value)) = (
                read_user_u32(space, entry),
                read_user_u16(space, entry + 6),
                read_user_u64(space, entry + 8),
            ) else {
                continue;
            };
            if st_name == 0 || st_shndx == 0 {
                continue; // 无名或未定义（SHN_UNDEF）——只认定义
            }
            let candidate = read_user_str(space, obj.base + obj.dyn_.strtab + st_name as u64, MAX_SYM_NAME)
                .unwrap_or_default();
            if candidate == name {
                return Some((obj.base + st_value, oi));
            }
        }
    }
    None
}

/// 读 `.dynstr` 里偏移 `off` 处的 NUL 结尾字符串（`DT_NEEDED` 的名字）。
fn read_dynstr(space: &UserSpace, base: u64, info: &DynamicInfo, off: u32) -> Result<String, i64> {
    read_user_str(space, base + info.strtab + off as u64, MAX_SYM_NAME)
}

/// 段权限位 → 页表项标志（与 `elf.rs::page_flags` 同一 W^X 规则）。
fn page_flags(p_flags: u32) -> u64 {
    use crate::arch::x86_64::paging::{NO_EXEC, WRITABLE};
    let mut flags = 0;
    if p_flags & PF_W != 0 {
        flags |= WRITABLE;
    }
    if p_flags & PF_X == 0 {
        flags |= NO_EXEC;
    }
    flags
}

/// 重定位类型名（日志可读性）。
fn rtype_name(rtype: u32) -> &'static str {
    match rtype {
        R_X86_64_64 => "R_X86_64_64",
        R_X86_64_GLOB_DAT => "R_X86_64_GLOB_DAT",
        R_X86_64_JUMP_SLOT => "R_X86_64_JUMP_SLOT",
        R_X86_64_RELATIVE => "R_X86_64_RELATIVE",
        _ => "?",
    }
}

/// `dyn: load <name> base=<hex> segments=<n> pages=<n>`。
fn log_object(name: &str, base: u64, segments: usize, pages: usize) {
    serial::print("dyn: load ");
    serial::print(name);
    serial::print(" base=");
    serial::print_hex(base);
    serial::print(" segments=");
    serial::print_dec(segments as u64);
    serial::print(" pages=");
    serial::print_dec(pages as u64);
    serial::println("");
}

/// 统一拒绝出口（串口留痕 + `-ENOEXEC`），与 `elf.rs::reject` 一致。
fn reject(reason: &str) -> i64 {
    serial::print("dyn: rejected: ");
    serial::println(reason);
    ENOEXEC
}

// ---- 用户虚拟地址读取（内核 CR3 下经 `phys_of` 反查；逐字节，绝不越页读） ----

fn read_user_u16(space: &UserSpace, virt: u64) -> Option<u16> {
    let mut raw = [0u8; 2];
    for (i, slot) in raw.iter_mut().enumerate() {
        *slot = read_user_u8(space, virt + i as u64)?;
    }
    Some(u16::from_le_bytes(raw))
}

fn read_user_u32(space: &UserSpace, virt: u64) -> Option<u32> {
    let mut raw = [0u8; 4];
    for (i, slot) in raw.iter_mut().enumerate() {
        *slot = read_user_u8(space, virt + i as u64)?;
    }
    Some(u32::from_le_bytes(raw))
}

fn read_user_u64(space: &UserSpace, virt: u64) -> Option<u64> {
    let mut raw = [0u8; 8];
    for (i, slot) in raw.iter_mut().enumerate() {
        *slot = read_user_u8(space, virt + i as u64)?;
    }
    Some(u64::from_le_bytes(raw))
}

fn read_user_u8(space: &UserSpace, virt: u64) -> Option<u8> {
    let phys = space.phys_of(virt)?;
    // SAFETY: phys 由本空间的映射帧算出，内核 CR3 下恒等可读
    Some(unsafe { core::ptr::read_volatile(phys as *const u8) })
}

/// 读一个 NUL 结尾字符串（超过 `max` 字节即截断，避免被坏数据牵着走）。
fn read_user_str(space: &UserSpace, virt: u64, max: usize) -> Result<String, i64> {
    let mut out = String::new();
    for i in 0..max {
        let Some(byte) = read_user_u8(space, virt + i as u64) else {
            break;
        };
        if byte == 0 {
            break;
        }
        out.push(byte as char);
    }
    if out.is_empty() {
        return Err(reject("empty string in .dynstr"));
    }
    Ok(out)
}
