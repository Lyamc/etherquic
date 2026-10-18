//! `classes.dex` for the Play activity.
//!
//! `dev.etherquic.play.MainActivity` extends `android.app.Activity`, implements
//! `View.OnClickListener`, and `onCreate` builds a paste field, a Join button,
//! and a status line. String, type, field, and method ids are in the order ART
//! requires.

use crate::hash::{adler32, sha1};

const NO_INDEX: u32 = 0xffff_ffff;
const ACC_PUBLIC: u32 = 0x1;
const ACC_CONSTRUCTOR: u32 = 0x0001_0000;

const MAP_HEADER: u16 = 0x0000;
const MAP_STRING_ID: u16 = 0x0001;
const MAP_TYPE_ID: u16 = 0x0002;
const MAP_PROTO_ID: u16 = 0x0003;
const MAP_FIELD_ID: u16 = 0x0004;
const MAP_METHOD_ID: u16 = 0x0005;
const MAP_CLASS_DEF: u16 = 0x0006;
const MAP_MAP_LIST: u16 = 0x1000;
const MAP_TYPE_LIST: u16 = 0x1001;
const MAP_CLASS_DATA: u16 = 0x2000;
const MAP_CODE: u16 = 0x2001;
const MAP_STRING_DATA: u16 = 0x2002;

const OP_MOVE_RESULT: u8 = 0x0a;
const OP_MOVE_RESULT_OBJECT: u8 = 0x0c;
const OP_RETURN_VOID: u8 = 0x0e;
const OP_CONST4: u8 = 0x12;
const OP_CONST16: u8 = 0x13;
const OP_CONST32: u8 = 0x14;
const OP_CONST_STRING: u8 = 0x1a;
const OP_NEW_INSTANCE: u8 = 0x22;
const OP_MOVE_OBJECT: u8 = 0x07;
const OP_IF_EQZ: u8 = 0x38;
const OP_IGET_OBJECT: u8 = 0x54;
const OP_IPUT_OBJECT: u8 = 0x5b;
const OP_INVOKE_VIRTUAL: u8 = 0x6e;
const OP_INVOKE_SUPER: u8 = 0x6f;
const OP_INVOKE_DIRECT: u8 = 0x70;

const ACTIVITY: &str = "Landroid/app/Activity;";
const CONTEXT: &str = "Landroid/content/Context;";
const BUNDLE: &str = "Landroid/os/Bundle;";
const VIEW: &str = "Landroid/view/View;";
const LISTENER: &str = "Landroid/view/View$OnClickListener;";
const VIEW_GROUP: &str = "Landroid/view/ViewGroup;";
const BUTTON: &str = "Landroid/widget/Button;";
const EDIT_TEXT: &str = "Landroid/widget/EditText;";
const LINEAR: &str = "Landroid/widget/LinearLayout;";
const TEXT_VIEW: &str = "Landroid/widget/TextView;";
const MAIN: &str = "Ldev/etherquic/play/MainActivity;";
const CHAR_SEQ: &str = "Ljava/lang/CharSequence;";
const OBJECT: &str = "Ljava/lang/Object;";
const STRING: &str = "Ljava/lang/String;";

const HINT: &str = "Paste the host card from EtherQuic Host, then tap Join.";
const CARD_HINT: &str = "etherquic host card";
const JOIN: &str = "Join";
const STATUS: &str = "UDP player. Ethernet is not used.";
const NEED_CARD: &str = "Paste a host card first.";
const BAD_CARD: &str = "That text is not an EtherQuic host card.";
const GOOD_CARD: &str = "Host card looks valid.";
const HEADER: &str = "etherquic";

pub fn activity_dex() -> Vec<u8> {
    assemble()
}

