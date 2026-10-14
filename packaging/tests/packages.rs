//! Opens the Windows and Android packages the build just wrote.

#![forbid(unsafe_code)]

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use etherquic_installers::{APPLICATION_ID, PRODUCT_NAME, UPGRADE_CODE};

#[test]
fn built_packages_share_one_identity_and_the_second_version_is_higher() {
    let dir = std::env::temp_dir().join("etherquic-installer-packages");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create package output directory");

    let windows_low = dir.join("etherquic-play-1.0.0.msi");
    let windows_high = dir.join("etherquic-play-1.1.0.msi");
    let android_low = dir.join("etherquic-play-1.apk");
    let android_high = dir.join("etherquic-play-2.apk");

    etherquic_installers::build_windows_installer(&windows_low, "1.0.0")
        .expect("build lower windows installer");
    etherquic_installers::build_windows_installer(&windows_high, "1.1.0")
        .expect("build higher windows installer");
    etherquic_installers::build_android_package(&android_low, 1, "1.0.0")
        .expect("build lower android package");
    etherquic_installers::build_android_package(&android_high, 2, "1.1.0")
        .expect("build higher android package");

    let low = read_msi(&windows_low);
    let high = read_msi(&windows_high);
    let low_app = read_apk(&android_low);
    let high_app = read_apk(&android_high);

    println!(
        "windows package={} identity={} name={} version={}",
        show(&windows_low),
        low.upgrade_code,
        low.name,
        low.version
    );
    println!(
        "windows package={} identity={} name={} version={}",
        show(&windows_high),
        high.upgrade_code,
        high.name,
        high.version
    );
    println!(
        "android package={} applicationId={} versionCode={} versionName={}",
        show(&android_low),
        low_app.application_id,
        low_app.version_code,
        low_app.version_name
    );
    println!(
        "android package={} applicationId={} versionCode={} versionName={}",
        show(&android_high),
        high_app.application_id,
        high_app.version_code,
        high_app.version_name
    );

    assert_eq!(
        guid(&low.upgrade_code),
        guid(&high.upgrade_code),
        "windows packages name different product identities"
    );
    assert_eq!(
        guid(&low.upgrade_code),
        guid(UPGRADE_CODE),
        "windows upgrade code was not the EtherQuic Play product identity"
    );
    assert_eq!(low.name, high.name, "windows product names differ");
    assert_eq!(low.name, PRODUCT_NAME);
    assert_ne!(
        guid(&low.product_code),
        guid(&high.product_code),
        "major upgrade packages need distinct product codes"
    );
    assert_eq!(
        cmp_version(&low.version, "1.0.0"),
        Ordering::Equal,
        "lower windows package did not embed 1.0.0, read {}",
        low.version
    );
    assert_eq!(
        cmp_version(&high.version, "1.1.0"),
        Ordering::Equal,
        "higher windows package did not embed 1.1.0, read {}",
        high.version
    );
    assert_eq!(
        cmp_version(&high.version, &low.version),
        Ordering::Greater,
        "windows version {} is not greater than {}",
        high.version,
        low.version
    );

    assert_eq!(
        low_app.application_id, high_app.application_id,
        "android packages name different application ids"
    );
    assert_eq!(low_app.application_id, APPLICATION_ID);
    assert!(!low_app.application_id.is_empty());
    assert_eq!(low_app.version_code, 1, "lower android version code");
    assert_eq!(high_app.version_code, 2, "higher android version code");
    assert!(
        high_app.version_code > low_app.version_code,
        "android version code {} is not greater than {}",
        high_app.version_code,
        low_app.version_code
    );

    println!("assertions=passed");
}

struct Msi {
    name: String,
    version: String,
    upgrade_code: String,
    product_code: String,
}

struct Apk {
    application_id: String,
    version_code: u32,
    version_name: String,
}

