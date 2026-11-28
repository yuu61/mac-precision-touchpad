//! `cargo xtask package`: assemble, stamp, verify, catalog and (optionally)
//! sign the driver package.
//!
//! There is one package, built from `packaging/AmtPtpDevice.inx`, which
//! installs all four drivers. Layout (under the
//! host workspace's target dir):
//! ```text
//! target/package/<arch>/AmtPtpDevice/            AmtPtpDevice.inf + .cat, AmtPtpDeviceUsbKm.sys,
//!                                                AmtPtpDeviceSpiKm.sys,
//!                                                AmtPtpHidFilter.sys, AmtPtpBthFilter.sys
//! target/package/<arch>/AmtPtpDevice.symbols/    the matching .pdb files of that package
//! ```
//!
//! The `.symbols` directory sits outside the package directory so `Inf2Cat`
//! does not see it. It is emptied together with the package, so its PDBs
//! always match the package's binaries.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::cli::{Arch, PackageOpts, parse_driver_version};
use crate::drivers::{self, DriverBuild};
use crate::paths::{package_arch_dir, repo_root, root_target_dir};
use crate::process;
use crate::wdk::{Tools, Wdk};

/// One driver binary: cargo builds `<name>.dll` (+ `<name>.pdb`); the INF's
/// `[SourceDisksFiles]` expects KMDF images as `<name>.sys`.
#[derive(Debug)]
struct Binary {
    name: &'static str,
}

impl Binary {
    fn packaged_name(&self) -> String {
        format!("{}.sys", self.name)
    }
}

const USB_KM: Binary = Binary {
    name: "AmtPtpDeviceUsbKm",
};
const SPI_KM: Binary = Binary {
    name: "AmtPtpDeviceSpiKm",
};
const HID_FILTER: Binary = Binary {
    name: "AmtPtpHidFilter",
};
const BTH_FILTER: Binary = Binary {
    name: "AmtPtpBthFilter",
};

/// INF/CAT base name and package directory name of the driver package.
const PACKAGE: &str = "AmtPtpDevice";

/// Package directories of the standalone `AmtPtpHidFilter.inx` that older xtask versions built. That
/// INF claimed the USB Magic Trackpad 2 against the universal one and only matched one hardware
/// revision; the universal package installs the filter for the Bluetooth trackpad.
const OBSOLETE_PACKAGES: [&str; 2] = ["AmtPtpHidFilter", "AmtPtpHidFilter.symbols"];

/// The driver binaries of the package. Must match the INF's `[SourceDisksFiles]`.
const BINARIES: [Binary; 4] = [USB_KM, SPI_KM, HID_FILTER, BTH_FILTER];

/// `packaging/AmtPtpDevice.inx`.
fn inx() -> PathBuf {
    repo_root().join("packaging").join(format!("{PACKAGE}.inx"))
}

struct Context<'a> {
    opts: &'a PackageOpts,
    tools: Tools,
    signtool: Option<PathBuf>,
    kmdf: String,
    driver_ver: &'a DriverVer,
    build: DriverBuild<'a>,
}

#[derive(Debug, PartialEq, Eq)]
struct DriverVer {
    date: String,
    version: String,
}

impl DriverVer {
    fn from_inf(text: &str) -> Result<Self, String> {
        let value = text
            .lines()
            .map(strip_inf_comment)
            .filter_map(|line| line.split_once('='))
            .find_map(|(key, value)| {
                key.trim()
                    .eq_ignore_ascii_case("DriverVer")
                    .then_some(value)
            })
            .ok_or("no DriverVer in stamped INF")?;
        let (date, version) = value
            .split_once(',')
            .filter(|(date, _)| !date.trim().is_empty())
            .ok_or_else(|| format!("DriverVer = `{value}` was not stamped (date,version)"))?;
        let [a, b, c, d] = parse_driver_version(version.trim())?;
        Ok(Self {
            date: date.trim().to_owned(),
            version: format!("{a}.{b}.{c}.{d}"),
        })
    }
}

