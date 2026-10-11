# Cerlesse OS — 构建入口（v0.1 实现）
# 详见 DEVELOPMENT.md 第 5、7 节。
#
# v0.6 起用户程序（user/*，见 libc/）由 kernel/build.rs 在内核构建时自动重建，
# 因此 image/test-* 都不需要额外的先后顺序；`make user` 只用于单独验证脚手架。
#
# v0.7：用户程序不再被摊平，而是以 **ELF 原样内嵌**（`$OUT_DIR/user_hello.elf`），
# 启动时写进 RAMFS 的 `/init`，由内核 ELF Loader 装载 —— 所以 `make user` 会跑
# tools/check_user_elf.py 校验段布局（基址/页对齐/W^X/映像窗口/无重定位）。

CARGO ?= cargo
QEMU  ?= qemu-system-x86_64
PYTHON ?= python3
GO    ?= go

BUILD := build
OVMF_CODE ?= /usr/share/OVMF/OVMF_CODE.fd
OVMF_VARS ?= /usr/share/OVMF/OVMF_VARS.fd

BOOT_EFI := target/x86_64-unknown-uefi/debug/boot.efi
KERNEL_ELF := target/x86_64-unknown-none/debug/kernel

# v0.6 用户程序：链接在 2^39（USER_VIRT_BASE），small 代码模型会产生
# R_X86_64_32S 重定位而链接失败，必须用 large 代码模型；large 模型与 LTO
# 不兼容，所以用 workspace 的 user-release profile（lto = false）。
# 这三个取值与 kernel/build.rs 的 USER_* 常量成对维护，改一处要同步另一处。
USER_RUSTFLAGS := -C relocation-model=static -C code-model=large
USER_PROFILE   := user-release
USER_ELF       := $(BUILD)/user/x86_64-unknown-none/$(USER_PROFILE)/user-hello
# v0.9：第二个用户程序 —— 交互式 Shell（RAMFS 上落在 /bin/sh，见 kernel/build.rs）。
USER_SHELL_ELF := $(BUILD)/user/x86_64-unknown-none/$(USER_PROFILE)/user-shell
# v0.9 第三步：外部程序（RAMFS 上落在 /bin/hello，由 Shell fork+execve 起来）。
USER_HELLO_BIN_ELF := $(BUILD)/user/x86_64-unknown-none/$(USER_PROFILE)/user-hello-bin

.PHONY: help build boot kernel image image-exception image-pagefault fat-image run test test-exception test-memory test-pagefault test-keyboard test-shell test-proc test-scheduler test-ipc test-fs test-user test-driver test-fat test-all user test-pkg go-test test-orch clean

help:
	@echo "targets:"
	@echo "  build  - build bootloader + kernel + disk image"
	@echo "  run    - boot the image in QEMU (serial on stdio)"
	@echo "  test   - integration test: assert 'Kernel started!' on serial"
	@echo "  test-exception - v0.2: trigger #DE, assert exception diagnostics"
	@echo "  test-memory - v0.3/v0.4: assert frame/heap/paging/context-switch self-tests"
	@echo "  test-pagefault - v0.3: touch unmapped address, assert #PF diagnostics"
	@echo "  test-keyboard - v0.4/v0.9: inject keys via QEMU monitor, assert IRQ1 markers + line buffer + PIT heartbeat"
	@echo "  test-shell - v0.9: inject keys into the interactive shell (help / cat /etc/motd / exit) via QEMU monitor"
	@echo "  test-proc - v0.9: fork/execve/wait4 through the shell: external program + argv + missing program + reap"
	@echo "  test-scheduler - v0.5: assert >=3 kernel tasks rotate under PIT time slices"
	@echo "  test-ipc - v0.5: assert pipe create/write/read summary logs + selftest"
	@echo "  test-fs - v0.7: assert VFS mount routing + RAMFS read/write/ls + negative errno cases"
	@echo "  test-user - v0.7/v0.9: assert ELF loaded from RAMFS + ring3 file syscalls + user heap + isolation + shell"
	@echo "  test-driver - v0.8: assert PCI enumeration + driver probe + virtio-blk sector read/write"
	@echo "  fat-image - v0.8: build the FAT test volume (tools/mkfs/mkfs.py from tests/fatroot/data)"
	@echo "  test-fat - v0.8: assert FAT16/32 on disk mounted in VFS: read/ls/LFN/multi-cluster/EROFS"
	@echo "  user   - v0.7/v0.9: build+validate the user-space scaffold alone (libc + user/hello + user/shell + user/hello-bin)"
	@echo "  test-pkg - H-06: assert the .ckc package container (CKC) format + packaging rules (host-side)"
	@echo "  test-all - run every integration test"

	@echo "  go-test  - v0.4 H-04/H-05: Go unit tests for tools-go (host side)"
	@echo "  test-orch - v0.4 H-04: run all 12 scenarios in parallel via Go orchestrator"
	@echo "  clean  - remove build artifacts"

