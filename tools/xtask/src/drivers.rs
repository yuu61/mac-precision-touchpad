//! The KMDF driver workspace (`drivers/km`): building,
//! linting, and reading their WDF framework version.

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::cli::Arch;
use crate::paths::repo_root;
use crate::process;

/// libclang for wdk-sys's bindgen. MSYS2 may put an unrelated libclang on PATH.
const DEFAULT_LIBCLANG_PATH: &str = r"C:\Program Files\LLVM\bin";

// Cargo replaces its configured rustflags when any of these variables are set.
const RUSTFLAGS_ENV: [&str; 3] = [
    "RUSTFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
    "CARGO_BUILD_RUSTFLAGS",
];

/// Variables that must not reach the driver builds: the target dir must stay
/// inside each workspace (wdk-build walks up from `OUT_DIR` to the first
/// Cargo.lock), and RUSTFLAGS-style variables replace `[build] rustflags`,
/// silently dropping the required `+crt-static`.
const DRIVER_TARGET_ENV: [&str; 3] = [
    "CARGO_TARGET_DIR",
    "CARGO_BUILD_TARGET_DIR",
    "CARGO_BUILD_TARGET",
];

fn driver_dir() -> PathBuf {
    repo_root().join("drivers/km")
}

pub fn output_dir(arch: Arch, release: bool) -> PathBuf {
    driver_dir()
        .join("target")
        .join(arch.triple())
        .join(if release { "release" } else { "debug" })
}

/// The framework version for stampinf, read from the workspace's
/// `[workspace.metadata.wdk.driver-model]` so the INF always matches what
/// the binaries were built against: KMDF `major.minor` for `-k`.
pub fn framework_version() -> Result<String, String> {
    let manifest = driver_dir().join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest)
        .map_err(|e| format!("cannot read {}: {e}", manifest.display()))?;
    framework_version_from_manifest(&text).map_err(|e| format!("{}: {e}", manifest.display()))
}

fn framework_version_from_manifest(text: &str) -> Result<String, String> {
    const SECTION: &str = "workspace.metadata.wdk.driver-model";
    let get = |key: &str| {
        toml_value(text, SECTION, key).ok_or_else(|| format!("[{SECTION}] has no `{key}`"))
    };
    let driver_type = get("driver-type")?;
    if driver_type.trim_matches('"') != "KMDF" {
        return Err(format!(
            "[{SECTION}] driver-type is {driver_type}, expected \"KMDF\""
        ));
    }
    let number = |key: &str| -> Result<u32, String> {
        let v = get(key)?;
        v.parse::<u32>()
            .map_err(|error| format!("[{SECTION}] {key} = {v} is not a number: {error}"))
    };
    let major = number("kmdf-version-major")?;
    let minor = number("target-kmdf-version-minor")?;
    Ok(format!("{major}.{minor}"))
}

/// Minimal TOML lookup of `key = value` inside `[section]` (enough for the
/// flat driver-model table; comments after `#` are ignored).
fn toml_value(text: &str, section: &str, key: &str) -> Option<String> {
    let mut in_section = false;
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if let Some(name) = line.strip_prefix('[') {
            in_section = name.trim_end_matches(']').trim() == section;
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some((k, v)) = line.split_once('=')
            && k.trim() == key
        {
            return Some(v.trim().to_owned());
        }
    }
    None
}

/// The cargo that runs xtask (same toolchain), or `cargo` from PATH.
fn cargo_program() -> OsString {
    env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"))
}

