//! A one-class `classes.dex` for the thin Play activity.
//!
//! `dev.etherquic.play.MainActivity` extends `android.app.Activity` and its
//! constructor calls `Activity.<init>`. String, type, and method ids are in
//! the order ART requires, and the map lists class data (`0x2000`), code
//! (`0x2001`), and string data (`0x2002`) at increasing offsets.

use crate::hash::{adler32, sha1};

const NO_INDEX: u32 = 0xffff_ffff;
const ACC_PUBLIC: u32 = 0x1;
const ACC_CONSTRUCTOR: u32 = 0x0001_0000;

const MAP_HEADER: u16 = 0x0000;
const MAP_STRING_ID: u16 = 0x0001;
const MAP_TYPE_ID: u16 = 0x0002;
const MAP_PROTO_ID: u16 = 0x0003;
const MAP_METHOD_ID: u16 = 0x0005;
const MAP_CLASS_DEF: u16 = 0x0006;
const MAP_MAP_LIST: u16 = 0x1000;
const MAP_CLASS_DATA: u16 = 0x2000;
const MAP_CODE: u16 = 0x2001;
const MAP_STRING_DATA: u16 = 0x2002;

pub fn activity_dex() -> Vec<u8> {
    // UTF-16 code-unit order. '<' sorts before 'L', and "Landroid..." before "Ldev...".
    let strings = [
        "<init>",
        "Landroid/app/Activity;",
        "Ldev/etherquic/play/MainActivity;",
        "V",
    ];
    let string_ids_size = strings.len() as u32;
    let type_ids_size = 3u32;
    let proto_ids_size = 1u32;
    let method_ids_size = 2u32;
    let class_defs_size = 1u32;

    let mut string_data = Vec::new();
    let mut string_at = Vec::new();
    for value in strings {
        string_at.push(string_data.len());
        string_data.extend(uleb128(value.len() as u32));
        string_data.extend_from_slice(value.as_bytes());
        string_data.push(0);
    }

    let string_ids_off = 0x70usize;
    let type_ids_off = string_ids_off + string_ids_size as usize * 4;
    let proto_ids_off = type_ids_off + type_ids_size as usize * 4;
    let method_ids_off = proto_ids_off + proto_ids_size as usize * 12;
    let class_defs_off = method_ids_off + method_ids_size as usize * 8;
    let map_items = 10usize;
    let map_len = 4 + map_items * 12;
    let map_off = align4(class_defs_off + class_defs_size as usize * 32);
    let class_data_off = map_off + map_len;
    let code_len = 16 + 8;
    let code_off = solve_code_off(class_data_off);
    let class_data = class_data_bytes(code_off as u32);
    debug_assert_eq!(align4(class_data_off + class_data.len()), code_off);
    let string_data_off = code_off + code_len;
    // data_size must be a multiple of 4. The id sections end on a 4-byte
    // boundary, so rounding the file up keeps the data section aligned.
    let file_size = align4(string_data_off + string_data.len());

    let mut dex = vec![0u8; file_size];
    dex[..8].copy_from_slice(b"dex\n035\0");
    poke_u32(&mut dex, 0x20, file_size as u32);
    poke_u32(&mut dex, 0x24, 0x70);
    poke_u32(&mut dex, 0x28, 0x1234_5678);
    poke_u32(&mut dex, 0x34, map_off as u32);
    poke_u32(&mut dex, 0x38, string_ids_size);
    poke_u32(&mut dex, 0x3c, string_ids_off as u32);
    poke_u32(&mut dex, 0x40, type_ids_size);
    poke_u32(&mut dex, 0x44, type_ids_off as u32);
    poke_u32(&mut dex, 0x48, proto_ids_size);
    poke_u32(&mut dex, 0x4c, proto_ids_off as u32);
    poke_u32(&mut dex, 0x58, method_ids_size);
    poke_u32(&mut dex, 0x5c, method_ids_off as u32);
    poke_u32(&mut dex, 0x60, class_defs_size);
    poke_u32(&mut dex, 0x64, class_defs_off as u32);
    poke_u32(&mut dex, 0x68, (file_size - map_off) as u32);
    poke_u32(&mut dex, 0x6c, map_off as u32);

    for (index, relative) in string_at.iter().enumerate() {
        poke_u32(
            &mut dex,
            string_ids_off + index * 4,
            (string_data_off + relative) as u32,
        );
    }
    // type_ids sorted by descriptor: Activity, MainActivity, V.
    poke_u32(&mut dex, type_ids_off, 1);
    poke_u32(&mut dex, type_ids_off + 4, 2);
    poke_u32(&mut dex, type_ids_off + 8, 3);
    // proto ()V, shorty and return type are "V".
    poke_u32(&mut dex, proto_ids_off, 3);
    poke_u32(&mut dex, proto_ids_off + 4, 2);
    poke_u32(&mut dex, proto_ids_off + 8, 0);
    // method_ids sorted by class: Activity.<init>, then MainActivity.<init>.
    write_method(&mut dex, method_ids_off, 0, 0, 0);
    write_method(&mut dex, method_ids_off + 8, 1, 0, 0);
    write_class_def(&mut dex, class_defs_off, 1, 0, class_data_off as u32);

    let map = [
        (MAP_HEADER, 1u32, 0u32),
        (MAP_STRING_ID, string_ids_size, string_ids_off as u32),
        (MAP_TYPE_ID, type_ids_size, type_ids_off as u32),
        (MAP_PROTO_ID, proto_ids_size, proto_ids_off as u32),
        (MAP_METHOD_ID, method_ids_size, method_ids_off as u32),
        (MAP_CLASS_DEF, class_defs_size, class_defs_off as u32),
        (MAP_MAP_LIST, 1, map_off as u32),
        (MAP_CLASS_DATA, 1, class_data_off as u32),
        (MAP_CODE, 1, code_off as u32),
        (MAP_STRING_DATA, string_ids_size, string_data_off as u32),
    ];
    debug_assert_eq!(map.len(), map_items);
    poke_u32(&mut dex, map_off, map.len() as u32);
    for (index, (kind, size, offset)) in map.iter().enumerate() {
        let at = map_off + 4 + index * 12;
        dex[at..at + 2].copy_from_slice(&kind.to_le_bytes());
        dex[at + 2..at + 4].copy_from_slice(&0u16.to_le_bytes());
        poke_u32(&mut dex, at + 4, *size);
        poke_u32(&mut dex, at + 8, *offset);
    }

    dex[class_data_off..class_data_off + class_data.len()].copy_from_slice(&class_data);
    write_code(&mut dex, code_off);
    dex[string_data_off..string_data_off + string_data.len()].copy_from_slice(&string_data);

    let signature = sha1(&dex[32..]);
    dex[12..32].copy_from_slice(&signature);
    let checksum = adler32(&dex[12..]);
    poke_u32(&mut dex, 8, checksum);
    dex
}