build: image

boot:
	$(CARGO) build -p boot --target x86_64-unknown-uefi

kernel:
	$(CARGO) build -p kernel --target x86_64-unknown-none

image: boot kernel
	$(PYTHON) tools/image-builder/build_image.py \
		--boot $(BOOT_EFI) \
		--kernel $(KERNEL_ELF) \
		--out $(BUILD)/disk.img

run: image
	cp $(OVMF_VARS) $(BUILD)/OVMF_VARS.fd
	$(QEMU) -machine q35 -m 512M \
		-drive if=pflash,format=raw,readonly=on,file=$(OVMF_CODE) \
		-drive if=pflash,format=raw,file=$(BUILD)/OVMF_VARS.fd \
		-drive if=none,id=blk0,format=raw,file=$(BUILD)/disk.img \
		-device virtio-blk-pci,drive=blk0,disable-modern=on \
		-serial stdio -display none -no-reboot

test: image
	$(PYTHON) tests/test_boot.py

image-exception: boot
	$(CARGO) build -p kernel --target x86_64-unknown-none --features exception-test
	$(PYTHON) tools/image-builder/build_image.py \
		--boot $(BOOT_EFI) \
		--kernel $(KERNEL_ELF) \
		--out $(BUILD)/disk-exception.img

image-pagefault: boot
	$(CARGO) build -p kernel --target x86_64-unknown-none --features pagefault-test
	$(PYTHON) tools/image-builder/build_image.py \
		--boot $(BOOT_EFI) \
		--kernel $(KERNEL_ELF) \
		--out $(BUILD)/disk-pagefault.img

test-exception: image-exception
	$(PYTHON) tests/test_boot.py \
		--disk $(BUILD)/disk-exception.img \
		--serial-log $(BUILD)/serial-exception.log \
		--expect "Kernel started!" \
		--expect "EXCEPTION: divide error"

# v0.3/v0.4 内核自测验收：帧分配器 + 内核堆 + 页表 Mapper + 上下文切换原语
test-memory: image
	$(PYTHON) tests/test_boot.py \
		--serial-log $(BUILD)/serial-memory.log \
		--expect "Kernel started!" \
		--expect "heap: Box/Vec/String PASS" \
		--expect "frame: stress PASS" \
		--expect "paging: map/unmap PASS" \
		--expect "context: switch PASS"

# v0.3 验收：页错误诊断（feature 门控注入未映射地址访问）
test-pagefault: image-pagefault
	$(PYTHON) tests/test_boot.py \
		--disk $(BUILD)/disk-pagefault.img \
		--serial-log $(BUILD)/serial-pagefault.log \
		--expect "Kernel started!" \
		--expect "EXCEPTION: page fault" \
		--expect "CR2=0x500000000000" \
		--expect "KERNEL PANIC: unhandled exception"

test-all: test test-exception test-memory test-pagefault test-keyboard test-shell test-proc test-scheduler test-ipc test-fs test-user test-driver test-fat

# v0.5 验收：≥3 个内核任务在 PIT 时间片驱动下轮转（Round Robin）
test-scheduler: image
	$(PYTHON) tests/test_boot.py \
		--serial-log $(BUILD)/serial-sched.log \
		--expect "Kernel started!" \
		--expect "sched: spawn pid=1 name=task_a" \
		--expect "task_a: entered" \
		--expect "task_b: entered" \
		--expect "task_c: entered" \
		--expect "sched: switch pid=3 -> 0" \
		--expect "sched: round-robin wrap PASS"

