//! Command-line parsing (hand-written: std only).

use crate::package::InfKind;

pub const USAGE: &str = "\
Usage: cargo xtask <command> [options]

Commands:
  build      Build the driver workspaces drivers/km (KMDF) and drivers/um (UMDF).
  package    Build, then assemble, stamp, catalog, verify (and optionally sign)
             the driver package(s) under target/package/<arch>/<inf-name>/.
  test       Run host tests and strict Clippy checks across all three
             workspaces, including the shared driver support crates.
  help       Show this text.

Options (build, package):
  --arch x64|arm64|all        Target architecture(s). Default: the host architecture.
  --release                   Use the release profile (default: dev/debug).

Options (package only):
  --inf universal|hidfilter|all
                              Which INF to package. universal = packaging/AmtPtpDevice.inx
                              (all four drivers), hidfilter = packaging/AmtPtpHidFilter.inx
                              (AmtPtpHidFilter only). Default: all.
  --driver-version a.b.c.d    DriverVer version (each part 0-65535). Default: stampinf's
                              '*' (current time h.m.s.ms).
  --sign <args...>            Sign with signtool. EVERYTHING after --sign is passed to
                              `signtool sign` verbatim (so --sign must come last), e.g.
                              --sign /sha1 <thumbprint> /tr http://timestamp.digicert.com /td sha256
                              (certificate in the certificate store). /fd sha256 is added when
                              the arguments contain no /fd. A /p <password> value is hidden in
                              xtask's output, but it is still on the process command lines and
                              in shell history; prefer /sha1.
                              Default: unsigned. xtask never creates certificates.

Environment:
  WDKContentRoot, MicrosoftKitRoot (+ WDKKitVersion), Version_Number
                              The kit wdk-build compiles the drivers against (set by an eWDK
                              prompt). xtask takes the packaging tools from the same kit:
                              root = WDKContentRoot, else MicrosoftKitRoot\\Windows Kits\\
                              <WDKKitVersion|10.0>, else registry KitsRoot10, else
                              C:\\Program Files (x86)\\Windows Kits\\10; version =
                              Version_Number, else the highest 10.* in bin\\ with stampinf.exe.
  WDK_ROOT, WDK_VERSION       xtask-only overrides of that root and version (e.g.
                              C:\\Program Files (x86)\\Windows Kits\\10, 10.0.28000.0). They pick
                              stampinf, Inf2Cat, InfVerif and signtool only, NOT the kit the
                              driver binaries are compiled against. Version_Number is ignored
                              when WDK_ROOT is set.
  LIBCLANG_PATH               Passed to the driver builds; set to C:\\Program Files\\LLVM\\bin
                              when unset and that directory exists.
";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arch {
    X64,
    Arm64,
}

impl Arch {
    pub const ALL: [Self; 2] = [Self::X64, Self::Arm64];

