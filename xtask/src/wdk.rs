//! Locating the WDK packaging tools (stampinf, Inf2Cat, InfVerif, signtool).
//!
//! Kits root: `WDK_ROOT` (xtask-only override), else `WDKContentRoot`, else
//! `MicrosoftKitRoot\Windows Kits\<WDKKitVersion, default 10.0>`, else the
//! registry value `HKLM\SOFTWARE\Microsoft\Windows Kits\Installed Roots\KitsRoot10`
//! (64-bit, then 32-bit view), else `C:\Program Files (x86)\Windows Kits\10`.
//! Version: `WDK_VERSION` (xtask-only override), else `Version_Number` (only
//! when `WDK_ROOT` is unset), else the highest `bin\10.*` that has stampinf.exe.
//!
//! After the `WDK_*` overrides this is the order wdk-build 0.6.0 uses
//! (`detect_wdk_content_root`, utils.rs:32-100; `detect_windows_sdk_version`,
//! utils.rs:334-336), so by default the packaging tools come from the kit the
//! driver binaries are compiled against, and an eWDK / NuGet shell (which sets
//! `WDKContentRoot` and `Version_Number` and has no KitsRoot10) works.
//! `WDK_ROOT` / `WDK_VERSION` steer only the packaging tools: wdk-build never
//! reads them. They are not exported to the driver builds either: wdk-build
//! emits no `rerun-if-env-changed`, so a change would leave stale wdk-sys
//! bindings in the driver target dirs.
//!
//! Tool locations differ per tool (WDK 10.0.28000): stampinf in
//! `bin\<ver>\{x64,x86,arm64}`, Inf2Cat only in `bin\<ver>\x86`, InfVerif in
//! `Tools\<ver>\{x64,arm64}` (older kits: `bin\<ver>\<arch>`), signtool in
//! `bin\<ver>\<arch>`.

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Environment lookup (`env::var_os`), a parameter so the precedence rules are
/// testable without mutating the process environment.
type EnvVar<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// `var(name)` as a non-empty string (non-Unicode counts as unset, as with
/// `env::var`).
fn non_empty(var: EnvVar<'_>, name: &str) -> Option<String> {
    var(name)
        .and_then(|v| v.into_string().ok())
        .filter(|v| !v.trim().is_empty())
}

const DEFAULT_KITS_ROOT: &str = r"C:\Program Files (x86)\Windows Kits\10";
const INSTALLED_ROOTS_KEY: &str = r"HKLM\SOFTWARE\Microsoft\Windows Kits\Installed Roots";

pub struct Wdk {
    pub root: PathBuf,
    pub version: String,
}

pub struct Tools {
    pub stampinf: PathBuf,
    pub inf2cat: PathBuf,
    pub infverif: PathBuf,
}

/// Host-architecture tool directories, most preferred first.
fn host_arch_dirs() -> &'static [&'static str] {
    if cfg!(target_arch = "aarch64") {
        &["arm64", "x64", "x86"]
    } else {
        &["x64", "x86"]
    }
}

impl Wdk {
    pub fn locate() -> Result<Wdk, String> {
        let var = |name: &str| env::var_os(name);
        let root = kits_root(&var)?;
        let version = match explicit_version(&var) {
            Some((name, v)) => {
                let w = Wdk {
                    root: root.clone(),
                    version: v.clone(),
                };
                if w.find_in_bin("stampinf.exe").is_none() {
                    return Err(format!(
                        "{name}={v}, but {} has no stampinf.exe (installed versions: {})",
                        root.join("bin").join(&v).display(),
                        list_versions(&root).join(", ")
                    ));
                }
                v
            }
            None => newest_version_with_stampinf(&root)?,
        };
        Ok(Wdk { root, version })
    }

    fn bin_dir(&self) -> PathBuf {
        self.root.join("bin").join(&self.version)
    }

    fn find_in(&self, base: &Path, file: &str) -> Option<PathBuf> {
        host_arch_dirs()
            .iter()
            .map(|a| base.join(a).join(file))
            .find(|p| p.is_file())
    }

    fn find_in_bin(&self, file: &str) -> Option<PathBuf> {
        self.find_in(&self.bin_dir(), file)
    }