/// Stamp a minimal INF once so all architectures use the same local date and
/// version, even if the driver builds run across midnight.
fn resolve_driver_ver(
    stampinf: &Path,
    scratch: &Path,
    version: Option<&str>,
) -> Result<DriverVer, String> {
    write(
        scratch,
        "[Version]\r\nSignature = \"$WINDOWS NT$\"\r\nDriverVer =\r\n",
    )?;
    let mut cmd = Command::new(stampinf);
    cmd.arg("-f")
        .arg(scratch)
        .args(["-d", "*", "-v", version.unwrap_or("*")])
        .env_remove("PRIVATE_DRIVER_PACKAGE");
    let result = process::run(&mut cmd, "resolve DriverVer").and_then(|()| {
        let stamped = fs::read_to_string(scratch)
            .map_err(|e| format!("cannot read {}: {e}", scratch.display()))?;
        DriverVer::from_inf(&stamped)
    });
    let removed = fs::remove_file(scratch);
    let driver_ver = result?;
    removed.map_err(|e| format!("cannot remove {}: {e}", scratch.display()))?;
    Ok(driver_ver)
}

pub fn package(opts: &PackageOpts) -> Result<(), String> {
    // Resolve everything that can fail cheaply before the (slow) builds.
    let wdk = Wdk::locate()?;
    let tools = wdk.packaging_tools()?;
    let signtool = match &opts.sign {
        Some(_) => Some(wdk.signtool()?),
        None => None,
    };
    process::require_file(&inx(), " (the INF source of the package)")?;
    let kmdf = drivers::framework_version()?;
    println!(
        "xtask: WDK {} at {}; KMDF {kmdf}",
        wdk.version,
        wdk.root.display()
    );
    println!("xtask: stampinf: {}", tools.stampinf.display());
    println!("xtask: Inf2Cat:  {}", tools.inf2cat.display());
    println!("xtask: InfVerif: {}", tools.infverif.display());
    if let Some(s) = &signtool {
        println!("xtask: signtool: {}", s.display());
    }

    let package_root = root_target_dir().join("package");
    fs::create_dir_all(&package_root)
        .map_err(|e| format!("cannot create {}: {e}", package_root.display()))?;
    // Both the packaging build cache and the final package directories are
    // shared by invocations. Hold the lock until all copies and catalogs finish.
    let lock = package_root.join(".lock");
    let package_lock = fs::File::options()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock)
        .map_err(|e| format!("cannot open {}: {e}", lock.display()))?;
    package_lock
        .try_lock()
        .map_err(|e| format!("cannot lock {} for packaging: {e}", lock.display()))?;
    let driver_ver = resolve_driver_ver(
        &tools.stampinf,
        &package_root.join(".driver-version.inf"),
        opts.driver_version.as_deref(),
    )?;
    let build = DriverBuild {
        release: opts.release,
        package_version: Some(&driver_ver.version),
    };
    drivers::build_drivers(&opts.arches, build)?;

    let cx = Context {
        opts,
        tools,
        signtool,
        kmdf,
        driver_ver: &driver_ver,
        build,
    };
    for &arch in &opts.arches {
        package_one(&cx, arch).map_err(|e| format!("[{} {PACKAGE}] {e}", arch.name()))?;
    }

    println!();
    println!(
        "xtask: packages ({} profile, {}):",
        if opts.release { "release" } else { "dev" },
        if opts.sign.is_some() {
            "signed"
        } else {
            "UNSIGNED"
        }
    );
    for &arch in &opts.arches {
        print_tree(&package_arch_dir(arch))?;
    }
    Ok(())
}

