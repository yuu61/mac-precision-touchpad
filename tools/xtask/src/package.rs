//! `cargo xtask package`: assemble, stamp, verify, catalog and (optionally)
//! sign the driver packages.
//!
//! Layout (under the host workspace's target dir):
//! ```text
//! target/package/<arch>/AmtPtpDevice/            AmtPtpDevice.inf + .cat, AmtPtpDeviceUsbKm.sys,
//!                                                AmtPtpDeviceUsbUm.dll, AmtPtpDeviceSpiKm.sys,
//!                                                AmtPtpHidFilter.sys
//! target/package/<arch>/AmtPtpDevice.symbols/    the matching .pdb files of that package
//! target/package/<arch>/AmtPtpHidFilter/         AmtPtpHidFilter.inf + .cat, AmtPtpHidFilter.sys
//! target/package/<arch>/AmtPtpHidFilter.symbols/ AmtPtpHidFilter.pdb
//! ```
//!
//! The `.symbols` directories sit outside the package directories so `Inf2Cat`
//! does not see them. Each is emptied together with its package, so its PDBs
//! always match that package's binaries. Both hold an `AmtPtpHidFilter.pdb`,
//! because both packages ship `AmtPtpHidFilter.sys` and may be built at
//! different times.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::cli::{Arch, PackageOpts};
use crate::drivers::{self, Workspace};
use crate::paths::{package_arch_dir, repo_root};
use crate::process;
use crate::wdk::{Tools, Wdk};

/// One driver binary: cargo builds `<name>.dll` (+ `<name>.pdb`); the INF's
/// `[SourceDisksFiles]` expects KMDF images as `<name>.sys`.
#[derive(Debug)]
struct Binary {
    ws: Workspace,
    name: &'static str,
}

impl Binary {
    fn packaged_name(&self) -> String {
        match self.ws {
            Workspace::Km => format!("{}.sys", self.name),
            Workspace::Um => format!("{}.dll", self.name),
        }
    }
}

const USB_KM: Binary = Binary {
    ws: Workspace::Km,
    name: "AmtPtpDeviceUsbKm",
};
const USB_UM: Binary = Binary {
    ws: Workspace::Um,
    name: "AmtPtpDeviceUsbUm",
};
const SPI_KM: Binary = Binary {
    ws: Workspace::Km,
    name: "AmtPtpDeviceSpiKm",
};
const HID_FILTER: Binary = Binary {
    ws: Workspace::Km,
    name: "AmtPtpHidFilter",
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InfKind {
    /// `packaging/AmtPtpDevice.inx`: all four drivers.
    Universal,
    /// `packaging/AmtPtpHidFilter.inx`: the standalone Magic Trackpad 2 filter package.
    HidFilter,
}

impl InfKind {
    pub const ALL: [Self; 2] = [Self::Universal, Self::HidFilter];

    /// INF/CAT base name and package directory name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Universal => "AmtPtpDevice",
            Self::HidFilter => "AmtPtpHidFilter",
        }
    }

    fn inx(self) -> PathBuf {
        repo_root()
            .join("packaging")
            .join(format!("{}.inx", self.name()))
    }

    /// Must match the INF's `[SourceDisksFiles]`.
    const fn binaries(self) -> &'static [Binary] {
        match self {
            Self::Universal => &[USB_KM, USB_UM, SPI_KM, HID_FILTER],
            Self::HidFilter => &[HID_FILTER],
        }
    }
}

/// The workspaces needed for `infs`, in build order.
fn needed_workspaces(infs: &[InfKind]) -> Vec<Workspace> {
    Workspace::ALL
        .into_iter()
        .filter(|ws| {
            infs.iter()
                .any(|i| i.binaries().iter().any(|b| b.ws == *ws))
        })
        .collect()
}

struct Context<'a> {
    opts: &'a PackageOpts,
    tools: Tools,
    signtool: Option<PathBuf>,
    kmdf: String,
    umdf: String,
}