fn solve_code_off(class_data_off: usize) -> usize {
    let mut code_off = align4(class_data_off + 8);
    for _ in 0..4 {
        let next = align4(class_data_off + class_data_bytes(code_off as u32).len());
        if next == code_off {
            return code_off;
        }
        code_off = next;
    }
    code_off
}

fn class_data_bytes(code_off: u32) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend(uleb128(0));
    data.extend(uleb128(0));
    data.extend(uleb128(1));
    data.extend(uleb128(0));
    // First direct method is MainActivity.<init>, which sorts as method id 1.
    data.extend(uleb128(1));
    data.extend(uleb128(ACC_PUBLIC | ACC_CONSTRUCTOR));
    data.extend(uleb128(code_off));
    data
}

fn write_code(dex: &mut [u8], code_off: usize) {
    let insns: [u16; 4] = [
        0x1070, // invoke-direct, one argument in v0
        0x0000, // Activity.<init>, method id 0 after sorting
        0x0000, // register v0
        0x000e, // return-void
    ];
    let mut at = code_off;
    for value in [1u16, 1, 1, 0] {
        dex[at..at + 2].copy_from_slice(&value.to_le_bytes());
        at += 2;
    }
    poke_u32(dex, at, 0);
    at += 4;
    poke_u32(dex, at, insns.len() as u32);
    at += 4;
    for insn in insns {
        dex[at..at + 2].copy_from_slice(&insn.to_le_bytes());
        at += 2;
    }
    debug_assert_eq!(at - code_off, 24);
}