    /// Name used on the command line and in `target/package/<arch>`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::X64 => "x64",
            Self::Arm64 => "arm64",
        }
    }

    pub const fn triple(self) -> &'static str {
        match self {
            Self::X64 => "x86_64-pc-windows-msvc",
            Self::Arm64 => "aarch64-pc-windows-msvc",
        }
    }

    /// Value for `stampinf -a` (replaces `$ARCH$`, e.g. `NTamd64.10.0...16299`).
    pub const fn stampinf_arch(self) -> &'static str {
        match self {
            Self::X64 => "amd64",
            Self::Arm64 => "arm64",
        }
    }

    /// `Inf2Cat /os:` list: every Windows 10/11 release from 1709 (RS3, the
    /// KMDF 1.23 / INF `NT$ARCH$.10.0...16299` floor) on, as accepted by the
    /// WDK 10.0.28000 `Inf2Cat` (`Inf2Cat /?`). x64 also lists the server ids
    /// at or above that floor (`ServerRS5` = 2019, `ServerFE` = 2022, 2025);
    /// `Server2016_X64` (1607) is below the floor and `Server10_X64` is the
    /// generic pre-1709 id, so both are omitted. ARM64 is client only.
    pub const fn inf2cat_os(self) -> &'static str {
        match self {
            Self::X64 => {
                "10_RS3_X64,10_RS4_X64,10_RS5_X64,ServerRS5_X64,10_19H1_X64,10_VB_X64,\
                 ServerFE_X64,10_CO_X64,10_NI_X64,10_GE_X64,Server2025_X64"
            }
            Self::Arm64 => {
                "10_RS3_ARM64,10_RS4_ARM64,10_RS5_ARM64,10_19H1_ARM64,10_VB_ARM64,\
                 10_CO_ARM64,10_NI_ARM64,10_GE_ARM64"
            }
        }
    }

    /// The architecture xtask itself runs on (default for `--arch`).
    pub const fn host() -> Self {
        if cfg!(target_arch = "aarch64") {
            Self::Arm64
        } else {
            Self::X64
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct BuildOpts {
    pub arches: Vec<Arch>,
    pub release: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct PackageOpts {
    pub arches: Vec<Arch>,
    pub release: bool,
    pub infs: Vec<InfKind>,
    /// `None` = stampinf `-v *`.
    pub driver_version: Option<String>,
    /// `Some(args)` = sign with `signtool sign <args>`.
    pub sign: Option<Vec<String>>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Build(BuildOpts),
    Package(PackageOpts),
    Test,
}

pub fn parse(args: &[String]) -> Result<Command, String> {
    let Some((cmd, rest)) = args.split_first() else {
        return Ok(Command::Help);
    };
    match cmd.as_str() {
        "help" | "-h" | "--help" => Ok(Command::Help),
        "build" => {
            let o = parse_options(rest, false)?;
            Ok(Command::Build(BuildOpts {
                arches: o.arches,
                release: o.release,
            }))
        }
        "package" => {
            let o = parse_options(rest, true)?;
            Ok(Command::Package(PackageOpts {
                arches: o.arches,
                release: o.release,
                infs: o.infs,
                driver_version: o.driver_version,
                sign: o.sign,
            }))
        }
        "test" => {
            if let Some(a) = rest.first() {
                return Err(format!("`test` takes no options (got `{a}`)"));
            }
            Ok(Command::Test)
        }
        other => Err(format!(
            "unknown command `{other}` (expected build, package, test or help; run `cargo xtask help`)"
        )),
    }
}

struct Options {
    arches: Vec<Arch>,
    release: bool,
    infs: Vec<InfKind>,
    driver_version: Option<String>,
    sign: Option<Vec<String>>,
}

fn parse_options(args: &[String], package: bool) -> Result<Options, String> {
    let mut o = Options {
        arches: vec![Arch::host()],
        release: false,
        infs: InfKind::ALL.to_vec(),
        driver_version: None,
        sign: None,
    };
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        i += 1;
        // Accept both `--opt value` and `--opt=value`.
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v.to_owned())),
            _ => (arg.as_str(), None),
        };
        let mut value = |what: &str| -> Result<String, String> {
            if let Some(v) = inline.clone() {
                return Ok(v);
            }
            let v = args
                .get(i)
                .ok_or_else(|| format!("`{name}` needs a value ({what})"))?;
            i += 1;
            Ok(v.clone())
        };
        match name {
            "--release" if inline.is_none() => o.release = true,
            "--arch" => {
                o.arches = match value("x64, arm64 or all")?.to_ascii_lowercase().as_str() {
                    "x64" | "amd64" | "x86_64" => vec![Arch::X64],
                    "arm64" | "aarch64" => vec![Arch::Arm64],
                    "all" => Arch::ALL.to_vec(),
                    v => return Err(format!("unknown --arch `{v}` (expected x64, arm64 or all)")),
                }
            }
            "--inf" if package => {
                o.infs = match value("universal, hidfilter or all")?
                    .to_ascii_lowercase()
                    .as_str()
                {
                    "universal" | "amtptpdevice" => vec![InfKind::Universal],
                    "hidfilter" | "amtptphidfilter" => vec![InfKind::HidFilter],
                    "all" => InfKind::ALL.to_vec(),
                    v => {
                        return Err(format!(
                            "unknown --inf `{v}` (expected universal, hidfilter or all)"
                        ));
                    }
                }
            }
            "--driver-version" if package => {
                let v = value("a.b.c.d")?;
                validate_driver_version(&v)?;
                o.driver_version = Some(v);
            }
            "--sign" if package => {
                if inline.is_some() {
                    return Err(
                        "use `--sign <signtool args...>` (everything after --sign goes to signtool)"
                            .into(),
                    );
                }
                let rest: Vec<String> = args.get(i..).unwrap_or_default().to_vec();
                if rest.is_empty() {
                    return Err(
                        "`--sign` needs signtool arguments after it, e.g. `--sign /sha1 <thumbprint>` \
                         or `--sign /f cert.pfx`"
                            .into(),
                    );
                }
                o.sign = Some(rest);
                break;
            }
            _ => {
                return Err(format!(
                    "unknown option `{arg}` for `{}` (run `cargo xtask help`)",
                    if package { "package" } else { "build" }
                ));
            }
        }
    }
    Ok(o)
}