/// A `cargo` command for a driver workspace with a sanitized environment.
fn driver_cargo() -> Command {
    let mut cmd = Command::new(cargo_program());
    cmd.current_dir(driver_dir());
    for var in DRIVER_TARGET_ENV.into_iter().chain(RUSTFLAGS_ENV) {
        if env::var_os(var).is_some() {
            println!("xtask: note: not passing {var} to the KMDF build");
        }
        cmd.env_remove(var);
    }
    if env::var_os("LIBCLANG_PATH").is_none() {
        if Path::new(DEFAULT_LIBCLANG_PATH)
            .join("libclang.dll")
            .is_file()
        {
            cmd.env("LIBCLANG_PATH", DEFAULT_LIBCLANG_PATH);
        } else {
            println!(
                "xtask: warning: LIBCLANG_PATH is unset and {DEFAULT_LIBCLANG_PATH} has no \
                 libclang.dll; wdk-sys's bindgen will use whatever libclang it finds"
            );
        }
    }
    cmd
}

/// `cargo build --target <triple> [--release]` in each workspace, per arch.
pub fn build_drivers(arches: &[Arch], release: bool) -> Result<(), String> {
    for &arch in arches {
        let mut cmd = driver_cargo();
        cmd.args(["build", "--target", arch.triple()]);
        if release {
            cmd.arg("--release");
        }
        process::run(
            &mut cmd,
            &format!("building drivers/km for {}", arch.name()),
        )?;
    }
    Ok(())
}

/// Runs the host tests and strict Clippy checks, collecting failures from every step.
pub fn test_all() -> Result<(), String> {
    let root = repo_root();
    let mut steps: Vec<(String, Command)> = Vec::new();

    let mut host_test = Command::new(cargo_program());
    host_test.current_dir(&root).args(["test", "--workspace"]);
    steps.push(("cargo test (host workspace)".into(), host_test));

    let mut host_clippy = Command::new(cargo_program());
    for var in RUSTFLAGS_ENV {
        host_clippy.env_remove(var);
    }
    host_clippy.current_dir(&root).args([
        "clippy",
        "--workspace",
        "--all-targets",
        "--all-features",
        "--",
        "-D",
        "warnings",
    ]);
    steps.push(("cargo clippy (host workspace)".into(), host_clippy));

    // Cargo Clippy only wraps workspace members. Use Clippy as the rustc wrapper
    // so the driver build also lints wdf-support and rc-embed in their build graph.
    let clippy_driver = Path::new(&cargo_program()).with_file_name("clippy-driver.exe");
    {
        let mut clippy = driver_cargo();
        clippy
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .env("RUSTC_WRAPPER", &clippy_driver)
            .args([
                "check",
                "--workspace",
                "--all-features",
                "--config",
                r#"build.rustflags=["-Dwarnings"]"#,
            ]);
        steps.push(("Clippy (drivers/km)".into(), clippy));
    }

    let mut failed = Vec::new();
    for (what, mut cmd) in steps {
        println!("xtask: === {what}");
        if let Err(e) = process::run(&mut cmd, &what) {
            eprintln!("xtask: {e}");
            failed.push(what);
        }
    }
    if failed.is_empty() {
        println!("xtask: all test and clippy steps passed");
        Ok(())
    } else {
        Err(format!("failed: {}", failed.join("; ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KM: &str = r#"
[workspace]
members = ["A"]

[workspace.metadata.wdk.driver-model]
driver-type = "KMDF"  # comment
kmdf-version-major = 1
target-kmdf-version-minor = 23

[workspace.dependencies]
kmdf-version-major = 9
"#;

    const UM: &str = r#"
[workspace.metadata.wdk.driver-model]
driver-type = "UMDF"
umdf-version-major = 2
target-umdf-version-minor = 15
"#;

    #[test]
    fn reads_framework_versions() {
        assert_eq!(framework_version_from_manifest(KM).unwrap(), "1.23");
    }

    #[test]
    fn rejects_wrong_driver_type_or_missing_keys() {
        framework_version_from_manifest(UM).unwrap_err();
        framework_version_from_manifest("[workspace]\n").unwrap_err();
    }

    #[test]
    fn real_manifests_match_the_inf_contract() {
        // The INF requires KMDF 1.23.
        assert_eq!(framework_version().unwrap(), "1.23");
    }
}