fn assemble() -> Vec<u8> {
    let mut strings: Vec<String> = Vec::new();
    intern_all(&mut strings);
    strings.sort();
    debug_assert!(strings.windows(2).all(|pair| pair[0] < pair[1]));

    let mut types: Vec<String> = TYPE_DESCS.iter().map(|s| s.to_string()).collect();
    types.sort();
    debug_assert!(types.windows(2).all(|pair| pair[0] < pair[1]));

    let string_of = |value: &str| -> u32 {
        strings
            .iter()
            .position(|item| item == value)
            .expect(value) as u32
    };
    let type_of = |desc: &str| -> u16 {
        types
            .iter()
            .position(|item| item == desc)
            .expect(desc) as u16
    };
    let type_string_ids: Vec<u32> = types.iter().map(|desc| string_of(desc)).collect();
    debug_assert!(type_string_ids.windows(2).all(|pair| pair[0] < pair[1]));

    let mut protos: Vec<Proto> = PROTO_SHAPES
        .iter()
        .map(|shape| Proto {
            shorty: shorty(shape.ret, shape.params),
            ret: type_of(shape.ret),
            params: shape.params.iter().map(|param| type_of(param)).collect(),
        })
        .collect();
    protos.sort_by(|left, right| {
        left.ret
            .cmp(&right.ret)
            .then(left.params.iter().cmp(right.params.iter()))
    });
    protos.dedup();

    let proto_of = |ret: &str, params: &[&str]| -> u16 {
        let want_ret = type_of(ret);
        let want_params: Vec<u16> = params.iter().map(|param| type_of(param)).collect();
        protos
            .iter()
            .position(|proto| proto.ret == want_ret && proto.params == want_params)
            .expect("proto") as u16
    };

    let mut fields: Vec<Field> = FIELD_SHAPES
        .iter()
        .map(|shape| Field {
            class: type_of(shape.class),
            name: string_of(shape.name),
            ty: type_of(shape.ty),
        })
        .collect();
    fields.sort_by(|left, right| {
        left.class
            .cmp(&right.class)
            .then(left.name.cmp(&right.name))
            .then(left.ty.cmp(&right.ty))
    });
    let field_of = |class: &str, name: &str| -> u16 {
        let class_idx = type_of(class);
        let name_idx = string_of(name);
        fields
            .iter()
            .position(|field| field.class == class_idx && field.name == name_idx)
            .expect(name) as u16
    };

    let mut methods: Vec<Method> = METHOD_SHAPES
        .iter()
        .map(|shape| Method {
            class: type_of(shape.class),
            proto: proto_of(shape.ret, shape.params),
            name: string_of(shape.name),
        })
        .collect();
    methods.sort_by(|left, right| {
        left.class
            .cmp(&right.class)
            .then(left.name.cmp(&right.name))
            .then(left.proto.cmp(&right.proto))
    });
    methods.dedup();
    let method_of = |class: &str, name: &str, ret: &str, params: &[&str]| -> u16 {
        let class_idx = type_of(class);
        let name_idx = string_of(name);
        let proto_idx = proto_of(ret, params);
        methods
            .iter()
            .position(|method| {
                method.class == class_idx && method.name == name_idx && method.proto == proto_idx
            })
            .unwrap_or_else(|| panic!("missing method {class}.{name}")) as u16
    };

    let ids = Ids {
        string_of: &string_of,
        type_of: &type_of,
        field_of: &field_of,
        method_of: &method_of,
    };

    let init_insns = encode(&init_ops(), &ids);
    let create_insns = encode(&on_create_ops(), &ids);
    let click_insns = encode(&on_click_ops(), &ids);
    let init_code = code_item(1, 1, 1, &init_insns);
    let create_code = code_item(8, 2, 5, &create_insns);
    let click_code = code_item(5, 2, 2, &click_insns);

    let mut type_lists = Vec::new();
    let mut interned_lists: Vec<(Vec<u16>, usize)> = Vec::new();
    let mut list_off = |params: &[u16]| -> u32 {
        if params.is_empty() {
            return 0;
        }
        if let Some((_, at)) = interned_lists.iter().find(|(item, _)| item == params) {
            return *at as u32;
        }
        let at = type_lists.len();
        write_type_list(&mut type_lists, params);
        interned_lists.push((params.to_vec(), at));
        at as u32
    };
    let proto_params_rel: Vec<u32> = protos.iter().map(|proto| list_off(&proto.params)).collect();
    let interfaces_rel = list_off(&[type_of(LISTENER)]);

    let mut string_data = Vec::new();
    let mut string_at = Vec::new();
    for value in &strings {
        string_at.push(string_data.len());
        string_data.extend(uleb128(value.len() as u32));
        string_data.extend_from_slice(value.as_bytes());
        string_data.push(0);
    }

    let string_ids_size = strings.len() as u32;
    let type_ids_size = types.len() as u32;
    let proto_ids_size = protos.len() as u32;
    let field_ids_size = fields.len() as u32;
    let method_ids_size = methods.len() as u32;
    let class_defs_size = 1u32;

    let string_ids_off = 0x70usize;
    let type_ids_off = string_ids_off + string_ids_size as usize * 4;
    let proto_ids_off = type_ids_off + type_ids_size as usize * 4;
    let field_ids_off = proto_ids_off + proto_ids_size as usize * 12;
    let method_ids_off = field_ids_off + field_ids_size as usize * 8;
    let class_defs_off = method_ids_off + method_ids_size as usize * 8;
    let map_items = 12usize;
    let map_len = 4 + map_items * 12;
    let map_off = align4(class_defs_off + class_defs_size as usize * 32);
    let type_list_off = align4(map_off + map_len);

    let init_idx = method_of(MAIN, "<init>", "V", &[]);
    let create_idx = method_of(MAIN, "onCreate", "V", &[BUNDLE]);
    let click_idx = method_of(MAIN, "onClick", "V", &[VIEW]);
    let card_idx = field_of(MAIN, "card");
    let status_idx = field_of(MAIN, "status");

    let mut class_data_off = align4(type_list_off + type_lists.len());
    let mut init_off = 0usize;
    let mut create_off = 0usize;
    let mut click_off = 0usize;
    let mut class_data = Vec::new();
    for _ in 0..6 {
        let data = class_data_bytes(
            &[(card_idx as u32, 0), (status_idx as u32, 0)],
            &[(init_idx as u32, ACC_PUBLIC | ACC_CONSTRUCTOR, init_off as u32)],
            &[
                (click_idx as u32, ACC_PUBLIC, click_off as u32),
                (create_idx as u32, ACC_PUBLIC, create_off as u32),
            ],
        );
        let next_class = align4(type_list_off + type_lists.len());
        let next_init = align4(next_class + data.len());
        let next_create = align4(next_init + init_code.len());
        let next_click = align4(next_create + create_code.len());
        if next_class == class_data_off
            && next_init == init_off
            && next_create == create_off
            && next_click == click_off
        {
            class_data = data;
            break;
        }
        class_data_off = next_class;
        init_off = next_init;
        create_off = next_create;
        click_off = next_click;
        class_data = data;
    }
    debug_assert_eq!(align4(class_data_off + class_data.len()), init_off);

    let string_data_off = align4(click_off + click_code.len());
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
    poke_u32(&mut dex, 0x50, field_ids_size);
    poke_u32(&mut dex, 0x54, field_ids_off as u32);
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
    for (index, desc) in types.iter().enumerate() {
        poke_u32(&mut dex, type_ids_off + index * 4, string_of(desc));
    }
    for (index, proto) in protos.iter().enumerate() {
        let at = proto_ids_off + index * 12;
        poke_u32(&mut dex, at, string_of(&proto.shorty));
        poke_u32(&mut dex, at + 4, proto.ret as u32);
        // Offset 0 in the type_list blob is a real list. Only empty parameter
        // lists use parameters_off = 0.
        let parameters_off = if protos[index].params.is_empty() {
            0
        } else {
            type_list_off as u32 + proto_params_rel[index]
        };
        poke_u32(&mut dex, at + 8, parameters_off);
    }
    for (index, field) in fields.iter().enumerate() {
        let at = field_ids_off + index * 8;
        dex[at..at + 2].copy_from_slice(&(field.class).to_le_bytes());
        dex[at + 2..at + 4].copy_from_slice(&(field.ty).to_le_bytes());
        poke_u32(&mut dex, at + 4, field.name);
    }
    for (index, method) in methods.iter().enumerate() {
        write_method(
            &mut dex,
            method_ids_off + index * 8,
            method.class,
            method.proto,
            method.name,
        );
    }
    write_class_def(
        &mut dex,
        class_defs_off,
        type_of(MAIN) as u32,
        type_of(ACTIVITY) as u32,
        type_list_off as u32 + interfaces_rel,
        class_data_off as u32,
    );

    let map = [
        (MAP_HEADER, 1u32, 0u32),
        (MAP_STRING_ID, string_ids_size, string_ids_off as u32),
        (MAP_TYPE_ID, type_ids_size, type_ids_off as u32),
        (MAP_PROTO_ID, proto_ids_size, proto_ids_off as u32),
        (MAP_FIELD_ID, field_ids_size, field_ids_off as u32),
        (MAP_METHOD_ID, method_ids_size, method_ids_off as u32),
        (MAP_CLASS_DEF, class_defs_size, class_defs_off as u32),
        (MAP_MAP_LIST, 1, map_off as u32),
        (MAP_TYPE_LIST, interned_lists.len() as u32, type_list_off as u32),
        (MAP_CLASS_DATA, 1, class_data_off as u32),
        (MAP_CODE, 3, init_off as u32),
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

    dex[type_list_off..type_list_off + type_lists.len()].copy_from_slice(&type_lists);
    dex[class_data_off..class_data_off + class_data.len()].copy_from_slice(&class_data);
    dex[init_off..init_off + init_code.len()].copy_from_slice(&init_code);
    dex[create_off..create_off + create_code.len()].copy_from_slice(&create_code);
    dex[click_off..click_off + click_code.len()].copy_from_slice(&click_code);
    dex[string_data_off..string_data_off + string_data.len()].copy_from_slice(&string_data);

    let signature = sha1(&dex[32..]);
    dex[12..32].copy_from_slice(&signature);
    let checksum = adler32(&dex[12..]);
    poke_u32(&mut dex, 8, checksum);
    dex
}

const TYPE_DESCS: &[&str] = &[
    ACTIVITY, CONTEXT, BUNDLE, VIEW, LISTENER, VIEW_GROUP, BUTTON, EDIT_TEXT, LINEAR, TEXT_VIEW,
    MAIN, CHAR_SEQ, OBJECT, STRING, "I", "V", "Z",
];

struct Shape {
    class: &'static str,
    name: &'static str,
    ret: &'static str,
    params: &'static [&'static str],
}