fn write_method(dex: &mut [u8], at: usize, class_idx: u16, proto_idx: u16, name_idx: u32) {
    dex[at..at + 2].copy_from_slice(&class_idx.to_le_bytes());
    dex[at + 2..at + 4].copy_from_slice(&proto_idx.to_le_bytes());
    poke_u32(dex, at + 4, name_idx);
}

fn write_class_def(
    dex: &mut [u8],
    at: usize,
    class_idx: u32,
    superclass_idx: u32,
    class_data_off: u32,
) {
    poke_u32(dex, at, class_idx);
    poke_u32(dex, at + 4, ACC_PUBLIC);
    poke_u32(dex, at + 8, superclass_idx);
    poke_u32(dex, at + 12, 0);
    poke_u32(dex, at + 16, NO_INDEX);
    poke_u32(dex, at + 20, 0);
    poke_u32(dex, at + 24, class_data_off);
    poke_u32(dex, at + 28, 0);
}

fn poke_u32(buf: &mut [u8], at: usize, value: u32) {
    buf[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn align4(value: usize) -> usize {
    (value + 3) & !3
}

fn uleb128(mut value: u32) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::activity_dex;
    use crate::hash::{adler32, sha1};

    #[test]
    fn header_checksum_matches_the_activity_class() {
        let dex = activity_dex();
        assert_eq!(&dex[..8], b"dex\n035\0");
        assert_eq!(
            u32::from_le_bytes(dex[8..12].try_into().unwrap()),
            adler32(&dex[12..])
        );
        assert_eq!(&dex[12..32], &sha1(&dex[32..]));
        assert_eq!(
            u32::from_le_bytes(dex[0x20..0x24].try_into().unwrap()) as usize,
            dex.len()
        );
        let data_size = u32::from_le_bytes(dex[0x68..0x6c].try_into().unwrap());
        assert_eq!(data_size % 4, 0, "data_size must be a multiple of 4");
        let descriptor = b"Ldev/etherquic/play/MainActivity;";
        assert!(dex
            .windows(descriptor.len())
            .any(|window| window == descriptor));
    }

    #[test]
    fn ids_are_sorted_and_the_map_types_increase() {
        let dex = activity_dex();
        let report = inspect(&dex);
        assert_eq!(
            report.strings,
            vec![
                "<init>".to_string(),
                "Landroid/app/Activity;".to_string(),
                "Ldev/etherquic/play/MainActivity;".to_string(),
                "V".to_string(),
            ]
        );
        assert!(
            report.strings.windows(2).all(|pair| pair[0] < pair[1]),
            "string_ids are out of order: {:?}",
            report.strings
        );
        assert_eq!(
            report.type_descriptors,
            vec![
                "Landroid/app/Activity;",
                "Ldev/etherquic/play/MainActivity;",
                "V",
            ]
        );
        assert!(
            report
                .type_descriptors
                .windows(2)
                .all(|pair| pair[0] < pair[1]),
            "type_ids are out of order"
        );
        assert!(
            report.methods.windows(2).all(|pair| pair[0] < pair[1]),
            "method_ids are out of order: {:?}",
            report.methods
        );
        assert_eq!(
            report.map_types,
            vec![0x0000, 0x0001, 0x0002, 0x0003, 0x0005, 0x0006, 0x1000, 0x2000, 0x2001, 0x2002,]
        );
        assert!(
            report.map_offsets.windows(2).all(|pair| pair[0] < pair[1]),
            "map offsets are not strictly increasing: {:?}",
            report.map_offsets
        );
        assert_eq!(report.invoke_method, 0, "invoke-direct target");
        assert_eq!(report.direct_method, 1, "class_data method");
        let invoke = &report.methods[report.invoke_method];
        assert_eq!(invoke.1, "Landroid/app/Activity;");
        assert_eq!(invoke.2, "<init>");
        let direct = &report.methods[report.direct_method];
        assert_eq!(direct.1, "Ldev/etherquic/play/MainActivity;");
        assert_eq!(direct.2, "<init>");
    }

    struct Inspected {
        strings: Vec<String>,
        type_descriptors: Vec<String>,
        methods: Vec<(u16, String, String, u16)>,
        map_types: Vec<u16>,
        map_offsets: Vec<u32>,
        invoke_method: usize,
        direct_method: usize,
    }

    fn inspect(dex: &[u8]) -> Inspected {
        let string_ids_size = u32_at(dex, 0x38) as usize;
        let string_ids_off = u32_at(dex, 0x3c) as usize;
        let type_ids_size = u32_at(dex, 0x40) as usize;
        let type_ids_off = u32_at(dex, 0x44) as usize;
        let method_ids_size = u32_at(dex, 0x58) as usize;
        let method_ids_off = u32_at(dex, 0x5c) as usize;
        let strings: Vec<String> = (0..string_ids_size)
            .map(|index| read_string(dex, string_ids_off, index))
            .collect();
        let type_descriptors: Vec<String> = (0..type_ids_size)
            .map(|index| {
                let descriptor = u32_at(dex, type_ids_off + index * 4) as usize;
                strings[descriptor].clone()
            })
            .collect();
        let methods: Vec<(u16, String, String, u16)> = (0..method_ids_size)
            .map(|index| {
                let at = method_ids_off + index * 8;
                let class_idx = u16_at(dex, at);
                let proto_idx = u16_at(dex, at + 2);
                let name_idx = u32_at(dex, at + 4) as usize;
                let class_name = type_descriptors[class_idx as usize].clone();
                (class_idx, class_name, strings[name_idx].clone(), proto_idx)
            })
            .collect();
        let map_off = u32_at(dex, 0x34) as usize;
        let map_count = u32_at(dex, map_off) as usize;
        let mut map_types = Vec::new();
        let mut map_offsets = Vec::new();
        let mut class_data_off = 0usize;
        let mut code_off = 0usize;
        for index in 0..map_count {
            let at = map_off + 4 + index * 12;
            let kind = u16_at(dex, at);
            let offset = u32_at(dex, at + 8) as usize;
            map_types.push(kind);
            map_offsets.push(offset as u32);
            if kind == 0x2000 {
                class_data_off = offset;
            }
            if kind == 0x2001 {
                code_off = offset;
            }
        }
        let invoke_method = u16_at(dex, code_off + 16 + 2) as usize;
        let mut cursor = class_data_off;
        for _ in 0..4 {
            let (_, next) = read_uleb(dex, cursor);
            cursor = next;
        }
        let (direct_method, _) = read_uleb(dex, cursor);
        Inspected {
            strings,
            type_descriptors,
            methods,
            map_types,
            map_offsets,
            invoke_method,
            direct_method: direct_method as usize,
        }
    }

    fn read_string(dex: &[u8], string_ids_off: usize, index: usize) -> String {
        let data_off = u32_at(dex, string_ids_off + index * 4) as usize;
        let (_len, start) = read_uleb(dex, data_off);
        let end = dex[start..]
            .iter()
            .position(|byte| *byte == 0)
            .map(|at| start + at)
            .unwrap_or(dex.len());
        String::from_utf8(dex[start..end].to_vec()).expect("dex string")
    }

    fn read_uleb(dex: &[u8], mut at: usize) -> (u32, usize) {
        let mut value = 0u32;
        let mut shift = 0;
        loop {
            let byte = dex[at];
            at += 1;
            value |= u32::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return (value, at);
            }
            shift += 7;
        }
    }

    fn u16_at(dex: &[u8], at: usize) -> u16 {
        u16::from_le_bytes(dex[at..at + 2].try_into().unwrap())
    }

    fn u32_at(dex: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(dex[at..at + 4].try_into().unwrap())
    }
}