# v0.7 验收：VFS（挂载点路由 / RAMFS 读写 / 目录列举 / 负例 errno）。
#
# 断言里的数字都是「内核真的做过这件事」的证据而不是日志措辞：
#   /init 的字节数来自内嵌 ELF（断言只写前缀，避免用户程序一改就失效），
#   /etc/motd=23 字节并打印内容，/mnt 走的是**另一个**后端（ramfs-scratch）。
test-fs: image
	$(PYTHON) tests/test_boot.py \
		--serial-log $(BUILD)/serial-fs.log \
		--expect "Kernel started!" \
		--expect "fs: mount point=/ fs=ramfs" \
		--expect "fs: create /init" \
		--expect "fs: write /init bytes=" \
		--expect "fs: mkdir /etc" \
		--expect "fs: create /etc/motd" \
		--expect "fs: write /etc/motd bytes=23" \
		--expect "fs: mkdir /tmp" \
		--expect "fs: stat /etc/motd kind=file size=23 perms=-rw-r--r--" \
		--expect "fs: read /etc/motd len=23 data=motd: hello from ramfs" \
		--expect "fs: mount point=/mnt fs=ramfs-scratch" \
		--expect "fs: route /mnt/note.txt fs=ramfs-scratch" \
		--expect "fs: readback /mnt/note.txt len=7 data=v0.7-ok" \
		--expect "fs: ls / entries=4" \
		--expect "fs: ls / name=init kind=file size=" \
		--expect "fs: ls / name=bin kind=dir size=0 perms=drwxr-xr-x" \
		--expect "fs: ls / name=etc kind=dir size=0 perms=drwxr-xr-x" \
		--expect "fs: ls / name=tmp kind=dir size=0 perms=drwxr-xr-x" \
		--expect "fs: negative cases PASS" \
		--expect "fs: selftest PASS"