struct FieldShape {
    class: &'static str,
    name: &'static str,
    ty: &'static str,
}

struct ProtoShape {
    ret: &'static str,
    params: &'static [&'static str],
}

const PROTO_SHAPES: &[ProtoShape] = &[
    ProtoShape { ret: "V", params: &[] },
    ProtoShape { ret: "V", params: &[BUNDLE] },
    ProtoShape { ret: "V", params: &[CONTEXT] },
    ProtoShape { ret: "V", params: &[VIEW] },
    ProtoShape { ret: "V", params: &[CHAR_SEQ] },
    ProtoShape { ret: "V", params: &["I"] },
    ProtoShape { ret: "V", params: &["I", "I", "I", "I"] },
    ProtoShape { ret: "V", params: &[LISTENER] },
    ProtoShape { ret: CHAR_SEQ, params: &[] },
    ProtoShape { ret: STRING, params: &[] },
    ProtoShape { ret: "Z", params: &[] },
    ProtoShape { ret: "Z", params: &[STRING] },
];

const FIELD_SHAPES: &[FieldShape] = &[
    FieldShape {
        class: MAIN,
        name: "card",
        ty: EDIT_TEXT,
    },
    FieldShape {
        class: MAIN,
        name: "status",
        ty: TEXT_VIEW,
    },
];

