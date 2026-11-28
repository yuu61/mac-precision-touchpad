//! Generate and link a KMDF driver's Win32 VERSIONINFO resource.
//!
//! For use from a driver crate's `build.rs` only:
//!
//! ```no_run
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     wdk_build::configure_wdk_binary_build()?;
//!     rc_embed::embed_driver_version("Example.sys", "Example driver")?;
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
//! - The generated script includes `winres.h` from the SDK's `um` and
//!   `shared` directories.
//! - `_DEBUG` is defined for the `dev` profile (Cargo `PROFILE=debug`), as Visual
//!   Studio does for a Debug configuration, so only debug builds set `VS_FF_DEBUG`.
//! - The `.res` is handed to `link.exe` with `cargo::rustc-cdylib-link-arg`;
//!   `link.exe` converts it (cvtres) for the target machine, so the same `.res`
//!   works for x64 and ARM64 and for `/DRIVER` images.

use core::error::Error;
use std::{
    env, fmt, fs,
    path::{Path, PathBuf},
    process::Command,
};

/// Error preparing or compiling the driver version resource.
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

/// Generates `$OUT_DIR/version.rc`, compiles it with the SDK, and links the
/// resulting resource into the calling driver's cdylib.
///
/// `AMTPTP_DRIVER_VERSION` is the package version supplied by xtask. A standalone
/// Cargo build uses the driver crate's major, minor and patch versions with a zero revision.
///
/// # Errors
/// Returns an error if the version is invalid, the generated script cannot be
/// written, the SDK cannot be located, or `rc.exe` fails.
pub fn embed_driver_version(filename: &str, description: &str) -> Result<(), Box<dyn Error>> {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-env-changed=AMTPTP_DRIVER_VERSION");
    let out_dir = env_path("OUT_DIR")?;
    let rc_file = out_dir.join("version.rc");
    let version = match env::var("AMTPTP_DRIVER_VERSION") {
        Ok(version) => version,
        Err(env::VarError::NotPresent) => format!(
            "{}.{}.{}.0",
            env::var("CARGO_PKG_VERSION_MAJOR")?,
            env::var("CARGO_PKG_VERSION_MINOR")?,
            env::var("CARGO_PKG_VERSION_PATCH")?
        ),
        Err(e) => return Err(e.into()),
    };
    let mut fields = version.split('.');
    let mut numbers = [0_u16; 4];
    let invalid = || {
        err(format!(
            "invalid VERSIONINFO version `{version}`: expected four u16 components"
        ))
    };
    for number in &mut numbers {
        *number = fields
            .next()
            .and_then(|field| field.parse().ok())
            .ok_or_else(&invalid)?;
    }
    if fields.next().is_some() {
        return Err(invalid().into());
    }
    let [a, b, c, d] = numbers;
    let script = format!(
        r#"#include "winres.h"

LANGUAGE LANG_ENGLISH, SUBLANG_ENGLISH_US

VS_VERSION_INFO VERSIONINFO
FILEVERSION {a}, {b}, {c}, {d}
PRODUCTVERSION {a}, {b}, {c}, {d}
FILEFLAGSMASK VS_FFI_FILEFLAGSMASK
#ifdef _DEBUG
FILEFLAGS VS_FF_DEBUG
#else
FILEFLAGS 0
#endif
FILEOS VOS_NT_WINDOWS32
FILETYPE VFT_DRV
FILESUBTYPE VFT2_DRV_SYSTEM
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904b0"
        BEGIN
            VALUE "CompanyName", "Bingxing Wang"
            VALUE "FileDescription", "{description}"
            VALUE "FileVersion", "{a}.{b}.{c}.{d}"
            VALUE "InternalName", "{filename}"
            VALUE "LegalCopyright", "Copyright (C) 2018-2019 Bingxing Wang and project contributors"
            VALUE "OriginalFilename", "{filename}"
            VALUE "ProductName", "Apple Precision Touchpad Driver Component"
            VALUE "ProductVersion", "{a}.{b}.{c}.{d}"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x409, 1200
    END
END
"#
    );
    fs::write(&rc_file, script)
        .map_err(|e| err(format!("cannot write {}: {e}", rc_file.display())))?;

    let (um, shared, rc_exe) = locate_sdk()?;
    let res = out_dir.join("version.res");

    let mut command = Command::new(&rc_exe);
    command.arg("/nologo");
    if env::var("PROFILE").is_ok_and(|p| p == "debug") {
        command.args(["/d", "_DEBUG"]);
    }
    command
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
    Ok(())
}