# v0.7 验收：syscall（syscall/sysret + EFER/STAR/LSTAR/FMASK）+ 用户态 ring3 + 地址空间隔离
# + **ELF Loader 从 RAMFS 加载 /init** + 文件 syscall（open/read/close/write）。
# v0.9 第一步追加**用户堆**（brk/mmap/munmap + `#[global_allocator]`）：
#   - 地址是**可算的**，所以直接断言：brk 起点 0x8000020000（增长两页到 0x8000022000）、
#     mmap 窗口首次分配落在 0x8000060000（first-fit）；
#   - `hello: heap zeroed=1` / `hello: mmap zeroed=1`：匿名内存不得泄漏上一任内容；
#   - `user: write fd=1 len=26` + `hello: heap write ret=26`：把**堆上**的缓冲交给内核写串口，
#     证明 `write` 的按页缓冲校验（`page_flags`）认可堆页而不只是映像/栈；
#   - 每个负例都要**具体**的 errno：文件映射/`MAP_SHARED`/W^X/未对齐 munmap = -22，超窗口 = -12；
#   - `hello: vec len=200 sum=20100`：`Vec`/`format!` 真的走通了全局分配器 → 内核 mmap；
#   - `user: heap frame reclamation PASS`：两次运行的地址空间（含堆页）整棵归还，不成帧泄漏。
#
# 两个程序的来源不同，断言里保留了这个区别：
#   /init —— `user/hello` 编出的 **ELF**：内核从 RAMFS 读它，由 elf.rs 按段权限装载，
#            write/getpid/open/read/close/nanosleep/exit 全链路往返，exit code=42；
#   probe —— 内嵌位置无关机器码（map_raw），直接读内核映像地址（0x2000000），
#            必须被 ring3 #PF 拦下（error=P|U）。
# v0.9 第二步追加**交互式 Shell**（`/bin/sh` = `user/shell`）：命令来自键盘行缓冲（stdin, fd 0），
#   没人按键时它会两次 `-EAGAIN` 后自行退出 —— 因此这条非交互路径断言的是
#   `user: program=/bin/sh source=elf` 与 `user: shell PASS`（交互路径见 `make test-shell`）。
# 硬证据（不是日志措辞）：
#   - `source=elf` + `segments=4 image_pages=7 writable_pages=2`：ELF 装载真的发生了，
#     且 RW 段（.data/.bss）确实被映射成可写；
#   - `user: read fd=3 want=64 got=23` 紧跟 `got=0`：fd 偏移真的在推进（EOF 语义）；
#   - `fs: readback /tmp/hello.txt ... written by user program`：用户在 ring3 写的内容
#     被内核从 RAMFS 读回来（跨态数据往返）；
#   - `hello: static counter=1`：RW 段对 ring3 可写（v0.6 会 #PF）；
#   - `user: EFAULT write ptr=0xdead00000000` / `bad write ret=-14`：用户指针被校验；
#   - `FAULT cr2=0x2000000 error=0x5`：越权读内核页被拦下（error 变了说明触发的不是权限违例）。
test-user: image
	$(PYTHON) tests/test_boot.py \
		--serial-log $(BUILD)/serial-user.log \
		--expect "Kernel started!" \
		--expect "syscall: syscall/sysret entry armed" \
		--expect "sched: suspended for user-mode run" \
		--expect "user: selftest begin" \
		--expect "user: program=/init source=elf" \
		--expect "segments=4 image_pages=7 writable_pages=2" \
		--expect "user: enter name=init pid=1 entry=0x8000000000" \
		--expect "hello from user space" \
		--expect "user: write fd=1 len=22" \
		--expect "user: getpid -> 1" \
		--expect "user: open path=/etc/motd flags=0x0 fd=3" \
		--expect "user: read fd=3 want=64 got=23" \
		--expect "motd: hello from ramfs" \
		--expect "user: read fd=3 want=64 got=0" \
		--expect "user: close fd=3" \
		--expect "user: open path=/tmp/hello.txt flags=0x41 fd=3" \
		--expect "user: write fd=3 len=24" \
		--expect "hello: static counter=1" \
		--expect "user: EFAULT write ptr=0xdead00000000 len=1" \
		--expect "hello: bad write ret=-14" \
		--expect "user: brk query -> 0x8000020000" \
		--expect "user: brk addr=0x8000022000 -> 0x8000022000 pages=2" \
		--expect "user: program=/bin/sh source=elf" \
		--expect "user: shell PASS (keyboard line buffer -> stdin -> builtins + external program)" \
		--expect "hello: brk grown=0x8000022000" \
		--expect "hello: heap zeroed=1" \
		--expect "hello: heap pattern=1" \
		--expect "user: brk rejected addr=0x8000120000" \
		--expect "hello: brk over ret=0x8000022000 refused=1" \
		--expect "user: mmap len=8192 prot=0x3 flags=0x22 -> 0x8000060000 pages=2" \
		--expect "hello: mmap zeroed=1" \
		--expect "hello: mmap pattern=1" \
		--expect "user: write fd=1 len=26" \
		--expect "heap buffer through write" \
		--expect "hello: heap write ret=26" \
		--expect "user: munmap addr=0x8000060000 len=8192" \
		--expect "hello: munmap ret=0" \
		--expect "user: mmap rejected (file-backed mmap is not supported" \
		--expect "hello: mmap file-backed ret=-22" \
		--expect "hello: mmap shared ret=-22" \
		--expect "user: mmap errno=12 len=266240" \
		--expect "hello: mmap too big ret=-12" \
		--expect "hello: mmap wx ret=-22" \
		--expect "hello: munmap misaligned ret=-22" \
		--expect "hello: munmap twice ret=-22" \
		--expect "hello: vec len=200 sum=20100" \
		--expect "hello: heap string bytes=200" \
		--expect "hello: heap PASS=1" \
		--expect "user: sleep ticks=2" \
		--expect "user: sleep done ticks=2" \
		--expect "user: exit code=42" \
		--expect "user: init PASS" \
		--expect "fs: readback /tmp/hello.txt len=24" \
		--expect "fs: user data: written by user program" \
		--expect "user: enter name=probe pid=2" \
		--expect "user: FAULT vector=0xe error=0x5" \
		--expect "user: FAULT cr2=0x2000000" \
		--expect "user: isolation PASS" \
		--expect "user: probe PASS" \
		--expect "user: heap frame reclamation PASS" \
		--expect "user: selftest PASS"