const METHOD_SHAPES: &[Shape] = &[
    Shape { class: ACTIVITY, name: "<init>", ret: "V", params: &[] },
    Shape { class: ACTIVITY, name: "onCreate", ret: "V", params: &[BUNDLE] },
    Shape { class: ACTIVITY, name: "setContentView", ret: "V", params: &[VIEW] },
    Shape { class: MAIN, name: "<init>", ret: "V", params: &[] },
    Shape { class: MAIN, name: "onCreate", ret: "V", params: &[BUNDLE] },
    Shape { class: MAIN, name: "onClick", ret: "V", params: &[VIEW] },
    Shape { class: LINEAR, name: "<init>", ret: "V", params: &[CONTEXT] },
    Shape { class: LINEAR, name: "setOrientation", ret: "V", params: &["I"] },
    Shape { class: VIEW_GROUP, name: "addView", ret: "V", params: &[VIEW] },
    Shape { class: VIEW, name: "setPadding", ret: "V", params: &["I", "I", "I", "I"] },
    Shape { class: VIEW, name: "setOnClickListener", ret: "V", params: &[LISTENER] },
    Shape { class: TEXT_VIEW, name: "<init>", ret: "V", params: &[CONTEXT] },
    Shape { class: TEXT_VIEW, name: "setText", ret: "V", params: &[CHAR_SEQ] },
    Shape { class: TEXT_VIEW, name: "setHint", ret: "V", params: &[CHAR_SEQ] },
    Shape { class: TEXT_VIEW, name: "setMinLines", ret: "V", params: &["I"] },
    Shape { class: TEXT_VIEW, name: "setGravity", ret: "V", params: &["I"] },
    Shape { class: TEXT_VIEW, name: "setInputType", ret: "V", params: &["I"] },
    Shape { class: TEXT_VIEW, name: "setTextColor", ret: "V", params: &["I"] },
    Shape { class: TEXT_VIEW, name: "setHintTextColor", ret: "V", params: &["I"] },
    Shape { class: TEXT_VIEW, name: "getText", ret: CHAR_SEQ, params: &[] },
    Shape { class: EDIT_TEXT, name: "<init>", ret: "V", params: &[CONTEXT] },
    Shape { class: BUTTON, name: "<init>", ret: "V", params: &[CONTEXT] },
    Shape { class: OBJECT, name: "toString", ret: STRING, params: &[] },
    Shape { class: STRING, name: "trim", ret: STRING, params: &[] },
    Shape { class: STRING, name: "isEmpty", ret: "Z", params: &[] },
    Shape { class: STRING, name: "startsWith", ret: "Z", params: &[STRING] },
];

#[derive(Clone, PartialEq, Eq)]
struct Proto {
    shorty: String,
    ret: u16,
    params: Vec<u16>,
}

#[derive(Clone)]
struct Field {
    class: u16,
    name: u32,
    ty: u16,
}

#[derive(Clone, PartialEq, Eq)]
struct Method {
    class: u16,
    proto: u16,
    name: u32,
}

struct Ids<'a> {
    string_of: &'a dyn Fn(&str) -> u32,
    type_of: &'a dyn Fn(&str) -> u16,
    field_of: &'a dyn Fn(&str, &str) -> u16,
    method_of: &'a dyn Fn(&str, &str, &str, &[&str]) -> u16,
}

