//! Compile a Win32 resource script (`.rc`) with the Windows SDK `rc.exe` and link
//! the resulting `.res` into the cdylib that is being built.
//!
//! For use from a driver crate's `build.rs` only:
//!
//! ```no_run
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     wdk_build::configure_wdk_binary_build()?;
//!     rc_embed::compile_and_link("Resource.rc")?;
//!     Ok(())
//! }
//! ```
//!
//! How it works:
//! - The SDK is the one wdk-build selects (`WDKContentRoot` / registry, newest
//!   `Include/10.*` version), so resources and bindings come from the same kit.
//! - `rc.exe` is `<WDKContentRoot>/bin/<sdk>/<host-arch>/rc.exe` (the directory
//!   `wdk_build::cargo_make::setup_path` would put on `PATH`); if it is missing,
//!   `rc.exe` from `PATH` is tried.
//! - Include path: the `.rc` file's own directory (for `resource.h`), then the
//!   SDK `um` (`winres.h`) and `shared` directories. wdk-build's KMDF include
//!   list has no `um`, so both are added explicitly for every driver model.
//! - `_DEBUG` is defined for the `dev` profile (Cargo `PROFILE=debug`), as Visual
//!   Studio does for a Debug configuration, so `#ifdef _DEBUG` blocks (e.g.
//!   `FILEFLAGS 0x1L`) behave like the C build.
//! - The `.res` is handed to `link.exe` with `cargo::rustc-cdylib-link-arg`;
//!   `link.exe` converts it (cvtres) for the target machine, so the same `.res`
//!   works for x64 and ARM64 and for `/DRIVER` images.

use core::error::Error;
use std::{
    env, fmt, fs,
    path::{Path, PathBuf},
    process::Command,
};

/// Error returned by [`compile_and_link`].
#[derive(Debug)]
pub struct RcError(String);

impl fmt::Display for RcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for RcError {}

fn err(message: impl Into<String>) -> RcError {
    RcError(message.into())
}

fn env_path(name: &str) -> Result<PathBuf, RcError> {
    env::var_os(name).map(PathBuf::from).ok_or_else(|| {
        err(format!(
            "{name} is not set; rc-embed must run from a build script"
        ))
    })
}

/// Strips the `\\?\` prefix that `canonicalize` adds on Windows; `rc.exe` does
/// not accept verbatim paths in `/I`.
fn plain(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    text.strip_prefix(r"\\?\")
        .map_or_else(|| path.to_path_buf(), PathBuf::from)
}

/// Returns `(Include/<sdk>/um, Include/<sdk>/shared, bin/<sdk>/<host>/rc.exe)`.
fn locate_sdk() -> Result<(PathBuf, PathBuf, PathBuf), RcError> {
    // `Config::new()` detects the WDK content root exactly like the driver build
    // does; its include list always contains `Include/<sdk>/shared`.
    let config = wdk_build::Config::new();
    let shared = config
        .include_paths()
        .map_err(|e| err(format!("cannot determine WDK include paths: {e}")))?
        .map(|p| plain(&p))
        .find(|p| {
            p.file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case("shared"))
        })
        .ok_or_else(|| err("WDK include paths contain no `shared` directory"))?;
    let sdk_include = shared.parent().ok_or_else(|| {
        err(format!(
            "unexpected SDK include layout: {}",
            shared.display()
        ))
    })?;
    let um = sdk_include.join("um");
    if !um.join("winres.h").is_file() {
        return Err(err(format!("winres.h not found in {}", um.display())));
    }
    let sdk_version = sdk_include.file_name().ok_or_else(|| {
        err(format!(
            "unexpected SDK include layout: {}",
            shared.display()
        ))
    })?;
    let root = sdk_include.parent().and_then(Path::parent).ok_or_else(|| {
        err(format!(
            "unexpected SDK include layout: {}",
            shared.display()
        ))
    })?;
    let host = match env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "x86",
        other => return Err(err(format!("unsupported build host architecture {other}"))),
    };
    let rc = root.join("bin").join(sdk_version).join(host).join("rc.exe");
    let rc = if rc.is_file() {
        rc
    } else {
        PathBuf::from("rc.exe")
    };
    Ok((um, shared, rc))
}

/// Compiles `rc_file` (relative to `CARGO_MANIFEST_DIR`, or absolute) into
/// `$OUT_DIR/<stem>.res` and links it into the cdylib of the calling package.
///
/// Emits `cargo::rerun-if-changed` for the `.rc` file and every `*.h` next to it.
///
/// # Errors
/// Returns an error if the WDK/SDK cannot be located, `rc.exe` cannot be started,
/// or `rc.exe` reports a failure (its output is forwarded to Cargo's log).
pub fn compile_and_link<P: AsRef<Path>>(rc_file: P) -> Result<PathBuf, Box<dyn Error>> {
    let manifest_dir = env_path("CARGO_MANIFEST_DIR")?;
    let out_dir = env_path("OUT_DIR")?;
    let rc_file = manifest_dir.join(rc_file);
    let rc_dir = rc_file
        .parent()
        .ok_or_else(|| err(format!("{} has no parent directory", rc_file.display())))?
        .to_path_buf();
    let stem = rc_file
        .file_stem()
        .ok_or_else(|| err(format!("{} has no file name", rc_file.display())))?;

    println!("cargo::rerun-if-changed={}", rc_file.display());
    for entry in fs::read_dir(&rc_dir)? {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("h"))
        {
            println!("cargo::rerun-if-changed={}", path.display());
        }
    }

    let (um, shared, rc_exe) = locate_sdk()?;
    let res = out_dir.join(stem).with_extension("res");

    let mut command = Command::new(&rc_exe);
    command.arg("/nologo");
    if env::var("PROFILE").is_ok_and(|p| p == "debug") {
        command.args(["/d", "_DEBUG"]);
    }
    command
        .arg("/I")
        .arg(&rc_dir)
        .arg("/I")
        .arg(&um)
        .arg("/I")
        .arg(&shared)
        .arg("/fo")
        .arg(&res)
        .arg(&rc_file);

    let output = command
        .output()
        .map_err(|e| err(format!("cannot run {}: {e}", rc_exe.display())))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        return Err(err(format!(
            "{} failed ({}) for {}\n{stdout}{stderr}",
            rc_exe.display(),
            output.status,
            rc_file.display()
        ))
        .into());
    }

    println!("cargo::rustc-cdylib-link-arg={}", res.display());
    Ok(res)
}
