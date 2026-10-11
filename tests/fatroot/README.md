# tests/fatroot — FAT 测试盘的内容（v0.8）

- `data/` —— **被写进镜像的目录树**（`mkfs.py --from tests/fatroot/data`）
- 本 README —— 说明与复核方法（在 `data/` 之外，因此不会进镜像）

`make fat-image` 用 `tools/mkfs/mkfs.py` 把 `data/` 写进 `build/fat.img`；
QEMU 里作为第二块 virtio-blk 盘挂到 `/mnt/fat`，内核自测按文件名/大小/内容逐项断言
（`make test-fat`）。

| 路径 | 大小 | 用途 |
| ---- | ---- | ---- |
| `hello.txt` | 17 | 普通文件，内容 `hello from FAT32\n`（8.3 短名 + NT 小写标志） |
| `empty.txt` | 0 | **空文件**：首簇为 0，读它必须返回 0 字节而不是去解引用簇 0 |
| `a long file name.txt` | 13 | **长文件名（LFN）**：路径带空格，覆盖 0x0F 目录项拼装 |
| `dir/nested.bin` | 3000 | **跨簇**文件（512 字节簇 → 6 个簇），覆盖簇链遍历 |
| `dir/deep/marker.txt` | 5 | 二级子目录，覆盖多级路径解析 |

## 内容约定（内核按这个公式逐字节校验）

`dir/nested.bin` 不是随机数据，而是由公式生成 —— 内核可以自己算出期望值再比对，
不依赖宿主侧预先算好的校验和：

```python
bytes((i * 7 + 3) & 0xFF for i in range(3000))
```

其余文件都是可读 ASCII，断言直接比字符串。

## 重新生成

```bash
python3 - <<'EOF'
from pathlib import Path
root = Path("tests/fatroot/data")
(root / "hello.txt").write_bytes(b"hello from FAT32\n")
(root / "empty.txt").write_bytes(b"")
(root / "a long file name.txt").write_bytes(b"long name ok\n")
(root / "dir" / "nested.bin").write_bytes(bytes((i * 7 + 3) & 0xFF for i in range(3000)))
(root / "dir" / "deep" / "marker.txt").write_bytes(b"deep\n")
EOF
```

## 独立复核（不经过内核）

生成的镜像用 **mtools** 直接读 —— 这是与内核实现完全独立的另一套 FAT 实现；
两套实现都认同一张盘，才能说明「镜像本身是对的」：

```bash
mdir  -i build/fat.img ::               # 根目录（应看到 hello.txt / empty.txt / a long file name.txt / dir）
mdir  -i build/fat.img ::/dir           # 子目录
mtype -i build/fat.img ::hello.txt      # 文件内容
mtype -i build/fat.img ::/dir/nested.bin | wc -c    # 3000
```

`mkfs.py` 自己也会对写出的镜像做一次回读校验（几何 / FAT 副本 / 簇链 / 引导签名），
但它只是第一道关；**mtools 这一道才是独立证据**。