# v0.8 验收：PCI 枚举 → 驱动注册框架 → virtio-blk 挂接 → 块设备读写。
#
# 断言的都是「硬件事实」而不是日志措辞：
#   - `vendor=0x1af4 device_id=0x1001`：QEMU 在 `disable-modern=on` 下暴露的 **legacy**
#     virtio-blk（modern 是 0x1042，被驱动明确拒绝）；
#   - `bar0=io:`：legacy 接口的标志 —— 寄存器在 **I/O 窗口** 里（modern 只有 MMIO）；
#   - `probe PASS (devices=5 matched=1 attached=1 failed=0)`：枚举到了 5 个 function，
#     其中恰好 1 个有驱动认领且挂接成功（没有驱动报错）；
#   - `capacity=131072 sectors (64 MiB)`：容量是设备报的，与 tools/image-builder 的镜像尺寸互相印证；
#   - `boot sector sig=0x55aa fstype=FAT16`：从磁盘真的读回了本次构建的 FAT 引导扇区；
#   - `chunked read PASS`：4608 字节的读跨过驱动内部 8 扇区搬运边界，与两次独立读逐字节一致；
#   - `write/readback PASS (lba=131071 pattern=0xa5)`：最后一个扇区写回读（随后还原原值）。
test-driver: image
	$(PYTHON) tests/test_boot.py \
		--serial-log $(BUILD)/serial-driver.log \
		--expect "Kernel started!" \
		--expect "pci: enumerated devices=" \
		--expect "vendor=0x1af4 device_id=0x1001" \
		--expect "pci: 00:03.0 bar0=io:" \
		--expect "pci: virtio-blk (legacy) at " \
		--expect "driver: registered index=0 name=virtio-blk-pci" \
		--expect "driver: match " \
		--expect "-> virtio-blk-pci" \
		--expect "virtio-blk: host features=" \
		--expect "virtio-blk: queue0 size=" \
		--expect "virtio-blk: capacity=131072 sectors (64 MiB)" \
		--expect "virtio-blk: registered as block device 0" \
		--expect "driver: attached " \
		--expect "driver: probe PASS (devices=" \
		--expect "matched=1 attached=1 failed=0)" \
		--expect "block: registered devices=1" \
		--expect "block: device=virtio-blk0 sector_size=512 sectors=131072" \
		--expect "block: boot sector sig=0x55aa fstype=FAT16" \
		--expect "block: chunked read PASS" \
		--expect "block: write/readback PASS (lba=131071 pattern=0xa5)" \
		--expect "block: negative cases PASS" \
		--expect "block: selftest PASS"

# v0.8 H-03 验收：用 tools/mkfs/mkfs.py 生成「带已知文件集」的 FAT32 测试盘（64 MiB）。
#
# 内容取自 tests/fatroot/data（见该目录的 README：每个文件对应一类断言）。
# 正确性由 **mtools 独立复核**：`mdir -i build/fat.img ::` / `mtype ... ::hello.txt`。
fat-image:
	$(PYTHON) tools/mkfs/mkfs.py --out $(BUILD)/fat.img --from tests/fatroot/data

