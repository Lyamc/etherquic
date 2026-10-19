//! Binary Android XML manifest.
//!
//! Attribute resource ids are the framework ids from `android.R.attr`. The
//! package manager looks up `versionCode` and the SDK attributes by those ids,
//! and reads the application id from the un-namespaced `package` attribute.

const ANDROID_NS: &str = "http://schemas.android.com/apk/res/android";
const TYPE_REFERENCE: u8 = 0x01;
const TYPE_STRING: u8 = 0x03;
const TYPE_INT_DEC: u8 = 0x10;
const TYPE_INT_BOOLEAN: u8 = 0x12;
const NO_STRING: u32 = 0xffff_ffff;

pub fn manifest(application_id: &str, version_code: u32, version_name: &str) -> Vec<u8> {
    let mut pool = Pool::default();
    let version_code_name = pool.attr("versionCode", 0x0101_021b);
    let version_name_name = pool.attr("versionName", 0x0101_021c);
    let min_sdk_name = pool.attr("minSdkVersion", 0x0101_020c);
    let target_sdk_name = pool.attr("targetSdkVersion", 0x0101_0270);
    let name_name = pool.attr("name", 0x0101_0003);
    let label_name = pool.attr("label", 0x0101_0001);
    let exported_name = pool.attr("exported", 0x0101_0010);
    let debuggable_name = pool.attr("debuggable", 0x0101_000f);
    let has_code_name = pool.attr("hasCode", 0x0101_000c);
    let extract_libs_name = pool.attr("extractNativeLibs", 0x0101_04ea);
    let window_soft_name = pool.attr("windowSoftInputMode", 0x0101_0016);
    let config_changes_name = pool.attr("configChanges", 0x0101_001f);
    let theme_name = pool.attr("theme", 0x0101_0000);
    let value_name = pool.attr("value", 0x0101_0024);
    let package_name = pool.attr("package", 0);

    let uri = pool.string(ANDROID_NS);
    let prefix = pool.string("android");
    let manifest_elem = pool.string("manifest");
    let uses_sdk_elem = pool.string("uses-sdk");
    let uses_permission_elem = pool.string("uses-permission");
    let internet = pool.string("android.permission.INTERNET");
    let network_state = pool.string("android.permission.ACCESS_NETWORK_STATE");
    let application_elem = pool.string("application");
    let activity_elem = pool.string("activity");
    let meta_elem = pool.string("meta-data");
    let filter_elem = pool.string("intent-filter");
    let action_elem = pool.string("action");
    let category_elem = pool.string("category");
    let app_id = pool.string(application_id);
    let version_name_val = pool.string(version_name);
    let version_code_raw = pool.string(&version_code.to_string());
    let min_sdk_raw = pool.string("21");
    let target_sdk_raw = pool.string("35");
    let label_val = pool.string("EtherQuic Play");
    let activity_class = pool.string("android.app.NativeActivity");
    let lib_name_key = pool.string("android.app.lib_name");
    let lib_name_val = pool.string("etherquic_android");
    let true_val = pool.string("true");
    let false_val = pool.string("false");
    let window_soft_raw = pool.string("16");
    let config_raw = pool.string("1200");
    let action_main = pool.string("android.intent.action.MAIN");
    let category_launcher = pool.string("android.intent.category.LAUNCHER");

    let mut nodes = Vec::new();
    start_namespace(&mut nodes, prefix, uri);
    start_element(
        &mut nodes,
        NO_STRING,
        manifest_elem,
        &[
            Attr::string(0, package_name, NO_STRING, app_id),
            Attr::int(
                0x0101_021b,
                version_code_name,
                uri,
                version_code_raw,
                version_code,
            ),
            Attr::string(0x0101_021c, version_name_name, uri, version_name_val),
        ],
    );
    start_element(
        &mut nodes,
        NO_STRING,
        uses_sdk_elem,
        &[
            Attr::int(0x0101_020c, min_sdk_name, uri, min_sdk_raw, 21),
            Attr::int(0x0101_0270, target_sdk_name, uri, target_sdk_raw, 35),
        ],
    );
    end_element(&mut nodes, NO_STRING, uses_sdk_elem);
    start_element(
        &mut nodes,
        NO_STRING,
        uses_permission_elem,
        &[Attr::string(0x0101_0003, name_name, uri, internet)],
    );
    end_element(&mut nodes, NO_STRING, uses_permission_elem);
    start_element(
        &mut nodes,
        NO_STRING,
        uses_permission_elem,
        &[Attr::string(0x0101_0003, name_name, uri, network_state)],
    );
    end_element(&mut nodes, NO_STRING, uses_permission_elem);
    start_element(
        &mut nodes,
        NO_STRING,
        application_elem,
        &[
            Attr::string(0x0101_0001, label_name, uri, label_val),
            Attr::boolean(0x0101_000f, debuggable_name, uri, true_val, true),
            Attr::boolean(0x0101_000c, has_code_name, uri, false_val, false),
            Attr::boolean(0x0101_04ea, extract_libs_name, uri, true_val, true),
        ],
    );
    start_element(
        &mut nodes,
        NO_STRING,
        activity_elem,
        &[
            Attr::string(0x0101_0003, name_name, uri, activity_class),
            Attr::boolean(0x0101_0010, exported_name, uri, true_val, true),
            Attr::int(0x0101_0016, window_soft_name, uri, window_soft_raw, 16),
            Attr::int(0x0101_001f, config_changes_name, uri, config_raw, 0x04b0),
            // Theme.NoTitleBar.Fullscreen — NativeActivity has no Java layout.
            Attr::reference(0x0101_0000, theme_name, uri, 0x0103_0007),
        ],
    );
    start_element(
        &mut nodes,
        NO_STRING,
        meta_elem,
        &[
            Attr::string(0x0101_0003, name_name, uri, lib_name_key),
            Attr::string(0x0101_0024, value_name, uri, lib_name_val),
        ],
    );
    end_element(&mut nodes, NO_STRING, meta_elem);
    start_element(&mut nodes, NO_STRING, filter_elem, &[]);
    start_element(
        &mut nodes,
        NO_STRING,
        action_elem,
        &[Attr::string(0x0101_0003, name_name, uri, action_main)],
    );
    end_element(&mut nodes, NO_STRING, action_elem);
    start_element(
        &mut nodes,
        NO_STRING,
        category_elem,
        &[Attr::string(0x0101_0003, name_name, uri, category_launcher)],
    );
    end_element(&mut nodes, NO_STRING, category_elem);
    end_element(&mut nodes, NO_STRING, filter_elem);
    end_element(&mut nodes, NO_STRING, activity_elem);
    end_element(&mut nodes, NO_STRING, application_elem);
    end_element(&mut nodes, NO_STRING, manifest_elem);
    end_namespace(&mut nodes, prefix, uri);

    let pool_chunk = pool.encode();
    let map = resource_map(&pool.res_ids);
    let mut body = Vec::new();
    body.extend_from_slice(&pool_chunk);
    body.extend_from_slice(&map);
    body.extend_from_slice(&nodes);

    let mut file = Vec::with_capacity(8 + body.len());
    u16(&mut file, 0x0003);
    u16(&mut file, 8);
    u32(&mut file, (8 + body.len()) as u32);
    file.extend_from_slice(&body);
    file
}

