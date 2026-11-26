//! The two driver Cargo workspaces (`drivers/km`, `drivers/um`): building,
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

/// Variables that must not reach the driver builds: the target dir must stay
/// inside each workspace (wdk-build walks up from OUT_DIR to the first
/// Cargo.lock), and RUSTFLAGS-style variables replace `[build] rustflags`,
/// silently dropping the required `+crt-static`.
const DRIVER_ENV_REMOVE: [&str; 6] = [
    "CARGO_TARGET_DIR",
    "CARGO_BUILD_TARGET_DIR",
    "CARGO_BUILD_TARGET",
    "RUSTFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
    "CARGO_BUILD_RUSTFLAGS",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Workspace {
    /// `drivers/km`: KMDF (AmtPtpDeviceUsbKm, AmtPtpDeviceSpiKm, AmtPtpHidFilter).
    Km,
    /// `drivers/um`: UMDF (AmtPtpDeviceUsbUm).
    Um,
}

impl Workspace {
    pub const ALL: [Workspace; 2] = [Workspace::Km, Workspace::Um];

    pub fn dir_name(self) -> &'static str {
        match self {
            Workspace::Km => "km",
            Workspace::Um => "um",
        }
    }

    pub fn dir(self) -> PathBuf {
        repo_root().join("drivers").join(self.dir_name())
    }

    /// `drivers/<ws>/target/<triple>/<debug|release>`.
    pub fn output_dir(self, arch: Arch, release: bool) -> PathBuf {
        self.dir()
            .join("target")
            .join(arch.triple())
            .join(if release { "release" } else { "debug" })
    }

    /// The framework version for stampinf, read from the workspace's
    /// `[workspace.metadata.wdk.driver-model]` so the INF always matches what
    /// the binaries were built against: KMDF `major.minor` for `-k`,
    /// UMDF `major.minor.0` for `-u`.
    pub fn framework_version(self) -> Result<String, String> {
        let manifest = self.dir().join("Cargo.toml");
        let text = std::fs::read_to_string(&manifest)
            .map_err(|e| format!("cannot read {}: {e}", manifest.display()))?;
        framework_version_from_manifest(self, &text)
            .map_err(|e| format!("{}: {e}", manifest.display()))
    }
}

fn framework_version_from_manifest(ws: Workspace, text: &str) -> Result<String, String> {
    const SECTION: &str = "workspace.metadata.wdk.driver-model";
    let (kind, prefix) = match ws {
        Workspace::Km => ("KMDF", "kmdf"),
        Workspace::Um => ("UMDF", "umdf"),
    };
    let get = |key: &str| {
        toml_value(text, SECTION, key).ok_or_else(|| format!("[{SECTION}] has no `{key}`"))
    };
    let driver_type = get("driver-type")?;
    if driver_type.trim_matches('"') != kind {
        return Err(format!(
            "[{SECTION}] driver-type is {driver_type}, expected \"{kind}\""
        ));
    }
    let number = |key: &str| -> Result<u32, String> {
        let v = get(key)?;
        v.parse::<u32>()
            .map_err(|_| format!("[{SECTION}] {key} = {v} is not a number"))
    };
    let major = number(&format!("{prefix}-version-major"))?;
    let minor = number(&format!("target-{prefix}-version-minor"))?;
    Ok(match ws {
        Workspace::Km => format!("{major}.{minor}"),
        Workspace::Um => format!("{major}.{minor}.0"),
    })
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
fn driver_cargo(ws: Workspace) -> Command {
    let mut cmd = Command::new(cargo_program());
    cmd.current_dir(ws.dir());
    for var in DRIVER_ENV_REMOVE {
        if env::var_os(var).is_some() {
            println!(
                "xtask: note: not passing {var} to the {} driver build",
                ws.dir_name()
            );
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
pub fn build_drivers(
    arches: &[Arch],
    release: bool,
    workspaces: &[Workspace],
) -> Result<(), String> {
    for &arch in arches {
        for &ws in workspaces {
            let mut cmd = driver_cargo(ws);
            cmd.args(["build", "--target", arch.triple()]);
            if release {
                cmd.arg("--release");
            }
            process::run(
                &mut cmd,
                &format!("building drivers/{} for {}", ws.dir_name(), arch.name()),
            )?;
        }
    }
    Ok(())
}

/// `cargo xtask test`: host-workspace tests, then clippy (-D warnings) in all
/// three workspaces. Runs every step and reports all failures at the end.
pub fn test_all() -> Result<(), String> {
    let root = repo_root();
    let mut steps: Vec<(String, Command)> = Vec::new();

    let mut host_test = Command::new(cargo_program());
    host_test.current_dir(&root).args(["test", "--workspace"]);
    steps.push(("cargo test (host workspace)".into(), host_test));

    let mut host_clippy = Command::new(cargo_program());
    host_clippy.current_dir(&root).args([
        "clippy",
        "--workspace",
        "--all-targets",
        "--",
        "-D",
        "warnings",
    ]);
    steps.push(("cargo clippy (host workspace)".into(), host_clippy));

    for ws in Workspace::ALL {
        let mut clippy = driver_cargo(ws);
        clippy.args(["clippy", "--", "-D", "warnings"]);
        steps.push((format!("cargo clippy (drivers/{})", ws.dir_name()), clippy));
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
        assert_eq!(
            framework_version_from_manifest(Workspace::Km, KM).unwrap(),
            "1.23"
        );
        assert_eq!(
            framework_version_from_manifest(Workspace::Um, UM).unwrap(),
            "2.15.0"
        );
    }

    #[test]
    fn rejects_wrong_driver_type_or_missing_keys() {
        assert!(framework_version_from_manifest(Workspace::Um, KM).is_err());
        assert!(framework_version_from_manifest(Workspace::Km, UM).is_err());
        assert!(framework_version_from_manifest(Workspace::Km, "[workspace]\n").is_err());
    }

    #[test]
    fn real_manifests_match_the_inf_contract() {
        // The C INF hard-coded UmdfLibraryVersion=2.15.0 and the port pins KMDF 1.23.
        assert_eq!(Workspace::Km.framework_version().unwrap(), "1.23");
        assert_eq!(Workspace::Um.framework_version().unwrap(), "2.15.0");
    }
}