    fn missing(&self, tool: &str, searched: &[PathBuf]) -> String {
        let dirs: Vec<String> = searched
            .iter()
            .map(|d| format!("{}\\{{{}}}", d.display(), host_arch_dirs().join(",")))
            .collect();
        format!(
            "{tool} not found in {} (WDK {} at {}). Install the Windows Driver Kit {} or point \
             WDK_ROOT / WDK_VERSION at a kit that has it.",
            dirs.join(" or "),
            self.version,
            self.root.display(),
            self.version
        )
    }

    pub fn packaging_tools(&self) -> Result<Tools, String> {
        let bin = self.bin_dir();
        let tools = self.root.join("Tools").join(&self.version);
        let stampinf = self
            .find_in_bin("stampinf.exe")
            .ok_or_else(|| self.missing("stampinf.exe", std::slice::from_ref(&bin)))?;
        // Inf2Cat ships only as an x86 binary.
        let inf2cat = [bin.join("x86").join("Inf2Cat.exe")]
            .into_iter()
            .chain(
                host_arch_dirs()
                    .iter()
                    .map(|a| bin.join(a).join("Inf2Cat.exe")),
            )
            .find(|p| p.is_file())
            .ok_or_else(|| self.missing("Inf2Cat.exe", std::slice::from_ref(&bin)))?;
        let infverif = self
            .find_in(&tools, "infverif.exe")
            .or_else(|| self.find_in_bin("infverif.exe"))
            .ok_or_else(|| self.missing("infverif.exe", &[tools.clone(), bin.clone()]))?;
        Ok(Tools {
            stampinf,
            inf2cat,
            infverif,
        })
    }

    pub fn signtool(&self) -> Result<PathBuf, String> {
        self.find_in_bin("signtool.exe")
            .ok_or_else(|| self.missing("signtool.exe", &[self.bin_dir()]))
    }
}

/// The explicitly requested kit version and the variable it came from:
/// `WDK_VERSION`, else `Version_Number` (set by an eWDK prompt; what wdk-build
/// uses). `Version_Number` belongs to the shell's kit, so it is ignored when
/// `WDK_ROOT` picked a different kit.
fn explicit_version(var: EnvVar<'_>) -> Option<(&'static str, String)> {
    if let Some(v) = non_empty(var, "WDK_VERSION") {
        return Some(("WDK_VERSION", v.trim().to_owned()));
    }
    if var("WDK_ROOT").is_some_and(|r| !r.is_empty()) {
        return None;
    }
    non_empty(var, "Version_Number").map(|v| ("Version_Number", v.trim().to_owned()))
}

/// `dir` if it is a Windows Kits root (has `bin\`), else an error naming `what`.
fn require_kits_root(dir: PathBuf, what: &str) -> Result<PathBuf, String> {
    if dir.join("bin").is_dir() {
        Ok(dir)
    } else {
        Err(format!(
            "{what}: {} is not a Windows Kits root (no bin\\ directory)",
            dir.display()
        ))
    }
}

/// The kits root an environment variable selects, or `None` when none does.
/// `WDK_ROOT` (xtask's override) must be usable. `WDKContentRoot` and
/// `MicrosoftKitRoot` follow wdk-build 0.6.0 `detect_wdk_content_root`: a value
/// that is not an existing directory is skipped (wdk-build warns and moves on);
/// an existing directory is used.
fn env_kits_root(var: EnvVar<'_>) -> Option<Result<PathBuf, String>> {
    if let Some(r) = var("WDK_ROOT").filter(|r| !r.is_empty()) {
        return Some(require_kits_root(PathBuf::from(r), "WDK_ROOT"));
    }
    if let Some(r) = var("WDKContentRoot").filter(|r| !r.is_empty()) {
        let p = PathBuf::from(r);
        if p.is_dir() {
            return Some(require_kits_root(p, "WDKContentRoot"));
        }
        println!(
            "xtask: warning: WDKContentRoot={} is not a directory; ignored",
            p.display()
        );
    }
    if let Some(r) = var("MicrosoftKitRoot").filter(|r| !r.is_empty()) {
        let k = PathBuf::from(r);
        if k.is_absolute() && k.is_dir() {
            let ver = non_empty(var, "WDKKitVersion").unwrap_or_else(|| "10.0".to_owned());
            let p = k.join("Windows Kits").join(ver);
            if p.is_dir() {
                return Some(require_kits_root(p, "MicrosoftKitRoot"));
            }
            println!(
                "xtask: warning: {} (from MicrosoftKitRoot) is not a directory; ignored",
                p.display()
            );
        } else {
            println!(
                "xtask: warning: MicrosoftKitRoot={} is not an absolute path to a directory; \
                 ignored",
                k.display()
            );
        }
    }
    None
}