fn package_one(cx: &Context<'_>, arch: Arch) -> Result<(), String> {
    let opts = cx.opts;
    let arch_dir = package_arch_dir(arch);
    let pkg = arch_dir.join(PACKAGE);
    let symbols = arch_dir.join(format!("{PACKAGE}.symbols"));
    let inf_file = format!("{PACKAGE}.inf");
    let cat_file = format!("{PACKAGE}.cat");
    println!(
        "xtask: === [{}] {PACKAGE} -> {}",
        arch.name(),
        pkg.display()
    );

    // The standalone HidFilter package of older xtask versions must not be
    // mistaken for a current one.
    for obsolete in OBSOLETE_PACKAGES {
        let dir = arch_dir.join(obsolete);
        if dir.is_dir() {
            println!(
                "xtask: removing obsolete {} (the HidFilter is part of {PACKAGE})",
                dir.display()
            );
            remove_dir(&dir)?;
        }
    }

    // The shared `symbols/` directory of older xtask versions holds PDBs that
    // may match neither package; remove it so it cannot be mistaken for them.
    let legacy_symbols = arch_dir.join("symbols");
    if legacy_symbols.is_dir() {
        println!(
            "xtask: removing obsolete {} (PDBs now go to <inf-name>.symbols/)",
            legacy_symbols.display()
        );
        remove_dir(&legacy_symbols)?;
    }

    // Start from empty directories: Inf2Cat catalogs what it finds, a stale
    // .cat/.inf must never survive a failed run, and the symbols must match
    // the binaries of THIS package (a missing PDB must not leave an old one).
    for d in [&pkg, &symbols] {
        if d.exists() {
            remove_dir(d)?;
        }
        fs::create_dir_all(d).map_err(|e| format!("cannot create {}: {e}", d.display()))?;
    }

    // 1. Binaries (KMDF .dll -> .sys) and their PDBs.
    let mut payload = Vec::new();
    for b in &BINARIES {
        let out = cx.build.output_dir(arch);
        let dll = out.join(format!("{}.dll", b.name));
        process::require_file(&dll, " (expected from the drivers build)")?;
        let dest = pkg.join(b.packaged_name());
        copy(&dll, &dest)?;
        payload.push(dest);
        let pdb = out.join(format!("{}.pdb", b.name));
        if pdb.is_file() {
            copy(&pdb, &symbols.join(format!("{}.pdb", b.name)))?;
        } else {
            println!("xtask: warning: no PDB at {}", pdb.display());
        }
    }

    // 2. INX -> INF (CRLF, the canonical INF line ending; the .inx is LF in git).
    let inx = inx();
    let text =
        fs::read_to_string(&inx).map_err(|e| format!("cannot read {}: {e}", inx.display()))?;
    write(&pkg.join(&inf_file), &to_crlf(&text))?;

    // 3. stampinf: $ARCH$, $KMDFVERSION$, DriverVer, CatalogFile.
    let mut cmd = Command::new(&cx.tools.stampinf);
    cmd.current_dir(&pkg)
        .env_remove("PRIVATE_DRIVER_PACKAGE")
        .args([
            "-f",
            &inf_file,
            "-d",
            &cx.driver_ver.date,
            "-a",
            arch.stampinf_arch(),
            "-c",
            &cat_file,
            "-v",
            &cx.driver_ver.version,
            "-k",
            &cx.kmdf,
        ]);
    process::run(&mut cmd, "stampinf")?;
    let stamped = fs::read_to_string(pkg.join(&inf_file))
        .map_err(|e| format!("cannot read stamped {inf_file}: {e}"))?;
    check_stamped(&stamped, &cx.kmdf, &cat_file, cx.driver_ver)
        .map_err(|e| format!("stampinf output check failed for {inf_file}: {e}"))?;

    // 4. Verify the entire stamped INF against Windows Driver requirements.
    let mut cmd = Command::new(&cx.tools.infverif);
    cmd.current_dir(&pkg).args(["/w", inf_file.as_str()]);
    process::run(&mut cmd, "InfVerif /w")?;

    // 5. Sign the binaries before cataloging them.
    if let (Some(signtool), Some(args)) = (&cx.signtool, &opts.sign) {
        sign(signtool, args, &payload)?;
    }

    // 6. Inf2Cat. The DriverVer date is local; /uselocaltime keeps
    //    Inf2Cat from rejecting it as future-dated east of UTC.
    let mut cmd = Command::new(&cx.tools.inf2cat);
    cmd.arg(format!("/driver:{}", pkg.display()))
        .arg(format!("/os:{}", arch.inf2cat_os()))
        .arg("/uselocaltime");
    process::run(&mut cmd, "Inf2Cat")?;
    let cat = catalog_with_inf_case(&pkg, &cat_file)?;

    // 7. Sign the catalog.
    if let (Some(signtool), Some(args)) = (&cx.signtool, &opts.sign) {
        sign(signtool, args, core::slice::from_ref(&cat))?;
    }
    Ok(())
}

/// `Inf2Cat` writes the catalog name in lower case (`amtptpdevice.cat`). The
/// file system is case-insensitive, so that already works; rename it to the
/// INF's `CatalogFile` spelling anyway so the package reads consistently.
fn catalog_with_inf_case(pkg: &Path, cat_file: &str) -> Result<PathBuf, String> {
    let wanted = pkg.join(cat_file);
    let found = fs::read_dir(pkg)
        .map_err(|e| format!("cannot list {}: {e}", pkg.display()))?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| n.eq_ignore_ascii_case(cat_file))
        .ok_or_else(|| format!("Inf2Cat reported success but wrote no {cat_file}"))?;
    if found != cat_file {
        // Two steps: a case-only rename is not reliable on every file system.
        let tmp = pkg.join(format!("{cat_file}.rename"));
        fs::rename(pkg.join(&found), &tmp)
            .and_then(|()| fs::rename(&tmp, &wanted))
            .map_err(|e| format!("cannot rename {found} to {cat_file}: {e}"))?;
    }
    Ok(wanted)
}