fn read_msi(path: &Path) -> Msi {
    let bytes = fs::read(path).unwrap_or_else(|error| panic!("open {}: {error}", path.display()));
    assert!(
        bytes.len() > 8 && bytes[..8] == [0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1],
        "{} is not an MSI compound file",
        path.display()
    );
    let output = tool(
        "msiinfo",
        &["export", &path.display().to_string(), "Property"],
    );
    assert!(
        output.status.success(),
        "msiinfo export {} failed: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    let mut properties = BTreeMap::new();
    for (index, line) in text.lines().enumerate() {
        if index < 3 {
            continue;
        }
        let mut columns = line.split('\t');
        let key = columns.next().unwrap_or("").trim();
        let value = columns.next().unwrap_or("").trim();
        if !key.is_empty() {
            properties.insert(key.to_string(), value.to_string());
        }
    }
    Msi {
        name: required(&properties, "ProductName", path),
        version: required(&properties, "ProductVersion", path),
        upgrade_code: required(&properties, "UpgradeCode", path),
        product_code: required(&properties, "ProductCode", path),
    }
}

fn read_apk(path: &Path) -> Apk {
    let bytes = fs::read(path).unwrap_or_else(|error| panic!("open {}: {error}", path.display()));
    assert!(
        bytes.len() > 4 && &bytes[..4] == b"PK\x03\x04",
        "{} is not a zip apk",
        path.display()
    );
    assert!(
        bytes
            .windows(19)
            .any(|window| window == b"AndroidManifest.xml"),
        "{} has no AndroidManifest.xml entry",
        path.display()
    );
    assert!(
        bytes.windows(11).any(|window| window == b"classes.dex"),
        "{} has no classes.dex entry",
        path.display()
    );
    assert_classes_dex(path, &bytes);
    let verify = tool(
        "apksigner",
        &["verify", "--verbose", &path.display().to_string()],
    );
    assert!(
        verify.status.success(),
        "apksigner verify {} failed\n{}{}",
        path.display(),
        String::from_utf8_lossy(&verify.stdout),
        String::from_utf8_lossy(&verify.stderr)
    );

    let badging = tool("aapt2", &["dump", "badging", &path.display().to_string()]);
    let stdout = String::from_utf8_lossy(&badging.stdout).to_string();
    let stderr = String::from_utf8_lossy(&badging.stderr).to_string();
    if !badging.status.success() || !stdout.lines().any(|line| line.starts_with("package:")) {
        let tree = tool(
            "aapt2",
            &[
                "dump",
                "xmltree",
                &path.display().to_string(),
                "--file",
                "AndroidManifest.xml",
            ],
        );
        panic!(
            "aapt2 dump badging {} failed\nstdout:{stdout}\nstderr:{stderr}\nxmltree:{}{}",
            path.display(),
            String::from_utf8_lossy(&tree.stdout),
            String::from_utf8_lossy(&tree.stderr)
        );
    }
    let package = stdout
        .lines()
        .find(|line| line.starts_with("package:"))
        .expect("package line");
    Apk {
        application_id: quoted_field(package, "name=")
            .unwrap_or_else(|| panic!("application id missing from {package}")),
        version_code: quoted_field(package, "versionCode=")
            .and_then(|value| value.parse().ok())
            .unwrap_or_else(|| panic!("version code missing from {package}")),
        version_name: quoted_field(package, "versionName=")
            .unwrap_or_else(|| panic!("version name missing from {package}")),
    }
}

fn assert_classes_dex(path: &Path, apk: &[u8]) {
    let dex = stored_entry(apk, "classes.dex");
    assert_eq!(
        &dex[..8],
        b"dex\n035\0",
        "{} classes.dex magic",
        path.display()
    );
    let string_ids_size = u32_at(&dex, 0x38) as usize;
    let string_ids_off = u32_at(&dex, 0x3c) as usize;
    let type_ids_size = u32_at(&dex, 0x40) as usize;
    let type_ids_off = u32_at(&dex, 0x44) as usize;
    let method_ids_size = u32_at(&dex, 0x58) as usize;
    let method_ids_off = u32_at(&dex, 0x5c) as usize;
    let strings: Vec<String> = (0..string_ids_size)
        .map(|index| read_dex_string(&dex, string_ids_off, index))
        .collect();
    assert!(
        strings.windows(2).all(|pair| pair[0] < pair[1]),
        "{} string_ids are out of order: {strings:?}",
        path.display()
    );
    let mut type_names = Vec::new();
    let mut previous_descriptor = 0u32;
    for index in 0..type_ids_size {
        let descriptor = u32_at(&dex, type_ids_off + index * 4);
        if index > 0 {
            assert!(
                descriptor > previous_descriptor,
                "{} type_ids are out of order",
                path.display()
            );
        }
        previous_descriptor = descriptor;
        type_names.push(strings[descriptor as usize].clone());
    }
    let mut methods = Vec::new();
    let mut previous: Option<(u16, u32, u16)> = None;
    for index in 0..method_ids_size {
        let at = method_ids_off + index * 8;
        let class_idx = u16_at(&dex, at);
        let proto_idx = u16_at(&dex, at + 2);
        let name_idx = u32_at(&dex, at + 4);
        let key = (class_idx, name_idx, proto_idx);
        if let Some(before) = previous {
            assert!(
                key > before,
                "{} method_ids are out of order",
                path.display()
            );
        }
        previous = Some(key);
        methods.push((
            type_names[class_idx as usize].clone(),
            strings[name_idx as usize].clone(),
        ));
    }
    let map_off = u32_at(&dex, 0x34) as usize;
    let map_count = u32_at(&dex, map_off) as usize;
    let mut map_types = Vec::new();
    let mut map_offsets = Vec::new();
    let mut class_data_off = 0usize;
    let mut code_off = 0usize;
    for index in 0..map_count {
        let at = map_off + 4 + index * 12;
        let kind = u16_at(&dex, at);
        let offset = u32_at(&dex, at + 8);
        if index > 0 {
            assert!(
                kind > map_types[index - 1],
                "{} map types are not strictly increasing: {map_types:?} then {kind:#x}",
                path.display()
            );
            assert!(
                offset > map_offsets[index - 1],
                "{} map offsets are not strictly increasing: {map_offsets:?}",
                path.display()
            );
        }
        map_types.push(kind);
        map_offsets.push(offset);
        if kind == 0x2000 {
            class_data_off = offset as usize;
        }
        if kind == 0x2001 {
            code_off = offset as usize;
        }
    }
    assert_eq!(
        map_types,
        vec![0x0000, 0x0001, 0x0002, 0x0003, 0x0005, 0x0006, 0x1000, 0x2000, 0x2001, 0x2002,],
        "{} classes.dex map types",
        path.display()
    );
    assert!(code_off > 0 && class_data_off > 0);
    let invoke_method = u16_at(&dex, code_off + 18) as usize;
    let (invoke_class, invoke_name) = &methods[invoke_method];
    assert_eq!(invoke_class, "Landroid/app/Activity;");
    assert_eq!(invoke_name, "<init>");
    let mut cursor = class_data_off;
    for _ in 0..4 {
        let (_, next) = read_uleb(&dex, cursor);
        cursor = next;
    }
    let (direct_method, _) = read_uleb(&dex, cursor);
    let (direct_class, direct_name) = &methods[direct_method as usize];
    assert_eq!(direct_class, "Ldev/etherquic/play/MainActivity;");
    assert_eq!(direct_name, "<init>");
    println!(
        "android dex package={} strings={strings:?} map={map_types:?} invoke={invoke_class}.{invoke_name} direct={direct_class}.{direct_name}",
        path.display()
    );
}

fn stored_entry(archive: &[u8], name: &str) -> Vec<u8> {
    let mut offset = 0usize;
    while offset + 30 <= archive.len() && &archive[offset..offset + 4] == b"PK\x03\x04" {
        let method = u16_at(archive, offset + 8);
        let size = u32_at(archive, offset + 18) as usize;
        let name_len = u16_at(archive, offset + 26) as usize;
        let extra_len = u16_at(archive, offset + 28) as usize;
        let name_at = offset + 30;
        let data_at = name_at + name_len + extra_len;
        let entry_name = std::str::from_utf8(&archive[name_at..name_at + name_len]).unwrap_or("");
        if entry_name == name {
            assert_eq!(method, 0, "{name} is compressed");
            return archive[data_at..data_at + size].to_vec();
        }
        offset = data_at + size;
    }
    panic!("archive has no stored {name}");
}

fn read_dex_string(dex: &[u8], string_ids_off: usize, index: usize) -> String {
    let data_off = u32_at(dex, string_ids_off + index * 4) as usize;
    let (_len, start) = read_uleb(dex, data_off);
    let end = dex[start..]
        .iter()
        .position(|byte| *byte == 0)
        .map(|at| start + at)
        .unwrap_or(dex.len());
    String::from_utf8(dex[start..end].to_vec()).expect("dex string")
}

fn read_uleb(bytes: &[u8], mut at: usize) -> (u32, usize) {
    let mut value = 0u32;
    let mut shift = 0;
    loop {
        let byte = bytes[at];
        at += 1;
        value |= u32::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return (value, at);
        }
        shift += 7;
    }
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap())
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn required(properties: &BTreeMap<String, String>, key: &str, path: &Path) -> String {
    properties.get(key).cloned().unwrap_or_else(|| {
        panic!(
            "{key} missing from {} properties {properties:?}",
            path.display()
        )
    })
}

fn quoted_field(line: &str, key: &str) -> Option<String> {
    let start = line.find(key)? + key.len();
    let rest = line[start..].strip_prefix('\'')?;
    let end = rest.find('\'')?;
    Some(rest[..end].to_string())
}

fn guid(value: &str) -> String {
    value
        .trim()
        .trim_matches(|ch| ch == '{' || ch == '}')
        .to_ascii_lowercase()
}

fn cmp_version(left: &str, right: &str) -> Ordering {
    let left = version_parts(left);
    let right = version_parts(right);
    let width = left.len().max(right.len());
    for index in 0..width {
        let a = left.get(index).copied().unwrap_or(0);
        let b = right.get(index).copied().unwrap_or(0);
        if a != b {
            return a.cmp(&b);
        }
    }
    Ordering::Equal
}

fn version_parts(version: &str) -> Vec<u64> {
    version
        .split('.')
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

fn show(path: &Path) -> String {
    fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

fn tool(program: &str, args: &[&str]) -> std::process::Output {
    Command::new(program).args(args).output().unwrap_or_else(|error| {
        panic!(
            "could not run {program}: {error}. Enter `nix-shell nix/installers.nix` so the installer tools are on PATH."
        )
    })
}