fn kits_root(var: EnvVar<'_>) -> Result<PathBuf, String> {
    if let Some(r) = env_kits_root(var) {
        return r;
    }
    if let Some(p) = registry_kits_root().filter(|p| p.join("bin").is_dir()) {
        return Ok(p);
    }
    let p = PathBuf::from(DEFAULT_KITS_ROOT);
    if p.join("bin").is_dir() {
        Ok(p)
    } else {
        Err(format!(
            "Windows Driver Kit not found: WDK_ROOT, WDKContentRoot and MicrosoftKitRoot are \
             unset, the registry has no usable KitsRoot10 under {INSTALLED_ROOTS_KEY}, and \
             {DEFAULT_KITS_ROOT} does not exist. Install the WDK, run from an eWDK prompt, or \
             set WDK_ROOT."
        ))
    }
}

/// `reg query "<key>" /v KitsRoot10` (64-bit view, then 32-bit view).
fn registry_kits_root() -> Option<PathBuf> {
    for view in [None, Some("/reg:32")] {
        let mut cmd = Command::new("reg");
        cmd.args(["query", INSTALLED_ROOTS_KEY, "/v", "KitsRoot10"]);
        if let Some(v) = view {
            cmd.arg(v);
        }
        let Ok(out) = cmd.output() else { continue };
        if !out.status.success() {
            continue;
        }
        if let Some(p) = parse_reg_query(&String::from_utf8_lossy(&out.stdout)) {
            return Some(p);
        }
    }
    None
}

/// Extract the value from `    KitsRoot10    REG_SZ    C:\...\`.
fn parse_reg_query(out: &str) -> Option<PathBuf> {
    out.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("KitsRoot10")?;
        let value = rest.trim_start().strip_prefix("REG_SZ")?.trim();
        let value = value.trim_end_matches('\\');
        (!value.is_empty()).then(|| PathBuf::from(value))
    })
}

/// Parse `10.0.28000.0` into comparable numbers (non-`10.*` -> None).
fn parse_version(name: &str) -> Option<Vec<u32>> {
    if !name.starts_with("10.") {
        return None;
    }
    name.split('.').map(|p| p.parse().ok()).collect()
}

fn list_versions(root: &Path) -> Vec<String> {
    let mut v: Vec<(Vec<u32>, String)> = std::fs::read_dir(root.join("bin"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            parse_version(&name).map(|n| (n, name))
        })
        .collect();
    v.sort();
    v.reverse();
    v.into_iter().map(|(_, s)| s).collect()
}