#[derive(Clone)]
struct Attr {
    res_id: u32,
    name: u32,
    ns: u32,
    raw: u32,
    data_type: u8,
    data: u32,
}

impl Attr {
    fn string(res_id: u32, name: u32, ns: u32, value: u32) -> Self {
        Self {
            res_id,
            name,
            ns,
            raw: value,
            data_type: TYPE_STRING,
            data: value,
        }
    }

    fn int(res_id: u32, name: u32, ns: u32, raw: u32, value: u32) -> Self {
        Self {
            res_id,
            name,
            ns,
            raw,
            data_type: TYPE_INT_DEC,
            data: value,
        }
    }

    fn boolean(res_id: u32, name: u32, ns: u32, raw: u32, on: bool) -> Self {
        Self {
            res_id,
            name,
            ns,
            raw,
            data_type: TYPE_INT_BOOLEAN,
            data: if on { 0xffff_ffff } else { 0 },
        }
    }

    fn reference(res_id: u32, name: u32, ns: u32, value: u32) -> Self {
        Self {
            res_id,
            name,
            ns,
            raw: NO_STRING,
            data_type: TYPE_REFERENCE,
            data: value,
        }
    }
}

#[derive(Default)]
struct Pool {
    strings: Vec<String>,
    res_ids: Vec<u32>,
}

impl Pool {
    fn attr(&mut self, name: &str, id: u32) -> u32 {
        let index = self.strings.len() as u32;
        self.strings.push(name.to_string());
        self.res_ids.push(id);
        index
    }

    fn string(&mut self, value: &str) -> u32 {
        let index = self.strings.len() as u32;
        self.strings.push(value.to_string());
        index
    }