enum Op {
    ReturnVoid,
    MoveObject { dest: u8, src: u8 },
    Const4 { dest: u8, value: i8 },
    Const16 { dest: u8, value: i16 },
    Const32 { dest: u8, value: u32 },
    ConstString { dest: u8, value: &'static str },
    New { dest: u8, ty: &'static str },
    Invoke { kind: u8, class: &'static str, name: &'static str, ret: &'static str, params: &'static [&'static str], args: &'static [u8] },
    MoveResultObject { dest: u8 },
    MoveResult { dest: u8 },
    IgetObject { dest: u8, obj: u8, name: &'static str },
    IputObject { src: u8, obj: u8, name: &'static str },
    IfEqz { reg: u8, label: &'static str },
    Label(&'static str),
}

fn init_ops() -> Vec<Op> {
    vec![
        invoke(OP_INVOKE_DIRECT, ACTIVITY, "<init>", "V", &[], &[0]),
        Op::ReturnVoid,
    ]
}

fn on_create_ops() -> Vec<Op> {
    // registers=8, ins=2 → this=v6, bundle=v7. v0=this, v1=root, v3=child, v2/v4 temps.
    vec![
        Op::MoveObject { dest: 0, src: 6 },
        invoke(OP_INVOKE_SUPER, ACTIVITY, "onCreate", "V", &[BUNDLE], &[0, 7]),
        Op::New { dest: 1, ty: LINEAR },
        invoke(OP_INVOKE_DIRECT, LINEAR, "<init>", "V", &[CONTEXT], &[1, 0]),
        Op::Const4 { dest: 2, value: 1 },
        invoke(OP_INVOKE_VIRTUAL, LINEAR, "setOrientation", "V", &["I"], &[1, 2]),
        Op::Const16 { dest: 2, value: 48 },
        invoke(OP_INVOKE_VIRTUAL, VIEW, "setPadding", "V", &["I", "I", "I", "I"], &[1, 2, 2, 2, 2]),
        Op::New { dest: 3, ty: TEXT_VIEW },
        invoke(OP_INVOKE_DIRECT, TEXT_VIEW, "<init>", "V", &[CONTEXT], &[3, 0]),
        Op::ConstString { dest: 4, value: HINT },
        invoke(OP_INVOKE_VIRTUAL, TEXT_VIEW, "setText", "V", &[CHAR_SEQ], &[3, 4]),
        Op::Const32 { dest: 4, value: 0xff22_2222 },
        invoke(OP_INVOKE_VIRTUAL, TEXT_VIEW, "setTextColor", "V", &["I"], &[3, 4]),
        invoke(OP_INVOKE_VIRTUAL, VIEW_GROUP, "addView", "V", &[VIEW], &[1, 3]),
        Op::New { dest: 3, ty: EDIT_TEXT },
        invoke(OP_INVOKE_DIRECT, EDIT_TEXT, "<init>", "V", &[CONTEXT], &[3, 0]),
        Op::ConstString { dest: 4, value: CARD_HINT },
        invoke(OP_INVOKE_VIRTUAL, TEXT_VIEW, "setHint", "V", &[CHAR_SEQ], &[3, 4]),
        Op::Const32 { dest: 4, value: 0xff88_8888 },
        invoke(OP_INVOKE_VIRTUAL, TEXT_VIEW, "setHintTextColor", "V", &["I"], &[3, 4]),
        Op::Const32 { dest: 4, value: 0xff22_2222 },
        invoke(OP_INVOKE_VIRTUAL, TEXT_VIEW, "setTextColor", "V", &["I"], &[3, 4]),
        Op::Const4 { dest: 4, value: 6 },
        invoke(OP_INVOKE_VIRTUAL, TEXT_VIEW, "setMinLines", "V", &["I"], &[3, 4]),
        Op::Const16 { dest: 4, value: 0x30 },
        invoke(OP_INVOKE_VIRTUAL, TEXT_VIEW, "setGravity", "V", &["I"], &[3, 4]),
        Op::Const32 { dest: 4, value: 0x0002_0001 },
        invoke(OP_INVOKE_VIRTUAL, TEXT_VIEW, "setInputType", "V", &["I"], &[3, 4]),
        Op::IputObject { src: 3, obj: 0, name: "card" },
        invoke(OP_INVOKE_VIRTUAL, VIEW_GROUP, "addView", "V", &[VIEW], &[1, 3]),
        Op::New { dest: 3, ty: BUTTON },
        invoke(OP_INVOKE_DIRECT, BUTTON, "<init>", "V", &[CONTEXT], &[3, 0]),
        Op::ConstString { dest: 4, value: JOIN },
        invoke(OP_INVOKE_VIRTUAL, TEXT_VIEW, "setText", "V", &[CHAR_SEQ], &[3, 4]),
        invoke(OP_INVOKE_VIRTUAL, VIEW, "setOnClickListener", "V", &[LISTENER], &[3, 0]),
        invoke(OP_INVOKE_VIRTUAL, VIEW_GROUP, "addView", "V", &[VIEW], &[1, 3]),
        Op::New { dest: 3, ty: TEXT_VIEW },
        invoke(OP_INVOKE_DIRECT, TEXT_VIEW, "<init>", "V", &[CONTEXT], &[3, 0]),
        Op::ConstString { dest: 4, value: STATUS },
        invoke(OP_INVOKE_VIRTUAL, TEXT_VIEW, "setText", "V", &[CHAR_SEQ], &[3, 4]),
        Op::Const32 { dest: 4, value: 0xff22_2222 },
        invoke(OP_INVOKE_VIRTUAL, TEXT_VIEW, "setTextColor", "V", &["I"], &[3, 4]),
        Op::IputObject { src: 3, obj: 0, name: "status" },
        invoke(OP_INVOKE_VIRTUAL, VIEW_GROUP, "addView", "V", &[VIEW], &[1, 3]),
        invoke(OP_INVOKE_VIRTUAL, ACTIVITY, "setContentView", "V", &[VIEW], &[0, 1]),
        Op::ReturnVoid,
    ]
}

fn on_click_ops() -> Vec<Op> {
    // registers=5, ins=2 → this=v3, view=v4.
    vec![
        Op::IgetObject { dest: 0, obj: 3, name: "card" },
        invoke(OP_INVOKE_VIRTUAL, TEXT_VIEW, "getText", CHAR_SEQ, &[], &[0]),
        Op::MoveResultObject { dest: 0 },
        invoke(OP_INVOKE_VIRTUAL, OBJECT, "toString", STRING, &[], &[0]),
        Op::MoveResultObject { dest: 0 },
        invoke(OP_INVOKE_VIRTUAL, STRING, "trim", STRING, &[], &[0]),
        Op::MoveResultObject { dest: 0 },
        invoke(OP_INVOKE_VIRTUAL, STRING, "isEmpty", "Z", &[], &[0]),
        Op::MoveResult { dest: 1 },
        Op::IfEqz { reg: 1, label: "has_text" },
        Op::IgetObject { dest: 0, obj: 3, name: "status" },
        Op::ConstString { dest: 1, value: NEED_CARD },
        invoke(OP_INVOKE_VIRTUAL, TEXT_VIEW, "setText", "V", &[CHAR_SEQ], &[0, 1]),
        Op::ReturnVoid,
        Op::Label("has_text"),
        Op::ConstString { dest: 1, value: HEADER },
        invoke(OP_INVOKE_VIRTUAL, STRING, "startsWith", "Z", &[STRING], &[0, 1]),
        Op::MoveResult { dest: 1 },
        Op::IfEqz { reg: 1, label: "bad" },
        Op::IgetObject { dest: 1, obj: 3, name: "status" },
        Op::ConstString { dest: 2, value: GOOD_CARD },
        invoke(OP_INVOKE_VIRTUAL, TEXT_VIEW, "setText", "V", &[CHAR_SEQ], &[1, 2]),
        Op::ReturnVoid,
        Op::Label("bad"),
        Op::IgetObject { dest: 1, obj: 3, name: "status" },
        Op::ConstString { dest: 2, value: BAD_CARD },
        invoke(OP_INVOKE_VIRTUAL, TEXT_VIEW, "setText", "V", &[CHAR_SEQ], &[1, 2]),
        Op::ReturnVoid,
    ]
}

fn invoke(
    kind: u8,
    class: &'static str,
    name: &'static str,
    ret: &'static str,
    params: &'static [&'static str],
    args: &'static [u8],
) -> Op {
    Op::Invoke {
        kind,
        class,
        name,
        ret,
        params,
        args,
    }
}

fn intern_all(strings: &mut Vec<String>) {
    intern(strings, "<init>");
    intern(strings, "onCreate");
    intern(strings, "onClick");
    intern(strings, "setContentView");
    intern(strings, "setOrientation");
    intern(strings, "addView");
    intern(strings, "setPadding");
    intern(strings, "setOnClickListener");
    intern(strings, "setText");
    intern(strings, "setHint");
    intern(strings, "setMinLines");
    intern(strings, "setGravity");
    intern(strings, "setInputType");
    intern(strings, "setTextColor");
    intern(strings, "setHintTextColor");
    intern(strings, "getText");
    intern(strings, "toString");
    intern(strings, "trim");
    intern(strings, "isEmpty");
    intern(strings, "startsWith");
    intern(strings, "card");
    intern(strings, "status");
    intern(strings, HINT);
    intern(strings, CARD_HINT);
    intern(strings, JOIN);
    intern(strings, STATUS);
    intern(strings, NEED_CARD);
    intern(strings, BAD_CARD);
    intern(strings, GOOD_CARD);
    intern(strings, HEADER);
    intern(strings, "I");
    intern(strings, "V");
    intern(strings, "Z");
    intern(strings, "L");
    intern(strings, "VL");
    intern(strings, "VI");
    intern(strings, "VIIII");
    intern(strings, "ZL");
    for desc in TYPE_DESCS {
        intern(strings, desc);
    }
}

fn intern(strings: &mut Vec<String>, value: &str) {
    if !strings.iter().any(|item| item == value) {
        strings.push(value.to_string());
    }
}

fn shorty(ret: &str, params: &[&str]) -> String {
    let mut out = String::new();
    out.push(shorty_char(ret));
    for param in params {
        out.push(shorty_char(param));
    }
    out
}

fn shorty_char(desc: &str) -> char {
    match desc {
        "V" => 'V',
        "Z" => 'Z',
        "B" => 'B',
        "S" => 'S',
        "C" => 'C',
        "I" => 'I',
        "J" => 'J',
        "F" => 'F',
        "D" => 'D',
        _ => 'L',
    }
}

fn encode(ops: &[Op], ids: &Ids<'_>) -> Vec<u16> {
    let mut units: Vec<Option<u16>> = Vec::new();
    let mut labels = Vec::new();
    let mut patches: Vec<(usize, &'static str)> = Vec::new();
    for op in ops {
        match op {
            Op::Label(name) => labels.push((*name, units.len())),
            Op::ReturnVoid => units.push(Some(OP_RETURN_VOID as u16)),
            Op::MoveObject { dest, src } => {
                units.push(Some(
                    OP_MOVE_OBJECT as u16 | u16::from(*dest) << 8 | u16::from(*src) << 12,
                ));
            }
            Op::Const4 { dest, value } => {
                units.push(Some(
                    OP_CONST4 as u16 | u16::from(*dest) << 8 | (*value as u16) << 12,
                ));
            }
            Op::Const16 { dest, value } => {
                units.push(Some(OP_CONST16 as u16 | u16::from(*dest) << 8));
                units.push(Some(*value as u16));
            }
            Op::Const32 { dest, value } => {
                units.push(Some(OP_CONST32 as u16 | u16::from(*dest) << 8));
                units.push(Some(*value as u16));
                units.push(Some((*value >> 16) as u16));
            }
            Op::ConstString { dest, value } => {
                units.push(Some(OP_CONST_STRING as u16 | u16::from(*dest) << 8));
                units.push(Some((ids.string_of)(value) as u16));
            }
            Op::New { dest, ty } => {
                units.push(Some(OP_NEW_INSTANCE as u16 | u16::from(*dest) << 8));
                units.push(Some((ids.type_of)(ty)));
            }
            Op::Invoke {
                kind,
                class,
                name,
                ret,
                params,
                args,
            } => {
                let method = (ids.method_of)(class, name, ret, params);
                let g = args.get(4).copied().unwrap_or(0);
                let c = args.get(0).copied().unwrap_or(0);
                let d = args.get(1).copied().unwrap_or(0);
                let e = args.get(2).copied().unwrap_or(0);
                let f = args.get(3).copied().unwrap_or(0);
                units.push(Some(
                    u16::from(*kind)
                        | u16::from(g) << 8
                        | (args.len() as u16) << 12,
                ));
                units.push(Some(method));
                units.push(Some(
                    u16::from(c)
                        | u16::from(d) << 4
                        | u16::from(e) << 8
                        | u16::from(f) << 12,
                ));
            }
            Op::MoveResultObject { dest } => {
                units.push(Some(OP_MOVE_RESULT_OBJECT as u16 | u16::from(*dest) << 8));
            }
            Op::MoveResult { dest } => {
                units.push(Some(OP_MOVE_RESULT as u16 | u16::from(*dest) << 8));
            }
            Op::IgetObject { dest, obj, name } => {
                units.push(Some(
                    OP_IGET_OBJECT as u16 | u16::from(*dest) << 8 | u16::from(*obj) << 12,
                ));
                units.push(Some((ids.field_of)(MAIN, name)));
            }
            Op::IputObject { src, obj, name } => {
                units.push(Some(
                    OP_IPUT_OBJECT as u16 | u16::from(*src) << 8 | u16::from(*obj) << 12,
                ));
                units.push(Some((ids.field_of)(MAIN, name)));
            }
            Op::IfEqz { reg, label } => {
                units.push(Some(OP_IF_EQZ as u16 | u16::from(*reg) << 8));
                patches.push((units.len(), *label));
                units.push(None);
            }
        }
    }
    let mut insns = vec![0u16; units.len()];
    for (index, unit) in units.iter().enumerate() {
        if let Some(value) = unit {
            insns[index] = *value;
        }
    }
    for (at, label) in patches {
        let target = labels
            .iter()
            .find(|(name, _)| *name == label)
            .unwrap_or_else(|| panic!("missing label {label}"))
            .1;
        let offset = target as i16 - (at as i16 - 1);
        insns[at] = offset as u16;
    }
    insns
}

fn code_item(registers: u16, ins: u16, outs: u16, insns: &[u16]) -> Vec<u8> {
    let mut item = Vec::new();
    item.extend_from_slice(&registers.to_le_bytes());
    item.extend_from_slice(&ins.to_le_bytes());
    item.extend_from_slice(&outs.to_le_bytes());
    item.extend_from_slice(&0u16.to_le_bytes());
    item.extend_from_slice(&0u32.to_le_bytes());
    item.extend_from_slice(&(insns.len() as u32).to_le_bytes());
    for insn in insns {
        item.extend_from_slice(&insn.to_le_bytes());
    }
    while item.len() % 4 != 0 {
        item.push(0);
    }
    item
}

fn write_type_list(buf: &mut Vec<u8>, params: &[u16]) {
    let start = buf.len();
    buf.extend_from_slice(&(params.len() as u32).to_le_bytes());
    for param in params {
        buf.extend_from_slice(&param.to_le_bytes());
    }
    while (buf.len() - start) % 4 != 0 {
        buf.push(0);
    }
}

fn class_data_bytes(
    fields: &[(u32, u32)],
    directs: &[(u32, u32, u32)],
    virtuals: &[(u32, u32, u32)],
) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend(uleb128(0));
    data.extend(uleb128(fields.len() as u32));
    data.extend(uleb128(directs.len() as u32));
    data.extend(uleb128(virtuals.len() as u32));
    let mut prev = 0u32;
    for (idx, access) in fields {
        data.extend(uleb128(idx - prev));
        prev = *idx;
        data.extend(uleb128(*access));
    }
    let mut prev = 0u32;
    for (idx, access, off) in directs {
        data.extend(uleb128(idx - prev));
        prev = *idx;
        data.extend(uleb128(*access));
        data.extend(uleb128(*off));
    }
    let mut prev = 0u32;
    for (idx, access, off) in virtuals {
        data.extend(uleb128(idx - prev));
        prev = *idx;
        data.extend(uleb128(*access));
        data.extend(uleb128(*off));
    }
    data
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
    interfaces_off: u32,
    class_data_off: u32,
) {
    poke_u32(dex, at, class_idx);
    poke_u32(dex, at + 4, ACC_PUBLIC);
    poke_u32(dex, at + 8, superclass_idx);
    poke_u32(dex, at + 12, interfaces_off);
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
        for needle in [b"onCreate".as_slice(), b"Join".as_slice(), b"onClick".as_slice()] {
            assert!(
                dex.windows(needle.len()).any(|window| window == needle),
                "missing {}",
                String::from_utf8_lossy(needle)
            );
        }
    }

    #[test]
    fn ids_are_sorted_and_the_map_types_increase() {
        let dex = activity_dex();
        let report = inspect(&dex);
        assert!(
            report.strings.windows(2).all(|pair| pair[0] < pair[1]),
            "string_ids are out of order: {:?}",
            report.strings
        );
        assert!(
            report
                .type_descriptors
                .windows(2)
                .all(|pair| pair[0] < pair[1]),
            "type_ids are out of order: {:?}",
            report.type_descriptors
        );
        assert!(
            report.methods.windows(2).all(|pair| pair[0] < pair[1]),
            "method_ids are out of order: {:?}",
            report.methods
        );
        assert!(
            report.map_types.windows(2).all(|pair| pair[0] < pair[1]),
            "map types are not strictly increasing: {:?}",
            report.map_types
        );
        assert!(
            report.map_offsets.windows(2).all(|pair| pair[0] < pair[1]),
            "map offsets are not strictly increasing: {:?}",
            report.map_offsets
        );
        assert_eq!(report.invoke_method_class, "Landroid/app/Activity;");
        assert_eq!(report.invoke_method_name, "<init>");
        assert_eq!(report.direct_class, "Ldev/etherquic/play/MainActivity;");
        assert_eq!(report.direct_name, "<init>");
        assert!(report.methods.iter().any(|method| method.2 == "onCreate"));
        assert!(report.methods.iter().any(|method| method.2 == "onClick"));
        for (shorty, param_count) in &report.protos {
            assert_eq!(
                shorty.len(),
                1 + *param_count,
                "shorty {shorty:?} does not match {param_count} parameters"
            );
        }
    }

    struct Inspected {
        strings: Vec<String>,
        type_descriptors: Vec<String>,
        methods: Vec<(u16, String, String, u16)>,
        protos: Vec<(String, usize)>,
        map_types: Vec<u16>,
        map_offsets: Vec<u32>,
        invoke_method_class: String,
        invoke_method_name: String,
        direct_class: String,
        direct_name: String,
    }

    fn inspect(dex: &[u8]) -> Inspected {
        let string_ids_size = u32_at(dex, 0x38) as usize;
        let string_ids_off = u32_at(dex, 0x3c) as usize;
        let type_ids_size = u32_at(dex, 0x40) as usize;
        let type_ids_off = u32_at(dex, 0x44) as usize;
        let proto_ids_size = u32_at(dex, 0x48) as usize;
        let proto_ids_off = u32_at(dex, 0x4c) as usize;
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
        let protos: Vec<(String, usize)> = (0..proto_ids_size)
            .map(|index| {
                let at = proto_ids_off + index * 12;
                let shorty = strings[u32_at(dex, at) as usize].clone();
                let parameters_off = u32_at(dex, at + 8) as usize;
                let param_count = if parameters_off == 0 {
                    0
                } else {
                    u32_at(dex, parameters_off) as usize
                };
                (shorty, param_count)
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
        let (static_fields, next) = read_uleb(dex, cursor);
        cursor = next;
        let (instance_fields, next) = read_uleb(dex, cursor);
        cursor = next;
        let (_directs, next) = read_uleb(dex, cursor);
        cursor = next;
        let (_virtuals, next) = read_uleb(dex, cursor);
        cursor = next;
        for _ in 0..static_fields {
            let (_, next) = read_uleb(dex, cursor);
            cursor = next;
            let (_, next) = read_uleb(dex, cursor);
            cursor = next;
        }
        for _ in 0..instance_fields {
            let (_, next) = read_uleb(dex, cursor);
            cursor = next;
            let (_, next) = read_uleb(dex, cursor);
            cursor = next;
        }
        let (direct_method, _) = read_uleb(dex, cursor);
        Inspected {
            strings,
            type_descriptors,
            methods: methods.clone(),
            protos,
            map_types,
            map_offsets,
            invoke_method_class: methods[invoke_method].1.clone(),
            invoke_method_name: methods[invoke_method].2.clone(),
            direct_class: methods[direct_method as usize].1.clone(),
            direct_name: methods[direct_method as usize].2.clone(),
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