fn newest_version_with_stampinf(root: &Path) -> Result<String, String> {
    let versions = list_versions(root);
    versions
        .iter()
        .find(|v| {
            let w = Wdk {
                root: root.to_path_buf(),
                version: (*v).clone(),
            };
            w.find_in_bin("stampinf.exe").is_some()
        })
        .cloned()
        .ok_or_else(|| {
            format!(
                "no WDK version under {} has stampinf.exe (versions found: {}). Install the \
                 Windows Driver Kit (not only the SDK), or set WDK_ROOT / WDK_VERSION.",
                root.join("bin").display(),
                if versions.is_empty() {
                    "none".to_owned()
                } else {
                    versions.join(", ")
                }
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_reg_output() {
        let out = "\r\nHKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\Windows Kits\\Installed Roots\r\n    \
                   KitsRoot10    REG_SZ    C:\\Program Files (x86)\\Windows Kits\\10\\\r\n\r\n";
        assert_eq!(
            parse_reg_query(out),
            Some(PathBuf::from(r"C:\Program Files (x86)\Windows Kits\10"))
        );
        assert_eq!(parse_reg_query("ERROR: not found"), None);
        assert_eq!(parse_reg_query("    KitsRoot10    REG_SZ    \r\n"), None);
    }

    fn fake_env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let vars: Vec<(String, OsString)> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), OsString::from(v)))
            .collect();
        move |name| vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
    }

    #[test]
    fn explicit_version_precedence() {
        let ver = |vars: &[(&str, &str)]| explicit_version(&fake_env(vars));
        assert_eq!(ver(&[]), None);
        assert_eq!(
            ver(&[
                ("WDK_VERSION", " 10.0.1.0 "),
                ("Version_Number", "10.0.2.0")
            ]),
            Some(("WDK_VERSION", "10.0.1.0".to_owned()))
        );
        // eWDK shell: Version_Number is what wdk-build compiles against.
        assert_eq!(
            ver(&[("Version_Number", "10.0.2.0")]),
            Some(("Version_Number", "10.0.2.0".to_owned()))
        );
        assert_eq!(
            ver(&[("WDK_VERSION", "  "), ("Version_Number", "10.0.2.0")]),
            Some(("Version_Number", "10.0.2.0".to_owned()))
        );
        // WDK_ROOT picked another kit: the shell's Version_Number does not apply.
        assert_eq!(
            ver(&[("WDK_ROOT", r"C:\k"), ("Version_Number", "10.0.2.0")]),
            None
        );
        assert_eq!(
            ver(&[("WDK_ROOT", r"C:\k"), ("WDK_VERSION", "10.0.1.0")]),
            Some(("WDK_VERSION", "10.0.1.0".to_owned()))
        );
        assert_eq!(
            ver(&[("WDK_ROOT", ""), ("Version_Number", "10.0.2.0")]),
            Some(("Version_Number", "10.0.2.0".to_owned()))
        );
    }

    #[test]
    fn env_kits_root_precedence() {
        let base = env::temp_dir().join(format!("xtask-wdk-test-{}", std::process::id()));
        let kit = |p: &Path| {
            std::fs::create_dir_all(p.join("bin")).unwrap();
            p.to_str().unwrap().to_owned()
        };
        let user = kit(&base.join("user"));
        let ewdk = kit(&base.join("ewdk"));
        let mkr = base.join("mkr");
        kit(&mkr.join("Windows Kits").join("10.0"));
        kit(&mkr.join("Windows Kits").join("11.0"));
        let mkr = mkr.to_str().unwrap().to_owned();
        let no_bin = base.join("nobin");
        std::fs::create_dir_all(&no_bin).unwrap();
        let no_bin = no_bin.to_str().unwrap().to_owned();
        let missing = base.join("missing").to_str().unwrap().to_owned();

        let root = |vars: &[(&str, &str)]| env_kits_root(&fake_env(vars));
        assert_eq!(root(&[]), None);
        assert_eq!(
            root(&[("WDK_ROOT", &user), ("WDKContentRoot", &ewdk)]),
            Some(Ok(PathBuf::from(&user)))
        );
        assert!(matches!(root(&[("WDK_ROOT", &no_bin)]), Some(Err(_))));
        assert_eq!(
            root(&[("WDKContentRoot", &ewdk), ("MicrosoftKitRoot", &mkr)]),
            Some(Ok(PathBuf::from(&ewdk)))
        );
        assert!(matches!(root(&[("WDKContentRoot", &no_bin)]), Some(Err(_))));
        // A WDKContentRoot that does not exist is skipped, as in wdk-build.
        assert_eq!(
            root(&[("WDKContentRoot", &missing), ("MicrosoftKitRoot", &mkr)]),
            Some(Ok(Path::new(&mkr).join("Windows Kits").join("10.0")))
        );
        assert_eq!(root(&[("WDKContentRoot", &missing)]), None);
        assert_eq!(
            root(&[("MicrosoftKitRoot", &mkr), ("WDKKitVersion", "11.0")]),
            Some(Ok(Path::new(&mkr).join("Windows Kits").join("11.0")))
        );
        assert_eq!(root(&[("MicrosoftKitRoot", "relative")]), None);
        assert_eq!(
            root(&[("MicrosoftKitRoot", &mkr), ("WDKKitVersion", "12.0")]),
            None
        );

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn version_order_is_numeric() {
        let mut v = [
            parse_version("10.0.9200.0").unwrap(),
            parse_version("10.0.28000.0").unwrap(),
            parse_version("10.0.26100.0").unwrap(),
        ];
        v.sort();
        assert_eq!(v.last().unwrap(), &vec![10, 0, 28000, 0]);
        assert_eq!(parse_version("x64"), None);
        assert_eq!(parse_version("10.0.foo"), None);
    }
}