# v0.8 验收：磁盘上的文件系统（FAT 后端挂进 VFS 后，syscall 层一行未改就能 open/read）。
#
# 断言的都是「磁盘上的事实」而不是日志措辞：
#   - `fs: mount point=/disk fs=fat16 sectors=131072 cluster_bytes=1024 clusters=65264`：
#     启动盘（mtools 写的 ESP）被 **FAT16 解析器** 认出来（类型由簇数判定，不靠扩展名）；
#   - `fs: read /disk/KERNEL.ELF magic=0x7f454c46`：从磁盘读回**内核自己的映像**，
#     前 4 字节是 ELF 魔数 —— VFS → FAT → 块设备 → virtio 的完整往返；
#   - `BOOTX64.EFI magic=0x4d5a`（`MZ`）：固件要用的 PE 引导文件在同一张盘上读到；
#   - `fs: mount point=/mnt/fat fs=fat32 sectors=131072 ... clusters=128992`：
#     `mkfs.py` 生成的测试盘，且挂在 `/mnt`（RAMFS）**下面** → 顺带验证最长前缀路由；
#   - `fs: content check PASS ... len=3000`：跨簇文件逐字节对公式（6 个 512 字节簇）；
#   - `fs: offset read PASS ... offset=1024`：任意偏移读（fd 的 read 路径形态）；
#   - `empty file PASS len=0`：首簇为 0 的空文件不会去解引用簇 0；
#   - `long file name PASS`：带空格的长文件名（LFN 目录项 + 校验和）；
#   - `fs: disk negative cases PASS (ENOENT/ENOTDIR/EISDIR/EROFS)`：错误各自具体。
test-fat: image fat-image
	$(PYTHON) tests/test_boot.py \
		--disk $(BUILD)/disk.img \
		--disk2 $(BUILD)/fat.img \
		--serial-log $(BUILD)/serial-fat.log \
		--expect "Kernel started!" \
		--expect "fs: mount point=/disk fs=fat16 sectors=131072 cluster_bytes=1024 clusters=65264" \
		--expect "fs: disk selftest begin" \
		--expect "fs: ls /disk entries=2" \
		--expect "fs: ls /disk name=EFI kind=dir size=0 perms=drwxr-xr-x" \
		--expect "fs: ls /disk name=KERNEL.ELF kind=file size=" \
		--expect "fs: read /disk/KERNEL.ELF magic=0x7f454c46 size=" \
		--expect "fs: read /disk/EFI/BOOT/BOOTX64.EFI magic=0x4d5a route=fat16" \
		--expect "fs: mount point=/mnt/fat fs=fat32 sectors=131072 cluster_bytes=512 clusters=128992" \
		--expect "fs: second volume at /mnt/fat fs=fat32" \
		--expect "fs: ls /mnt/fat entries=4" \
		--expect "fs: read /mnt/fat/hello.txt len=17 data=hello from FAT32" \
		--expect "fs: content check PASS /mnt/fat/dir/nested.bin len=3000 (formula verified byte by byte across 6 clusters)" \
		--expect "fs: offset read PASS /mnt/fat/dir/nested.bin offset=1024 len=8 data=0x" \
		--expect "(matches the formula)" \
		--expect "fs: empty file PASS /mnt/fat/empty.txt len=0" \
		--expect "fs: long file name PASS /mnt/fat/a long file name.txt (LFN entry)" \
		--expect "fs: nested directory PASS /mnt/fat/dir/deep/marker.txt" \
		--expect "fs: route /mnt/note.txt fs=ramfs-scratch /mnt/fat/hello.txt fs=fat32" \
		--expect "fs: disk negative cases PASS (ENOENT/ENOTDIR/EISDIR/EROFS)" \
		--expect "fs: disk selftest PASS"

# v0.5 验收：IPC（pipe 骨架）创建/写入/读取串口摘要日志 + 自测，随 Round Robin 巡回后执行
test-ipc: image
	$(PYTHON) tests/test_boot.py \
		--serial-log $(BUILD)/serial-ipc.log \
		--expect "Kernel started!" \
		--expect "sched: round-robin wrap PASS" \
		--expect "pipe: create id=1 cap=64" \
		--expect "pipe: write id=1 len=10" \
		--expect "pipe: read id=1 len=10" \
		--expect "pipe: selftest PASS"