/// `DriverVer` version: four dot-separated 16-bit numbers.
pub fn validate_driver_version(v: &str) -> Result<(), String> {
    let parts: Vec<&str> = v.split('.').collect();
    let ok = parts.len() == 4
        && parts.iter().all(|p| {
            !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) && p.parse::<u16>().is_ok()
        });
    if ok {
        Ok(())
    } else {
        Err(format!(
            "invalid --driver-version `{v}`: expected a.b.c.d with each part a number 0-65535"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn no_args_is_help() {
        assert_eq!(parse(&[]).unwrap(), Command::Help);
    }

    #[test]
    fn build_defaults_to_host_debug() {
        assert_eq!(
            parse(&s(&["build"])).unwrap(),
            Command::Build(BuildOpts {
                arches: vec![Arch::host()],
                release: false
            })
        );
    }

    #[test]
    fn package_full() {
        let c = parse(&s(&[
            "package",
            "--arch=all",
            "--release",
            "--inf",
            "hidfilter",
            "--driver-version",
            "1.2.3.65535",
            "--sign",
            "/f",
            "a.pfx",
            "--release",
        ]))
        .unwrap();
        assert_eq!(
            c,
            Command::Package(PackageOpts {
                arches: vec![Arch::X64, Arch::Arm64],
                release: true,
                infs: vec![InfKind::HidFilter],
                driver_version: Some("1.2.3.65535".into()),
                sign: Some(s(&["/f", "a.pfx", "--release"])),
            })
        );
    }

    #[test]
    fn rejects_bad_input() {
        parse(&s(&["build", "--inf", "universal"])).unwrap_err();
        parse(&s(&["build", "--arch", "x86"])).unwrap_err();
        parse(&s(&["build", "--arch"])).unwrap_err();
        parse(&s(&["package", "--sign"])).unwrap_err();
        parse(&s(&["package", "--bogus"])).unwrap_err();
        parse(&s(&["test", "--release"])).unwrap_err();
        parse(&s(&["frobnicate"])).unwrap_err();
    }

    #[test]
    fn driver_version_validation() {
        validate_driver_version("0.0.0.0").unwrap();
        validate_driver_version("19.0.0.200").unwrap();
        validate_driver_version("65535.65535.65535.65535").unwrap();
        assert!(validate_driver_version("65536.0.0.0").is_err());
        assert!(validate_driver_version("1.2.3").is_err());
        assert!(validate_driver_version("1.2.3.4.5").is_err());
        assert!(validate_driver_version("1.2..4").is_err());
        assert!(validate_driver_version("1.2.3.+4").is_err());
        assert!(validate_driver_version("*").is_err());
    }

    #[test]
    fn inf2cat_lists_are_per_arch() {
        assert!(
            Arch::X64
                .inf2cat_os()
                .split(',')
                .all(|o| o.ends_with("_X64"))
        );
        assert!(
            Arch::Arm64
                .inf2cat_os()
                .split(',')
                .all(|o| o.ends_with("_ARM64"))
        );
        assert!(Arch::X64.inf2cat_os().starts_with("10_RS3_X64,"));
        assert!(Arch::Arm64.inf2cat_os().starts_with("10_RS3_ARM64,"));
    }
}
