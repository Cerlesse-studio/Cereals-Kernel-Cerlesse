//! 用户程序链接脚本接线（与 `user/hello/build.rs` 同一职责）。
//!
//! 只做一件事：把 `user.ld` 交给最终链接（`cargo:rustc-link-arg-bins`），
//! 让 Shell 固定链接在 `USER_VIRT_BASE`（0x0000_0080_0000_0000）上 ——
//! 内核的 ELF Loader 把映像按段映射后直接取指，不做重定位。
//!
//! **不要在 `user.ld` 里加段**：映像窗口只有 16 页（64 KiB），
//! 内核构建期（`kernel/build.rs::validate_image`）与运行期装载器都会拒绝越界映像。
fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    println!("cargo:rustc-link-arg-bins=-T{dir}/user.ld");
    println!("cargo:rerun-if-changed=user.ld");
}