# v0.4 验收：PIT 心跳递增 + PS/2 键盘 IRQ1 扫描码回显（经 QEMU monitor 注入真实按键）
# v0.9 起回显协议改了（字符原样回显，见 driver/keyboard.rs 模块文档的表），
# 因此这里断言的是「IRQ 真的进了、进了行缓冲、行真的组好了、PIT 还在走」，
# 而不是旧的 `KB <字符>` 那一行。
#
# 按键顺序是刻意选的：`a b Backspace c Enter` → 交付行必须是 **3 字节**（`ac` + `\n`）。
# 若退格没生效就是 `abc\n` = 4 字节 —— 于是 `len=3` 同时证明了「行缓冲收到了字符」
# 与「行编辑真的删掉了一个字符」，而不只是「有个标记出现了」。
test-keyboard: image
	$(PYTHON) tests/test_keyboard.py \
		--disk $(BUILD)/disk.img \
		--serial-log $(BUILD)/serial-kb.log \
		--key a \
		--key b \
		--key backspace \
		--key c \
		--key ret \
		--expect "KBIRQ enabled" \
		--expect "KBIRQ ENTRY" \
		--expect "kbd: line ready len=3" \
		--expect "KBIRQ EXIT" \
		--expect "IRQ0_heartbeat tick="

# v0.9 验收（交互式 Shell 第一步）：键盘行缓冲 → stdin(0) → 解析 → 内建命令。
#
# 硬断言（不是「日志里有 Shell 字样」）：
#   - `user: program=/bin/sh source=elf`：Shell 与 /init 一样是**从 VFS 装载的 ELF**；
#   - `user: read fd=0 want=128 got=5`：注入的 `help` + Enter 真的经键盘进了 stdin（5 = 4 + 行尾 \n）；
#   - `user: write fd=1 len=17` + `root@cerlesse C:>`：提示符按默认加载样式输出（17 字节），一次 write；
#   - `user: open path=/etc/motd ...` + `user: write fd=1 len=23`：`cat` 走真实文件 syscall；
#   - `syscall: ENOSYS nr=57 name=fork`：外部命令（`ls`）真的调了 fork，内核还没有进程模型；
#   - `sh: bye` + `user: shell PASS`：`exit` 正常结束。
test-shell: image
	$(PYTHON) tests/test_keyboard.py \
		--disk $(BUILD)/disk.img \
		--serial-log $(BUILD)/serial-shell.log \
		--ready-marker "user: program=/bin/sh" \
		--key h --key e --key l --key p --key ret \
		--key c --key a --key t --key spc --key slash \
		--key e --key t --key c --key slash --key m --key o --key t --key d --key ret \
		--key e --key x --key i --key t --key ret \
		--expect "user: program=/bin/sh source=elf" \
		--expect "user: read fd=0 want=128 got=5" \
		--expect "root@cerlesse C:>" \
		--expect "user: write fd=1 len=17" \
		--expect "sh: builtins (v0.9 step 3):" \
		--expect "sh:   /bin/hello      - external program" \
		--expect "user: open path=/etc/motd flags=0x0 fd=3" \
		--expect "motd: hello from ramfs" \
		--expect "sh: cat /etc/motd bytes=23" \
		--expect "sh: bye" \
		--expect "user: shell PASS"

