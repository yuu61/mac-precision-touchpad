//! Repository paths.
//!
//! Derived from this crate's manifest directory at compile time, never from
//! `fs::canonicalize` (its `\\?\` verbatim prefix breaks the WDK tools).

use std::path::{Path, PathBuf};

/// The repository root (parent of `xtask/`).
pub fn repo_root() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().unwrap_or(manifest_dir).to_path_buf()
}

/// The host workspace's target directory (`<root>/target`).
pub fn root_target_dir() -> PathBuf {
    repo_root().join("target")
}

/// `<root>/target/package/<arch>`.
pub fn package_arch_dir(arch: crate::cli::Arch) -> PathBuf {
    root_target_dir().join("package").join(arch.name())
}
