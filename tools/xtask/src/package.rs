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

use crate::cli::{Arch, PackageOpts};
use crate::drivers;
use crate::paths::{package_arch_dir, repo_root};
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

    drivers::build_drivers(&opts.arches, opts.release)?;

    let cx = Context {
        opts,
        tools,
        signtool,
        kmdf,
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
        let out = drivers::output_dir(arch, opts.release);
        let dll = out.join(format!("{}.dll", b.name));
        process::require_file(&dll, " (expected from the drivers/km build)")?;
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
    let version = opts.driver_version.as_deref().unwrap_or("*");
    let mut cmd = Command::new(&cx.tools.stampinf);
    cmd.current_dir(&pkg).args([
        "-f",
        &inf_file,
        "-d",
        "*",
        "-a",
        arch.stampinf_arch(),
        "-c",
        &cat_file,
        "-v",
        version,
        "-k",
        &cx.kmdf,
    ]);
    process::run(&mut cmd, "stampinf")?;
    let stamped = fs::read_to_string(pkg.join(&inf_file))
        .map_err(|e| format!("cannot read stamped {inf_file}: {e}"))?;
    check_stamped(&stamped, &cx.kmdf, &cat_file)
        .map_err(|e| format!("stampinf output check failed for {inf_file}: {e}"))?;

    // 4. InfVerif in Universal mode (/u): the C package's
    //    DriverTargetPlatform was Universal (see packaging.md for why not /w).
    let mut cmd = Command::new(&cx.tools.infverif);
    cmd.current_dir(&pkg).args(["/u", inf_file.as_str()]);
    process::run(&mut cmd, "InfVerif /u")?;

    // 5. Sign the binaries before cataloging them.
    if let (Some(signtool), Some(args)) = (&cx.signtool, &opts.sign) {
        sign(signtool, args, &payload)?;
    }

    // 6. Inf2Cat. stampinf -d * writes the LOCAL date; /uselocaltime keeps
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
/// `CatalogFile` set, framework versions as requested.
fn check_stamped(text: &str, kmdf: &str, cat: &str) -> Result<(), String> {
    let mut problems = Vec::new();
    let mut driver_ver = None;
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
            "driverver" => driver_ver = Some(value.to_owned()),
            "catalogfile" => catalog = Some(value.to_owned()),
            "kmdflibraryversion" if value != kmdf => problems.push(format!(
                "line {}: KmdfLibraryVersion = {value}, expected {kmdf}",
                n + 1
            )),
            _ => {}
        }
    }
    match &driver_ver {
        Some(v) if v.contains(',') => {}
        Some(v) => problems.push(format!("DriverVer = `{v}` was not stamped (date,version)")),
        None => problems.push("no DriverVer".to_owned()),
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
mod tests {
    use super::*;

    #[test]
    fn crlf_normalization() {
        assert_eq!(to_crlf("a\nb\r\nc\n"), "a\r\nb\r\nc\r\n");
    }

    #[test]
    fn comments_and_tokens() {
        assert_eq!(strip_inf_comment("A = 1 ; c"), "A = 1 ");
        assert_eq!(strip_inf_comment("A = \"x;y\" ; c"), "A = \"x;y\" ");
        assert_eq!(unreplaced_token("Signature = \"$WINDOWS NT$\""), None);
        assert_eq!(
            unreplaced_token("%M%=Standard,NT$ARCH$.10.0...16299"),
            Some("$ARCH$")
        );
        assert_eq!(
            unreplaced_token("KmdfLibraryVersion = $KMDFVERSION$"),
            Some("$KMDFVERSION$")
        );
    }

    #[test]
    fn stamped_check() {
        let good = "[Version]\nSignature = \"$WINDOWS NT$\"\nCatalogFile = A.cat\n\
                    DriverVer = 11/26/2026,1.2.3.4\n[Manufacturer]\n\
                    %M%=Standard,NTamd64.10.0...16299 ; NT$ARCH$ in a comment is fine\n\
                    [w]\nKmdfLibraryVersion = 1.23\n";
        assert_eq!(check_stamped(good, "1.23", "A.cat"), Ok(()));
        assert!(check_stamped(good, "1.25", "A.cat").is_err());
        assert!(check_stamped(good, "1.23", "B.cat").is_err());
        let unstamped = good.replace("NTamd64", "NT$ARCH$");
        assert!(check_stamped(&unstamped, "1.23", "A.cat").is_err());
        let no_ver = good.replace("DriverVer = 11/26/2026,1.2.3.4", "DriverVer =");
        assert!(check_stamped(&no_ver, "1.23", "A.cat").is_err());
    }

    #[test]
    fn binaries_match_inx_source_disks_files() {
        let text = fs::read_to_string(inx()).unwrap();
        let section = text
            .split("[SourceDisksFiles]")
            .nth(1)
            .and_then(|s| s.split("\n[").next())
            .unwrap();
        let mut listed: Vec<String> = section
            .lines()
            .filter_map(|l| l.split('=').next())
            .map(|f| f.trim().to_owned())
            .filter(|f| !f.is_empty() && !f.starts_with(';'))
            .collect();
        let mut ours: Vec<String> = BINARIES.iter().map(Binary::packaged_name).collect();
        listed.sort();
        ours.sort();
        assert_eq!(listed, ours);
    }

    /// The INF sources in `packaging/`: only the universal one.
    #[test]
    fn one_inf_template() {
        let mut inx_files: Vec<String> = fs::read_dir(repo_root().join("packaging"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.to_ascii_lowercase().ends_with(".inx"))
            .collect();
        inx_files.sort();
        assert_eq!(inx_files, [format!("{PACKAGE}.inx")]);
    }

    #[test]
    fn universal_models_have_unique_supported_hardware_ids() {
        let text = fs::read_to_string(inx()).unwrap();
        {
            let build = "16299";
            let usb_section = "amtptpdeviceusbkm_device";
            let heading = format!("[Standard.NT$ARCH$.10.0...{build}]");
            let models = text
                .lines()
                .skip_while(|line| *line != heading)
                .skip(1)
                .take_while(|line| !line.starts_with('['));
            let mut bindings = std::collections::BTreeMap::new();
            for line in models {
                let line = line.split(';').next().unwrap().trim();
                if line.is_empty() {
                    continue;
                }
                let (_, entry) = line.split_once('=').unwrap();
                let (install, id) = entry.split_once(',').unwrap();
                let id = id.trim().to_ascii_lowercase();
                let duplicate = bindings.insert(id.clone(), install.trim().to_ascii_lowercase());
                assert!(duplicate.is_none(), "{heading}: duplicate binding for {id}");
            }
            for pid in ["0273", "0274", "0290", "0291"] {
                let id = format!("usb\\vid_05ac&pid_{pid}&mi_02");
                assert_eq!(bindings.get(&id).map(String::as_str), Some(usb_section));
            }
            assert!(!bindings.contains_key("spi\\vid_05ac&pid_0280&mi_02"));
            assert!(bindings.contains_key("usb\\vid_05ac&pid_0280&mi_02"));
        }
    }

    /// `(VID, PID)` of a hardware ID: `USB\Vid_05ac&Pid_0273&MI_02`, `SPI\VID_05ac&PID_0272&MI_02`,
    /// `HID\VID_05AC&PID_0265&...` or the Bluetooth form `HID\{...}_VID&0001004c_PID&0265&Col01`, whose
    /// 8-digit VID carries the vendor-ID source in its upper half.
    fn vid_pid(id: &str) -> (u16, u16) {
        let id = id.to_ascii_lowercase();
        let field = |key: &str| {
            let (_, rest) = id
                .split_once(key)
                .unwrap_or_else(|| panic!("{key} in {id}"));
            let digits: String = rest.chars().take_while(char::is_ascii_hexdigit).collect();
            let value = u32::from_str_radix(&digits, 16).unwrap_or_else(|e| panic!("{id}: {e}"));
            u16::try_from(value & 0xFFFF).unwrap()
        };
        let vid = if id.contains("vid&") {
            field("vid&")
        } else {
            field("vid_")
        };
        let pid = if id.contains("pid&") {
            field("pid&")
        } else {
            field("pid_")
        };
        (vid, pid)
    }

    /// Every `(install section, hardware ID)` of every decorated models section of the INF.
    fn model_bindings() -> Vec<(String, String, String)> {
        let text = fs::read_to_string(inx()).unwrap();
        let mut bindings = Vec::new();
        let mut section: Option<String> = None;
        for line in text.lines() {
            let line = strip_inf_comment(line).trim();
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                section = name
                    .starts_with("Standard.NT$ARCH$.")
                    .then(|| name.to_owned());
                continue;
            }
            let Some(section) = &section else { continue };
            if line.is_empty() {
                continue;
            }
            let (_, entry) = line.split_once('=').unwrap();
            let (install, id) = entry.split_once(',').unwrap();
            bindings.push((
                section.clone(),
                install.trim().to_ascii_lowercase(),
                id.trim().to_owned(),
            ));
        }
        assert!(!bindings.is_empty(), "{PACKAGE}: no models");
        bindings
    }

    /// Every binding of the INF template resolves to a model definition of the driver it installs:
    /// SPI to a table entry that also selects a report descriptor, the USB KMDF driver to a table
    /// row or a listed fallback PID, and the filter to a supported Magic Trackpad 2 transport.
    #[test]
    fn model_bindings_resolve_to_model_definitions() {
        let mut kmdf_fallbacks = std::collections::BTreeSet::new();
        for (section, install, id) in model_bindings() {
            let ctx = format!("{PACKAGE} [{section}] {install}, {id}");
            match install.as_str() {
                "amtptpdevicespikm_device" => {
                    let (vid, pid) = vid_pid(&id);
                    assert!(id.to_ascii_lowercase().starts_with("spi\\"), "{ctx}");
                    let trackpad = amtptp_spikm_core::device::lookup_trackpad(vid, pid);
                    assert!(trackpad.is_some(), "{ctx}: no SPI model");
                    assert!(
                        amtptp_spikm_core::device::family_for_product(pid).is_some(),
                        "{ctx}: no SPI descriptor family"
                    );
                }
                "amtptpdeviceusbkm_device" => {
                    let (vid, pid) = vid_pid(&id);
                    assert_eq!(vid, 0x05AC, "{ctx}");
                    let config = amtptp_usbkm_core::config::config_for_pid(pid);
                    if !config.matches(pid) {
                        assert!(
                            amtptp_usbkm_core::config::uses_listed_fallback(pid),
                            "{ctx}: falls back to the generic profile without being listed"
                        );
                        kmdf_fallbacks.insert(pid);
                    }
                }
                "amtptphidfilter_miniportdevice" => {
                    let (vid, pid) = vid_pid(&id);
                    assert!(
                        amtptp_hidfilter_core::Transport::from_ids(vid, pid).is_ok(),
                        "{ctx}: no Magic Trackpad 2 transport"
                    );
                }
                // The Bluetooth HID node itself (HidBth with the SDP/padding filter below it).
                "amtptpbthfilter_device" => {
                    let id = id.to_ascii_lowercase();
                    assert!(id.starts_with("bthenum\\{00001124-"), "{ctx}");
                    let (vid, pid) = vid_pid(&id);
                    assert!(
                        amtptp_hidfilter_core::Transport::from_ids(vid, pid).is_ok(),
                        "{ctx}: no Magic Trackpad 2 transport"
                    );
                }
                // The collections the filter disables or reads (Col03).
                "amtptphidfilter_nulldevice" => {}
                other => panic!("{ctx}: unknown install section {other}"),
            }
        }
        // No listed fallback PID is stale.
        let listed: std::collections::BTreeSet<u16> = amtptp_usbkm_core::config::FALLBACK_PIDS
            .into_iter()
            .collect();
        assert_eq!(kmdf_fallbacks, listed);
    }

    #[test]
    fn hardware_id_parsing() {
        assert_eq!(vid_pid("USB\\Vid_05ac&Pid_027A&MI_02"), (0x05AC, 0x027A));
        assert_eq!(vid_pid("SPI\\VID_05ac&PID_0272&MI_02"), (0x05AC, 0x0272));
        assert_eq!(
            vid_pid("HID\\{00001124-0000-1000-8000-00805f9b34fb}_VID&0001004c_PID&0265&Col01"),
            (0x004C, 0x0265)
        );
        assert_eq!(
            vid_pid("HID\\VID_05AC&PID_0265&REV_0855&MI_01&Col01"),
            (0x05AC, 0x0265)
        );
    }
}