pub fn package(opts: &PackageOpts) -> Result<(), String> {
    // Resolve everything that can fail cheaply before the (slow) builds.
    let wdk = Wdk::locate()?;
    let tools = wdk.packaging_tools()?;
    let signtool = match &opts.sign {
        Some(_) => Some(wdk.signtool()?),
        None => None,
    };
    for inf in &opts.infs {
        process::require_file(&inf.inx(), " (the INF source of this package)")?;
    }
    let kmdf = Workspace::Km.framework_version()?;
    let umdf = Workspace::Um.framework_version()?;
    println!(
        "xtask: WDK {} at {}; KMDF {kmdf}, UMDF {umdf}",
        wdk.version,
        wdk.root.display()
    );
    println!("xtask: stampinf: {}", tools.stampinf.display());
    println!("xtask: Inf2Cat:  {}", tools.inf2cat.display());
    println!("xtask: InfVerif: {}", tools.infverif.display());
    if let Some(s) = &signtool {
        println!("xtask: signtool: {}", s.display());
    }

    drivers::build_drivers(&opts.arches, opts.release, &needed_workspaces(&opts.infs))?;

    let cx = Context {
        opts,
        tools,
        signtool,
        kmdf,
        umdf,
    };
    for &arch in &opts.arches {
        for &inf in &opts.infs {
            package_one(&cx, arch, inf)
                .map_err(|e| format!("[{} {}] {e}", arch.name(), inf.name()))?;
        }
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

fn package_one(cx: &Context<'_>, arch: Arch, inf: InfKind) -> Result<(), String> {
    let opts = cx.opts;
    let arch_dir = package_arch_dir(arch);
    let pkg = arch_dir.join(inf.name());
    // Per-package symbols: both INFs ship AmtPtpHidFilter.sys, so a shared
    // directory would let one package's run replace the other's PDB.
    let symbols = arch_dir.join(format!("{}.symbols", inf.name()));
    let inf_file = format!("{}.inf", inf.name());
    let cat_file = format!("{}.cat", inf.name());
    println!(
        "xtask: === [{}] {} -> {}",
        arch.name(),
        inf.name(),
        pkg.display()
    );

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
    for b in inf.binaries() {
        let out = b.ws.output_dir(arch, opts.release);
        let dll = out.join(format!("{}.dll", b.name));
        process::require_file(
            &dll,
            &format!(" (expected from the drivers/{} build)", b.ws.dir_name()),
        )?;
        let dest = pkg.join(b.packaged_name());
        copy(&dll, &dest)?;
        // Before signtool and Inf2Cat hash the image (see clear_force_integrity).
        if matches!(b.ws, Workspace::Um) {
            clear_force_integrity(&dest)?;
        }
        payload.push(dest);
        let pdb = out.join(format!("{}.pdb", b.name));
        if pdb.is_file() {
            copy(&pdb, &symbols.join(format!("{}.pdb", b.name)))?;
        } else {
            println!("xtask: warning: no PDB at {}", pdb.display());
        }
    }

    // 2. INX -> INF (CRLF, the canonical INF line ending; the .inx is LF in git).
    let inx = inf.inx();
    let text =
        fs::read_to_string(&inx).map_err(|e| format!("cannot read {}: {e}", inx.display()))?;
    write(&pkg.join(&inf_file), &to_crlf(&text))?;

    // 3. stampinf: $ARCH$, $KMDFVERSION$, $UMDFVERSION$, DriverVer, CatalogFile.
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
        "-u",
        &cx.umdf,
    ]);
    process::run(&mut cmd, "stampinf")?;
    let stamped = fs::read_to_string(pkg.join(&inf_file))
        .map_err(|e| format!("cannot read stamped {inf_file}: {e}"))?;
    check_stamped(&stamped, &cx.kmdf, &cx.umdf, &cat_file)
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

/// `IMAGE_DLLCHARACTERISTICS_FORCE_INTEGRITY` (winnt.h).
const FORCE_INTEGRITY: u16 = 0x0080;

/// wdk-build 0.6.0 (lib.rs:1305) links every driver with `/INTEGRITYCHECK`.
/// link.exe cannot undo that: `/INTEGRITYCHECK:NO` is ignored (LNK4056) and
/// still sets the flag. The C AmtPtpDeviceUsbUm.dll never had
/// `FORCE_INTEGRITY` (shipped binary: `DllCharacteristics` 0x4160), and with the
/// flag an unsigned or test-signed UMDF DLL fails to load with 577 (P1).
/// So clear the bit in the packaged copy, before signtool and `Inf2Cat` hash it.
/// The KM `.sys` images keep it: signatures are enforced for them anyway, and
/// their non-zero PE checksum would have to be recomputed.
fn clear_force_integrity(path: &Path) -> Result<(), String> {
    let mut img = fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    clear_force_integrity_bytes(&mut img).map_err(|e| format!("{}: {e}", path.display()))?;
    fs::write(path, &img).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

fn clear_force_integrity_bytes(img: &mut [u8]) -> Result<(), String> {
    if !img.starts_with(b"MZ") {
        return Err("not a PE image (no MZ)".into());
    }
    let pe = u32::from_le_bytes(
        *img.get(0x3C..)
            .and_then(<[u8]>::first_chunk)
            .ok_or("truncated DOS header")?,
    ) as usize;
    let pe_header = img.get_mut(pe..).ok_or("no PE signature")?;
    if !pe_header.starts_with(b"PE\0\0") {
        return Err("no PE signature".into());
    }
    // The optional header follows the 4-byte signature and 20-byte file header.
    let optional = pe_header.get_mut(24..).ok_or("not a PE32+ image")?;
    if optional
        .first_chunk()
        .map(|magic| u16::from_le_bytes(*magic))
        != Some(0x20B)
    {
        return Err("not a PE32+ image".into());
    }
    // A non-zero CheckSum would also have to be recomputed; the UMDF link sets none.
    if optional.get(64..68) != Some([0; 4].as_slice()) {
        return Err("non-zero PE CheckSum; clearing FORCE_INTEGRITY would leave it stale".into());
    }
    let characteristics = optional
        .get_mut(70..)
        .and_then(<[u8]>::first_chunk_mut)
        .ok_or("truncated optional header")?;
    *characteristics = (u16::from_le_bytes(*characteristics) & !FORCE_INTEGRITY).to_le_bytes();
    Ok(())
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
fn check_stamped(text: &str, kmdf: &str, umdf: &str, cat: &str) -> Result<(), String> {
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
            "umdflibraryversion" if value != umdf => problems.push(format!(
                "line {}: UmdfLibraryVersion = {value}, expected {umdf}",
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
        assert_eq!(unreplaced_token("UmdfLibraryVersion=2.15.0"), None);
    }

    #[test]
    fn stamped_check() {
        let good = "[Version]\nSignature = \"$WINDOWS NT$\"\nCatalogFile = A.cat\n\
                    DriverVer = 11/26/2026,1.2.3.4\n[Manufacturer]\n\
                    %M%=Standard,NTamd64.10.0...16299 ; NT$ARCH$ in a comment is fine\n\
                    [w]\nKmdfLibraryVersion = 1.23\n[u]\nUmdfLibraryVersion=2.15.0\n";
        assert_eq!(check_stamped(good, "1.23", "2.15.0", "A.cat"), Ok(()));
        assert!(check_stamped(good, "1.25", "2.15.0", "A.cat").is_err());
        assert!(check_stamped(good, "1.23", "2.15.0", "B.cat").is_err());
        let unstamped = good.replace("NTamd64", "NT$ARCH$");
        assert!(check_stamped(&unstamped, "1.23", "2.15.0", "A.cat").is_err());
        let no_ver = good.replace("DriverVer = 11/26/2026,1.2.3.4", "DriverVer =");
        assert!(check_stamped(&no_ver, "1.23", "2.15.0", "A.cat").is_err());
    }

    #[test]
    fn binaries_match_inx_source_disks_files() {
        for inf in InfKind::ALL {
            let text = fs::read_to_string(inf.inx()).unwrap();
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
            let mut ours: Vec<String> = inf.binaries().iter().map(Binary::packaged_name).collect();
            listed.sort();
            ours.sort();
            assert_eq!(listed, ours, "{}", inf.name());
        }
    }

    #[test]
    fn universal_models_have_unique_supported_hardware_ids() {
        let text = fs::read_to_string(InfKind::Universal.inx()).unwrap();
        for (build, usb_section) in [
            ("16299", "amtptpdeviceusbkm_device"),
            ("17134", "amtptpdeviceusbum_install"),
        ] {
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

    /// Minimal PE32+ header: `e_lfanew` = 0x40, optional header at 0x58.
    fn pe32plus(dll_characteristics: u16, checksum: u32) -> Vec<u8> {
        let mut img = vec![0_u8; 0x100];
        img[0..2].copy_from_slice(b"MZ");
        img[0x3C..0x40].copy_from_slice(&0x40_u32.to_le_bytes());
        img[0x40..0x44].copy_from_slice(b"PE\0\0");
        img[0x58..0x5A].copy_from_slice(&0x20B_u16.to_le_bytes());
        img[0x58 + 64..0x58 + 68].copy_from_slice(&checksum.to_le_bytes());
        img[0x58 + 70..0x58 + 72].copy_from_slice(&dll_characteristics.to_le_bytes());
        img
    }

    fn dll_characteristics(img: &[u8]) -> u16 {
        u16::from_le_bytes([img[0x58 + 70], img[0x58 + 71]])
    }

    #[test]
    fn clears_force_integrity_only() {
        // What wdk-build's link produces: 0x1E0 = HIGH_ENTROPY_VA | DYNAMIC_BASE
        // | FORCE_INTEGRITY | NX_COMPAT.
        let mut img = pe32plus(0x01E0, 0);
        let before = img.clone();
        assert_eq!(clear_force_integrity_bytes(&mut img), Ok(()));
        assert_eq!(dll_characteristics(&img), 0x0160);
        // Only those two bytes change.
        let changed: Vec<usize> = (0..img.len()).filter(|&i| img[i] != before[i]).collect();
        assert_eq!(changed, vec![0x58 + 70]);
        // Idempotent, and a clear flag stays clear.
        assert_eq!(clear_force_integrity_bytes(&mut img), Ok(()));
        assert_eq!(dll_characteristics(&img), 0x0160);
    }

    #[test]
    fn force_integrity_rejects_unexpected_images() {
        assert!(clear_force_integrity_bytes(&mut pe32plus(0x01E0, 0x1234)).is_err());
        let mut no_mz = pe32plus(0x01E0, 0);
        no_mz[0] = b'X';
        assert!(clear_force_integrity_bytes(&mut no_mz).is_err());
        let mut no_pe = pe32plus(0x01E0, 0);
        no_pe[0x41] = b'X';
        assert!(clear_force_integrity_bytes(&mut no_pe).is_err());
        let mut pe32 = pe32plus(0x01E0, 0);
        pe32[0x58..0x5A].copy_from_slice(&0x10B_u16.to_le_bytes());
        assert!(clear_force_integrity_bytes(&mut pe32).is_err());
        let mut truncated = pe32plus(0x01E0, 0);
        truncated.truncate(0x58 + 71);
        assert!(clear_force_integrity_bytes(&mut truncated).is_err());
        assert!(clear_force_integrity_bytes(&mut []).is_err());
        let mut bad_lfanew = pe32plus(0x01E0, 0);
        bad_lfanew[0x3C..0x40].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(clear_force_integrity_bytes(&mut bad_lfanew).is_err());
    }

    #[test]
    fn workspaces_per_inf() {
        assert_eq!(
            needed_workspaces(&[InfKind::HidFilter]),
            vec![Workspace::Km]
        );
        assert_eq!(
            needed_workspaces(&InfKind::ALL),
            vec![Workspace::Km, Workspace::Um]
        );
    }
}