# v0.9 第三步验收：**进程模型**（fork / execve / wait4）—— 从 Shell 交互触发，
# 断言的是「内核真的把另一个程序跑起来了」，而不是日志措辞：
#   - `user: fork parent=3 -> child pid=N image_pages=7`：地址空间被复制（7 个映像页 = Shell 的映像）；
#   - `user: run child pid=N name=fork-child`：子进程在父进程 wait4 时被恢复（单 CPU 的延迟调度）；
#   - `user: exec pid=N path=/bin/hello ... segments=4 image_pages=5`：映像被换掉，**PID 不变**；
#   - `hello-bin: argc=2` / `argv[0]=hello` / `argv[1]=world`：内核把 argv 写进了新进程的用户栈；
#   - `user: exec /bin/ls -> ENOENT` + `sh: ls: exit code=127`：
#     **找不到的程序**也是完整路径（fork 成功 → exec 失败 → 子进程 exit(127) → 父进程拿到退出码）；
#   - `user: frames free before=… after=…`：父进程 + 所有子进程的帧**全部归还**（没有地址空间泄漏）。
#
# 注意 PID 用**反向引用**断言而不是常数：syscall 手测里 `/bin/sh` 固定是 PID 3，但 fork
# 出的子进程号取决于此前消耗了多少个 PID（调度器自测会先取几个），写常数会让测试随
# 启动顺序变化而碎。`\1` 把 fork / execve / wait4 三处的 PID 绑定为同一个值。
test-proc: image
	$(PYTHON) tests/test_keyboard.py \
		--disk $(BUILD)/disk.img \
		--serial-log $(BUILD)/serial-proc.log \
		--ready-marker "user: program=/bin/sh" \
		--key h --key e --key l --key l --key o --key spc \
		--key w --key o --key r --key l --key d --key ret \
		--key l --key s --key ret \
		--key e --key x --key i --key t --key ret \
		--expect "user: program=/bin/sh source=elf" \
		--expect "hello-bin: Hello from an external program!" \
		--expect "sh: exec /bin/ls failed errno=-2" \
		--expect "sh: hello: exit code=1" \
		--expect "sh: ls: exit code=127" \
		--expect "sh: bye" \
		--expect "user: heap frame reclamation PASS" \
		--expect "user: selftest PASS" \
		--expect-re "user: fork parent=3 -> child pid=([0-9]+) image_pages=7.*?user: run child pid=\1 name=fork-child.*?sh: child pid=\1 exec /bin/hello \(argc=2\).*?user: exec pid=\1 path=/bin/hello segments=4 image_pages=5 entry=0x8000000000.*?hello-bin: pid=\1.*?user: reaped pid=\1 exit=1" \
		--expect-re "hello-bin: argc=2.*?hello-bin: argv\[0\]=hello.*?hello-bin: argv\[1\]=world" \
		--expect-re "user: run child pid=([0-9]+) name=fork-child.*?user: exec /bin/ls -> ENOENT \(no such program\).*?user: reaped pid=\1 exit=127" \
		--expect-re "user: frames free before=([0-9]+) after=\1"

# v0.7 用户态脚手架：单独构建 libc + user/hello（内核构建时会自动重建同一程序，
# 这个目标用于在不启动 QEMU 的情况下验证脚手架可编译、可链接、**段布局合法**）。
user:
	RUSTFLAGS="$(USER_RUSTFLAGS)" $(CARGO) build -p user-hello -p user-shell -p user-hello-bin \
		--target x86_64-unknown-none --profile $(USER_PROFILE) --target-dir $(BUILD)/user
	@$(PYTHON) tools/check_user_elf.py $(USER_ELF)
	@$(PYTHON) tools/check_user_elf.py $(USER_SHELL_ELF)
	@$(PYTHON) tools/check_user_elf.py $(USER_HELLO_BIN_ELF)

# H-06 P0a 验收：CKC 包容器（后缀 `.ckc`）。**主机侧、不启 QEMU、纯标准库**：
#
# 断言的是容器与打包规则本身（详见 tools/pkgmgr/ckc.py 的模块文档）：
#   - 确定性：同一目录树两次打包逐字节相同、rev 相同；
#   - 头部逐字段：magic/版本/头长/flags/payload_off/payload_len/保留位，改任一处 → 退出码 2；
#   - 内容哈希：翻 payload 一个字节 → 退出码 2；
#   - ustar 结构：条目未排序 / PKG.toml 不在第一 / 非零 mtime / 非零补零 / 符号链接 → 2；
#   - 打包期规则：超长路径（ENAMETOOLONG）/目标越出 scope/setuid/重复 target/hooks/… → 1；
#   - 独立复核：系统 `tar` 读同一段 payload，条目与内容必须与 ckc.py 一致（不是自己验自己）。
test-pkg:
	$(PYTHON) tests/test_ckc.py

# v0.4 H-04/H-05 主机侧：Go 单元测试（串口分析 + 并行编排器）
go-test:
	cd tools-go && $(GO) vet ./... && $(GO) test ./...

# v0.4 H-04 验收：Go 编排器并行跑全部 10 个集成测试场景 + 串口日志二次复核。
# 磁盘镜像先构建（默认/异常注入/页错误 + FAT 测试盘），随后场景并行执行；
# 每个场景独占 OVMF_VARS 副本、串口日志与 monitor socket（见 tests/orchestrate.json）。
test-orch: image image-exception image-pagefault fat-image
	cd tools-go && $(GO) run ./cmd/testorch -config ../tests/orchestrate.json -root ..

clean:
	rm -rf $(BUILD)
