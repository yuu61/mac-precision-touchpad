use amtptp_core::spi::device::{family_for_product, lookup_trackpad};
use amtptp_usb_bt::bluetooth::Transport;
use amtptp_usb_bt::usb::config::{FALLBACK_PIDS, config_for_pid, uses_listed_fallback};

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
    let expected = DriverVer {
        date: "11/26/2026".to_owned(),
        version: "1.2.3.4".to_owned(),
    };
    assert_eq!(check_stamped(good, "1.23", "A.cat", &expected), Ok(()));
    assert!(check_stamped(good, "1.25", "A.cat", &expected).is_err());
    assert!(check_stamped(good, "1.23", "B.cat", &expected).is_err());
    let unstamped = good.replace("NTamd64", "NT$ARCH$");
    assert!(check_stamped(&unstamped, "1.23", "A.cat", &expected).is_err());
    let no_ver = good.replace("DriverVer = 11/26/2026,1.2.3.4", "DriverVer =");
    assert!(check_stamped(&no_ver, "1.23", "A.cat", &expected).is_err());
    let wrong_version = good.replace("1.2.3.4", "1.2.3.5");
    assert!(check_stamped(&wrong_version, "1.23", "A.cat", &expected).is_err());
    let wrong_date = good.replace("11/26/2026", "11/27/2026");
    assert!(check_stamped(&wrong_date, "1.23", "A.cat", &expected).is_err());
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
    let mut win10 = std::collections::BTreeMap::new();
    let mut win11 = std::collections::BTreeMap::new();
    for (section, install, id) in model_bindings() {
        let bindings = match section.as_str() {
            "Standard.NT$ARCH$.10.0...19044" => &mut win10,
            "Standard.NT$ARCH$.10.0...22000" => &mut win11,
            _ => panic!("unexpected models section {section}"),
        };
        let previous = bindings.insert(id.to_ascii_lowercase(), install);
        assert!(previous.is_none(), "{section}: duplicate binding for {id}");
    }
    let usb_section = "amtptpdeviceusbkm_device";
    for pid in ["0273", "0274", "0290", "0291"] {
        let id = format!("usb\\vid_05ac&pid_{pid}&mi_02");
        assert_eq!(win10.get(&id).map(String::as_str), Some(usb_section));
    }
    assert!(!win10.contains_key("spi\\vid_05ac&pid_0280&mi_02"));
    assert!(win10.contains_key("usb\\vid_05ac&pid_0280&mi_02"));

    // Only the three stacks with mshidkmdf as function driver change install sections.
    for install in win10.values_mut() {
        match install.as_str() {
            "amtptpdeviceusbkm_device"
            | "amtptpdevicespikm_device"
            | "amtptphidfilter_miniportdevice" => install.push_str("_win11"),
            "amtptpbthfilter_device" | "amtptphidfilter_nulldevice" => {}
            _ => panic!("unexpected Windows 10 install section {install}"),
        }
    }
    assert_eq!(win10, win11, "hardware bindings differ between OS builds");
}

#[test]
fn model_install_sections_register_their_lower_filters() {
    let text = fs::read_to_string(inx()).unwrap().to_ascii_lowercase();
    let installs: std::collections::BTreeSet<_> = model_bindings()
        .into_iter()
        .map(|(_, install, _)| install)
        .collect();
    for install in installs {
        let header = format!("[{install}.nt.filters]");
        let section = text
            .split_once(&header)
            .map(|(_, rest)| rest.split("\n[").next().unwrap().trim());
        if install == "amtptphidfilter_nulldevice" {
            assert!(section.is_none(), "{header}: null device has a filter");
        } else {
            let (service, _) = install.split_once('_').unwrap();
            let expected = format!("addfilter = {service},, ptplowerfilter");
            assert_eq!(section, Some(expected.as_str()), "{header}");
        }
    }
    assert!(text.contains("[ptplowerfilter]\nfilterposition = lower\n"));
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
        match install.strip_suffix("_win11").unwrap_or(&install) {
            "amtptpdevicespikm_device" => {
                let (vid, pid) = vid_pid(&id);
                assert!(id.to_ascii_lowercase().starts_with("spi\\"), "{ctx}");
                let trackpad = lookup_trackpad(vid, pid);
                assert!(trackpad.is_some(), "{ctx}: no SPI model");
                assert!(
                    family_for_product(pid).is_some(),
                    "{ctx}: no SPI descriptor family"
                );
            }
            "amtptpdeviceusbkm_device" => {
                let (vid, pid) = vid_pid(&id);
                assert_eq!(vid, 0x05AC, "{ctx}");
                let config = config_for_pid(pid);
                if !config.matches(pid) {
                    assert!(
                        uses_listed_fallback(pid),
                        "{ctx}: falls back to the generic profile without being listed"
                    );
                    kmdf_fallbacks.insert(pid);
                }
            }
            "amtptphidfilter_miniportdevice" => {
                let (vid, pid) = vid_pid(&id);
                assert!(
                    Transport::from_ids(vid, pid).is_ok(),
                    "{ctx}: no Magic Trackpad 2 transport"
                );
            }
            // The Bluetooth HID node itself (HidBth with the SDP/padding filter below it).
            "amtptpbthfilter_device" => {
                let id = id.to_ascii_lowercase();
                assert!(id.starts_with("bthenum\\{00001124-"), "{ctx}");
                let (vid, pid) = vid_pid(&id);
                assert!(
                    Transport::from_ids(vid, pid).is_ok(),
                    "{ctx}: no Magic Trackpad 2 transport"
                );
            }
            // The collections the filter disables or reads (Col03).
            "amtptphidfilter_nulldevice" => {}
            other => panic!("{ctx}: unknown install section {other}"),
        }
    }
    // No listed fallback PID is stale.
    let listed: std::collections::BTreeSet<u16> = FALLBACK_PIDS.into_iter().collect();
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