/// `signtool sign <user args> [/fd sha256] <files>`. The PFX password after
/// `/p` is redacted in the log and in errors (the process command line still
/// carries it; packaging.md recommends `/sha1 <thumbprint>`).
fn sign(signtool: &Path, args: &[String], files: &[PathBuf]) -> Result<(), String> {
    let mut cmd = Command::new(signtool);
    cmd.arg("sign").args(args);
    if !args
        .iter()
        .any(|a| a.eq_ignore_ascii_case("/fd") || a.eq_ignore_ascii_case("-fd"))
    {
        cmd.args(["/fd", "sha256"]);
    }
    cmd.args(files);
    process::run_redacted(&mut cmd, "signtool sign", &["/p", "-p"])
}

fn remove_dir(d: &Path) -> Result<(), String> {
    fs::remove_dir_all(d)
        .map_err(|e| format!("cannot remove {} (is a file in it open?): {e}", d.display()))
}

fn copy(from: &Path, to: &Path) -> Result<(), String> {
    fs::copy(from, to)
        .map(|_| ())
        .map_err(|e| format!("cannot copy {} to {}: {e}", from.display(), to.display()))
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

fn to_crlf(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\n', "\r\n")
}

/// The part of an INF line before a `;` comment (ignoring `;` inside quotes).
fn strip_inf_comment(line: &str) -> &str {
    let mut quoted = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ';' if !quoted => return line.get(..i).unwrap_or(line),
            _ => {}
        }
    }
    line
}

/// A stampinf keyword left in `line`, e.g. `$ARCH$` (but not `"$WINDOWS NT$"`).
fn unreplaced_token(line: &str) -> Option<&str> {
    let dollars: Vec<usize> = line.match_indices('$').map(|(i, _)| i).collect();
    dollars.windows(2).find_map(|w| {
        let (a, b) = (*w.first()?, *w.get(1)?);
        let inner = line.get(a + 1..b)?;
        let is_keyword = !inner.is_empty()
            && inner
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_');
        if is_keyword { line.get(a..=b) } else { None }
    })
}

/// Sanity-check stampinf's output: every keyword replaced, `DriverVer` and
/// `CatalogFile` set, and the driver and framework versions as requested.
fn check_stamped(text: &str, kmdf: &str, cat: &str, expected: &DriverVer) -> Result<(), String> {
    let mut problems = Vec::new();
    let mut catalog = None;
    for (n, raw) in text.lines().enumerate() {
        let line = strip_inf_comment(raw).trim();
        if let Some(tok) = unreplaced_token(line) {
            problems.push(format!("line {}: {tok} was not replaced", n + 1));
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "catalogfile" => catalog = Some(value.to_owned()),
            "kmdflibraryversion" if value != kmdf => problems.push(format!(
                "line {}: KmdfLibraryVersion = {value}, expected {kmdf}",
                n + 1
            )),
            _ => {}
        }
    }
    match DriverVer::from_inf(text) {
        Ok(found) if found == *expected => {}
        Ok(found) => problems.push(format!(
            "DriverVer = {},{}, expected {},{}",
            found.date, found.version, expected.date, expected.version
        )),
        Err(error) => problems.push(error),
    }
    if catalog.as_deref() != Some(cat) {
        problems.push(format!("CatalogFile = {catalog:?}, expected {cat}"));
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

/// Print `dir` recursively with file sizes.
fn print_tree(dir: &Path) -> Result<(), String> {
    fn walk(dir: &Path, depth: usize) -> Result<(), String> {
        let mut entries: Vec<_> = fs::read_dir(dir)
            .map_err(|e| format!("cannot list {}: {e}", dir.display()))?
            .flatten()
            .collect();
        entries.sort_by_key(|e| (e.path().is_file(), e.file_name()));
        for e in entries {
            let path = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            let indent = "  ".repeat(depth);
            if path.is_dir() {
                println!("  {indent}{name}/");
                walk(&path, depth + 1)?;
            } else {
                let len = e.metadata().map_or(0, |m| m.len());
                println!("  {indent}{name:<28} {len:>9}");
            }
        }
        Ok(())
    }
    println!("  {}/", dir.display());
    walk(dir, 1)
}

#[cfg(test)]
mod tests;