    fn encode(&self) -> Vec<u8> {
        let mut data = Vec::new();
        let mut offsets = Vec::with_capacity(self.strings.len());
        for value in &self.strings {
            let units: Vec<u16> = value.encode_utf16().collect();
            assert!(units.len() <= 0x7fff, "manifest string is too long");
            offsets.push(data.len() as u32);
            data.extend_from_slice(&(units.len() as u16).to_le_bytes());
            for unit in units {
                data.extend_from_slice(&unit.to_le_bytes());
            }
            data.extend_from_slice(&0u16.to_le_bytes());
        }
        while data.len() % 4 != 0 {
            data.push(0);
        }

        let count = self.strings.len() as u32;
        let header_size = 28u32;
        let strings_start = header_size + count * 4;
        let size = strings_start + data.len() as u32;
        let mut chunk = Vec::with_capacity(size as usize);
        u16(&mut chunk, 0x0001);
        u16(&mut chunk, header_size as u16);
        u32(&mut chunk, size);
        u32(&mut chunk, count);
        u32(&mut chunk, 0);
        u32(&mut chunk, 0);
        u32(&mut chunk, strings_start);
        u32(&mut chunk, 0);
        for offset in offsets {
            u32(&mut chunk, offset);
        }
        chunk.extend_from_slice(&data);
        debug_assert_eq!(chunk.len(), size as usize);
        chunk
    }
}

fn resource_map(ids: &[u32]) -> Vec<u8> {
    let size = 8 + ids.len() * 4;
    let mut chunk = Vec::with_capacity(size);
    u16(&mut chunk, 0x0180);
    u16(&mut chunk, 8);
    u32(&mut chunk, size as u32);
    for id in ids {
        u32(&mut chunk, *id);
    }
    chunk
}

fn start_namespace(buf: &mut Vec<u8>, prefix: u32, uri: u32) {
    node_header(buf, 0x0100, 0x18);
    u32(buf, prefix);
    u32(buf, uri);
}

fn end_namespace(buf: &mut Vec<u8>, prefix: u32, uri: u32) {
    node_header(buf, 0x0101, 0x18);
    u32(buf, prefix);
    u32(buf, uri);
}

fn end_element(buf: &mut Vec<u8>, ns: u32, name: u32) {
    node_header(buf, 0x0103, 0x18);
    u32(buf, ns);
    u32(buf, name);
}

fn start_element(buf: &mut Vec<u8>, ns: u32, name: u32, attrs: &[Attr]) {
    let mut ordered = attrs.to_vec();
    ordered.sort_by(|left, right| {
        left.res_id
            .cmp(&right.res_id)
            .then(left.name.cmp(&right.name))
    });
    let size = 16 + 20 + ordered.len() * 20;
    node_header(buf, 0x0102, size as u32);
    u32(buf, ns);
    u32(buf, name);
    u16(buf, 0x14);
    u16(buf, 0x14);
    u16(buf, ordered.len() as u16);
    u16(buf, 0);
    u16(buf, 0);
    u16(buf, 0);
    for attr in &ordered {
        u32(buf, attr.ns);
        u32(buf, attr.name);
        u32(buf, attr.raw);
        u16(buf, 8);
        buf.push(0);
        buf.push(attr.data_type);
        u32(buf, attr.data);
    }
}

fn node_header(buf: &mut Vec<u8>, kind: u16, size: u32) {
    u16(buf, kind);
    u16(buf, 0x10);
    u32(buf, size);
    u32(buf, 1);
    u32(buf, NO_STRING);
}

fn u16(buf: &mut Vec<u8>, value: u16) {
    buf.extend_from_slice(&value.to_le_bytes());
}

fn u32(buf: &mut Vec<u8>, value: u32) {
    buf.extend_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::manifest;

    #[test]
    fn manifest_carries_the_application_id_and_version() {
        let xml = manifest("dev.etherquic.play", 7, "1.2.3");
        assert!(xml.len() > 8);
        assert_eq!(u16::from_le_bytes(xml[0..2].try_into().unwrap()), 0x0003);
        assert_eq!(u16::from_le_bytes(xml[2..4].try_into().unwrap()), 8);
        let encoded: Vec<u8> = "dev.etherquic.play"
            .encode_utf16()
            .flat_map(|unit| unit.to_le_bytes())
            .collect();
        assert!(
            xml.windows(encoded.len())
                .any(|window| window == encoded.as_slice()),
            "application id is not in the binary manifest"
        );
        assert!(xml.windows(4).any(|window| window == 7u32.to_le_bytes()));
        assert!(
            xml.windows(4).any(|window| window == 35u32.to_le_bytes()),
            "targetSdkVersion 35 is not in the binary manifest"
        );
        for permission in [
            "android.permission.INTERNET",
            "android.permission.ACCESS_NETWORK_STATE",
        ] {
            let encoded: Vec<u8> = permission
                .encode_utf16()
                .flat_map(|unit| unit.to_le_bytes())
                .collect();
            assert!(
                xml.windows(encoded.len())
                    .any(|window| window == encoded.as_slice()),
                "{permission} is not in the binary manifest"
            );
        }
    }
}
