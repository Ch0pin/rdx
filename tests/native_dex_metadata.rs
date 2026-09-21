use rdx::native_dex::{self, DexValue};
use rdx::{engine::DecompilerEngine, native_engine::NativeDexEngine};
use std::sync::Arc;

fn put(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn uleb(bytes: &mut Vec<u8>, mut value: u32) {
    loop {
        let next = (value & 127) as u8;
        value >>= 7;
        bytes.push(next | if value != 0 { 128 } else { 0 });
        if value == 0 {
            break;
        }
    }
}
fn align(bytes: &mut Vec<u8>) {
    while !bytes.len().is_multiple_of(4) {
        bytes.push(0);
    }
}

/// Independent tiny DEX writer. IDs are resolved explicitly, no production encoder.
fn fixture(
    field_types: &[&str],
    encoded: &[u8],
    regions: &[(u32, u16, u16)],
    handlers: &[u8],
) -> Vec<u8> {
    let mut types = vec![
        "V",
        "Lsample/Metadata;",
        "Ljava/lang/Object;",
        "Ljava/lang/Exception;",
    ];
    for ty in field_types {
        if !types.contains(ty) {
            types.push(ty);
        }
    }
    let mut strings: Vec<String> = types.iter().map(|s| (*s).to_owned()).collect();
    let method_name = strings.len() as u32;
    strings.push("test".into());
    strings.push("value".into());
    let field_names: Vec<u32> = (0..field_types.len())
        .map(|i| {
            let n = strings.len() as u32;
            strings.push(format!("f{i}"));
            n
        })
        .collect();
    let strings_offset = 112;
    let types_offset = strings_offset + strings.len() * 4;
    let proto_offset = types_offset + types.len() * 4;
    let fields_offset = proto_offset + 12;
    let method_offset = fields_offset + field_types.len() * 8;
    let class_offset = method_offset + 8;
    let data_start = class_offset + 32;
    let mut bytes = vec![0u8; data_start];
    bytes[..8].copy_from_slice(b"dex\n039\0");
    put(&mut bytes, 36, 112);
    put(&mut bytes, 40, 0x12345678);
    for (header, count, offset) in [
        (56, strings.len(), strings_offset),
        (64, types.len(), types_offset),
        (72, 1, proto_offset),
        (80, field_types.len(), fields_offset),
        (88, 1, method_offset),
        (96, 1, class_offset),
    ] {
        put(&mut bytes, header, count as u32);
        put(
            &mut bytes,
            header + 4,
            if count == 0 { 0 } else { offset as u32 },
        );
    }
    put(&mut bytes, 108, data_start as u32);
    for (i, string) in strings.iter().enumerate() {
        let offset = bytes.len() as u32;
        put(&mut bytes, strings_offset + i * 4, offset);
        uleb(&mut bytes, string.len() as u32);
        bytes.extend_from_slice(string.as_bytes());
        bytes.push(0);
    }
    for i in 0..types.len() {
        put(&mut bytes, types_offset + i * 4, i as u32);
    }
    // Proto is ()V; class_idx is sample.Metadata (index 1).
    bytes[method_offset..method_offset + 2].copy_from_slice(&1u16.to_le_bytes());
    put(&mut bytes, method_offset + 4, method_name);
    for (i, ty) in field_types.iter().enumerate() {
        let p = fields_offset + i * 8;
        bytes[p..p + 2].copy_from_slice(&1u16.to_le_bytes());
        bytes[p + 2..p + 4]
            .copy_from_slice(&(types.iter().position(|v| v == ty).unwrap() as u16).to_le_bytes());
        put(&mut bytes, p + 4, field_names[i]);
    }
    align(&mut bytes);
    let code = bytes.len() as u32;
    for n in [1u16, 0, 0, regions.len() as u16] {
        bytes.extend(n.to_le_bytes());
    }
    bytes.extend(0u32.to_le_bytes());
    bytes.extend(4u32.to_le_bytes());
    for word in [0u16, 0, 0x000d, 0x000e] {
        bytes.extend(word.to_le_bytes());
    }
    for (start, count, offset) in regions {
        bytes.extend(start.to_le_bytes());
        bytes.extend(count.to_le_bytes());
        bytes.extend(offset.to_le_bytes());
    }
    bytes.extend(handlers);
    let class_data = bytes.len() as u32;
    uleb(&mut bytes, field_types.len() as u32);
    bytes.extend([0, 1, 0]);
    for i in 0..field_types.len() {
        uleb(&mut bytes, u32::from(i != 0));
        bytes.push(9);
    }
    bytes.extend([0, 9]);
    uleb(&mut bytes, code);
    let values = if encoded.is_empty() {
        0
    } else {
        bytes.len() as u32
    };
    bytes.extend(encoded);
    put(&mut bytes, class_offset, 1);
    put(&mut bytes, class_offset + 4, 1);
    put(&mut bytes, class_offset + 8, 2);
    put(&mut bytes, class_offset + 16, u32::MAX);
    put(&mut bytes, class_offset + 24, class_data);
    put(&mut bytes, class_offset + 28, values);
    let len = bytes.len();
    put(&mut bytes, 32, len as u32);
    put(&mut bytes, 104, (len - data_start) as u32);
    bytes
}

#[test]
fn static_values_preserve_sign_extension_float_bits_and_explicit_prefix() {
    let fields = [
        "B",
        "S",
        "C",
        "I",
        "J",
        "F",
        "D",
        "Z",
        "Ljava/lang/String;",
        "Ljava/lang/Object;",
        "I",
    ];
    let encoded = [
        10, 0x00, 0x80, 0x02, 0xff, 0x23, 0xff, 0xff, 0x64, 0, 0, 0, 0x80, 0x06, 0xff, 0x30, 0x80,
        0x3f, 0x31, 0xf0, 0x3f, 0x3f, 0x17, 0, 0x1e,
    ];
    let dex = native_dex::parse(&fixture(&fields, &encoded, &[], &[])).unwrap();
    assert_eq!(
        dex.classes[0].static_values,
        vec![
            DexValue::Byte(-128),
            DexValue::Short(-1),
            DexValue::Char(65535),
            DexValue::Int(i32::MIN),
            DexValue::Long(-1),
            DexValue::Float(1f32.to_bits()),
            DexValue::Double(1f64.to_bits()),
            DexValue::Boolean(true),
            DexValue::String(0),
            DexValue::Null
        ]
    );
    assert_eq!(dex.classes[0].fields.len(), 11); // trailing field is implicitly zero
}

#[test]
fn composite_values_remain_available_when_java_emission_cannot_use_them() {
    let encoded = [2, 0x1c, 2, 0x04, 7, 0x1e, 0x1d, 1, 1, 0, 0x3f];
    let dex = native_dex::parse(&fixture(
        &["[Ljava/lang/Object;", "Ljava/lang/Object;"],
        &encoded,
        &[],
        &[],
    ))
    .unwrap();
    assert_eq!(
        dex.classes[0].static_values,
        vec![
            DexValue::Array(vec![DexValue::Int(7), DexValue::Null]),
            DexValue::Annotation {
                type_idx: 1,
                elements: vec![(0, DexValue::Boolean(true))]
            }
        ]
    );
}

#[test]
fn reflective_values_keep_checked_symbol_indexes() {
    let encoded = [6, 0x15, 0, 0x17, 0, 0x18, 0, 0x19, 0, 0x1a, 0, 0x1b, 0];
    let dex = native_dex::parse(&fixture(&["Ljava/lang/Object;"; 6], &encoded, &[], &[])).unwrap();
    assert_eq!(
        dex.classes[0].static_values,
        vec![
            DexValue::MethodType(0),
            DexValue::String(0),
            DexValue::Type(0),
            DexValue::Field(0),
            DexValue::Method(0),
            DexValue::Enum(0)
        ]
    );
    for tag in [0x15, 0x17, 0x18, 0x19, 0x1a, 0x1b] {
        assert!(
            native_dex::parse(&fixture(&["Ljava/lang/Object;"], &[1, tag, 255], &[], &[])).is_err()
        );
    }
}

#[test]
fn method_handle_values_validate_map_members_and_index() {
    fn with_handle(index: u8, kind: u16, member: u16) -> Vec<u8> {
        let mut bytes = fixture(&["Ljava/lang/Object;"], &[1, 0x16, index], &[], &[]);
        align(&mut bytes);
        let handles = bytes.len() as u32;
        for value in [kind, 0, member, 0] {
            bytes.extend(value.to_le_bytes());
        }
        let map = bytes.len() as u32;
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(8u16.to_le_bytes());
        bytes.extend(0u16.to_le_bytes());
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(handles.to_le_bytes());
        put(&mut bytes, 52, map);
        let len = bytes.len() as u32;
        let data_start = u32::from_le_bytes(bytes[108..112].try_into().unwrap());
        put(&mut bytes, 32, len);
        put(&mut bytes, 104, len - data_start);
        bytes
    }
    let dex = native_dex::parse(&with_handle(0, 4, 0)).unwrap();
    assert_eq!(
        dex.classes[0].static_values,
        vec![DexValue::MethodHandle(0)]
    );
    for (index, kind, member) in [(1, 4, 0), (0, 9, 0), (0, 4, 1), (0, 0, 1)] {
        assert!(native_dex::parse(&with_handle(index, kind, member)).is_err());
    }
}

#[test]
fn typed_catch_and_catchall_order_and_shared_handlers_are_preserved() {
    let dex = native_dex::parse(&fixture(
        &[],
        &[],
        &[(0, 1, 1), (1, 1, 1)],
        &[1, 0x7f, 3, 2, 3],
    ))
    .unwrap();
    let code = dex.classes[0].methods[0].code.as_ref().unwrap();
    assert_eq!(code.tries, 2);
    assert_eq!((code.try_regions[0].start, code.try_regions[0].end), (0, 1));
    assert_eq!(
        code.try_regions[0].catches.as_ref(),
        &[(Some(Arc::from("Ljava/lang/Exception;")), 2), (None, 3)]
    );
    assert!(Arc::ptr_eq(
        &code.try_regions[0].catches,
        &code.try_regions[1].catches
    ));
    let catchall = native_dex::parse(&fixture(&[], &[], &[(0, 2, 1)], &[1, 0, 2])).unwrap();
    assert_eq!(
        catchall.classes[0].methods[0]
            .code
            .as_ref()
            .unwrap()
            .try_regions[0]
            .catches
            .as_ref(),
        &[(None, 2)]
    );
}

#[test]
fn malformed_values_and_handler_ranges_are_rejected() {
    for encoded in [
        &[1, 0x24, 1][..],
        &[1, 0x20, 0],
        &[1, 0x5f],
        &[1, 0x17, 255],
        &[2, 0x04, 1, 0x04, 2],
        &[1, 0x02, 1],
        &[1, 0x05],
        &[1, 0x90, 0, 0, 0, 0, 0],
    ] {
        assert!(
            native_dex::parse(&fixture(&["I"], encoded, &[], &[])).is_err(),
            "{encoded:?}"
        );
    }
    for (regions, handlers) in [
        (vec![(0, 2, 2)], vec![1, 0x7f, 3, 2, 3]), // middle of handler
        (vec![(0, 2, 1), (1, 1, 1)], vec![1, 0, 2]), // overlapping ranges
        (vec![(0, 5, 1)], vec![1, 0, 2]),          // range beyond code
        (vec![(0, 0, 1)], vec![1, 0, 2]),          // empty range
        (vec![(0, 1, 1)], vec![1, 0, 4]),          // target beyond code
        (vec![(0, 1, 1)], vec![1, 1, 0, 2]),       // primitive catch
        (vec![(0, 1, 1)], vec![1, 0xff, 0xff, 0xff, 0xff, 0x0f]), // SLEB overflow
        (vec![(0, 1, 1)], vec![1, 0x80, 0x80, 0x80, 0x80, 0x80]), // unterminated SLEB
    ] {
        assert!(
            native_dex::parse(&fixture(&[], &[], &regions, &handlers)).is_err(),
            "{regions:?} {handlers:?}"
        );
    }
}

#[test]
fn encoded_value_depth_and_retained_allocation_are_bounded() {
    let mut encoded = vec![1];
    for _ in 0..70 {
        encoded.extend([0x1c, 1]);
    }
    encoded.push(0x1e);
    assert!(native_dex::parse(&fixture(&["Ljava/lang/Object;"], &encoded, &[], &[])).is_err());
    let bytes = fixture(&["I"], &[1, 0x04, 1], &[(0, 1, 1)], &[1, 0, 2]);
    let mut remaining = 1;
    assert!(native_dex::parse_with_budget(&bytes, &mut remaining).is_err());
    assert_eq!(remaining, 0);
}

struct Annotations {
    bytes: Vec<u8>,
    item: usize,
    set: usize,
    directory: usize,
    parameters: usize,
    class_data: usize,
}

fn annotation_fixture() -> Annotations {
    let mut bytes = fixture(
        &[
            "Ldalvik/annotation/Throws;",
            "Ljava/io/IOException;",
            "Lsample/OtherAnnotation;",
        ],
        &[],
        &[],
        &[],
    );
    let read =
        |bytes: &[u8], at| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    let class = read(&bytes, 100);
    let class_data = read(&bytes, class + 24);
    let item = bytes.len();
    // visibility=system, Throws(type4), value(string8)=[IOException(type5), Exception(type3)]
    bytes.extend([2, 4, 1, 8, 0x1c, 2, 0x18, 5, 0x18, 3]);
    let other = bytes.len();
    bytes.extend([1, 6, 0]);
    align(&mut bytes);
    let set = bytes.len();
    bytes.extend(2u32.to_le_bytes());
    bytes.extend((item as u32).to_le_bytes());
    bytes.extend((other as u32).to_le_bytes());
    let other_set = bytes.len();
    bytes.extend(1u32.to_le_bytes());
    bytes.extend((other as u32).to_le_bytes());
    let parameters = bytes.len();
    bytes.extend(0u32.to_le_bytes());
    let directory = bytes.len();
    for value in [
        other_set as u32,
        1,
        1,
        1,
        0,
        other_set as u32,
        0,
        set as u32,
        0,
        parameters as u32,
    ] {
        bytes.extend(value.to_le_bytes());
    }
    put(&mut bytes, class + 20, directory as u32);
    let len = bytes.len();
    let data_start = read(&bytes, 108);
    put(&mut bytes, 32, len as u32);
    put(&mut bytes, 104, (len - data_start) as u32);
    Annotations {
        bytes,
        item,
        set,
        directory,
        parameters,
        class_data,
    }
}

#[test]
fn throws_annotations_decode_declared_order_and_preserve_other_annotation_metadata() {
    let fixture = annotation_fixture();
    let dex = native_dex::parse(&fixture.bytes).unwrap();
    assert_eq!(dex.classes[0].annotations_offset, fixture.directory as u32);
    assert_eq!(
        dex.classes[0].methods[0].thrown_types,
        vec![
            Arc::<str>::from("Ljava/io/IOException;"),
            Arc::from("Ljava/lang/Exception;")
        ]
    );
    let class = &dex.classes[0];
    let directory = class
        .symbols
        .annotations
        .get(&class.annotations_offset)
        .expect("decoded annotation directory");
    assert_eq!(directory.class.as_ref().unwrap()[0].visibility, 1);
    assert_eq!(directory.class.as_ref().unwrap()[0].type_idx, 6);
    assert_eq!(directory.methods[0].as_ref().unwrap().len(), 2);
    let mut remaining = 1;
    assert!(native_dex::parse_with_budget(&fixture.bytes, &mut remaining).is_err());
    assert_eq!(remaining, 0);
}

#[test]
fn class_only_annotation_directory_does_not_allocate_member_slots() {
    let mut fixture = annotation_fixture();
    put(&mut fixture.bytes, fixture.directory + 4, 0);
    put(&mut fixture.bytes, fixture.directory + 8, 0);
    put(&mut fixture.bytes, fixture.directory + 12, 0);
    let dex = native_dex::parse(&fixture.bytes).unwrap();
    let class = &dex.classes[0];
    let directory = class
        .symbols
        .annotations
        .get(&class.annotations_offset)
        .unwrap();
    assert!(directory.class.is_some());
    assert!(directory.fields.is_empty());
    assert!(directory.methods.is_empty());
    assert!(directory.parameters.is_empty());
}

#[test]
fn annotation_item_shapes_visibility_and_types_are_checked() {
    for (offset, value) in [
        (0, 3),
        (0, 1),
        (1, 0),
        (3, 255),
        (4, 0x1e),
        (6, 0x04),
        (7, 0),
        (7, 255),
        (5, 255),
    ] {
        let mut fixture = annotation_fixture();
        fixture.bytes[fixture.item + offset] = value;
        assert!(
            native_dex::parse(&fixture.bytes).is_err(),
            "item offset {offset}, value {value}"
        );
    }
}

#[test]
fn annotation_directories_sets_and_member_ownership_are_checked() {
    for case in 0..9 {
        let mut fixture = annotation_fixture();
        match case {
            0 => put(&mut fixture.bytes, fixture.directory + 16, 99), // field out of range
            1 => put(&mut fixture.bytes, fixture.directory + 24, 99), // method out of range
            2 => put(&mut fixture.bytes, fixture.directory + 32, 99), // parameter owner out of range
            3 => put(
                &mut fixture.bytes,
                fixture.directory + 28,
                (fixture.set + 1) as u32,
            ), // set alignment
            4 => put(&mut fixture.bytes, fixture.set + 4, u32::MAX),  // item offset bounds
            5 => put(&mut fixture.bytes, fixture.set + 8, fixture.item as u32), // duplicate annotation type
            6 => put(&mut fixture.bytes, fixture.parameters, 1), // zero-param method cannot have one parameter annotation
            7 => fixture.bytes[fixture.class_data + 2] = 0, // index exists but method not declared by class
            _ => put(&mut fixture.bytes, fixture.directory + 28, 0), // missing member annotations
        }
        assert!(
            native_dex::parse(&fixture.bytes).is_err(),
            "directory case {case}"
        );
    }
}

/// Optional local corpus check. It reads metadata only and does not invoke a JVM.
#[test]
#[ignore = "requires RDX_METADATA_APK pointing to a local APK"]
fn real_apk_metadata() {
    let input = std::env::var("RDX_METADATA_APK").expect("set RDX_METADATA_APK");
    let mut engine = NativeDexEngine::default();
    let project = engine.open(std::path::Path::new(&input)).unwrap();
    let mut regions = 0;
    let mut values = 0;
    let mut printed_values = 0;
    let mut methods_declaring_throws = 0;
    let mut declared_exceptions = 0;
    for name in &project.classes {
        let class = engine.class(name).unwrap();
        values += class.static_values.len();
        for method in &class.methods {
            if !method.thrown_types.is_empty() {
                methods_declaring_throws += 1;
                declared_exceptions += method.thrown_types.len();
            }
            if let Some(code) = &method.code {
                regions += code.try_regions.len();
                if method.name.as_ref() == "acceptNewIncomingCall" {
                    println!("{name}.{} regions={:?}", method.name, code.try_regions);
                }
            }
        }
        for (field, value) in class
            .fields
            .iter()
            .filter(|f| f.is_static)
            .zip(&class.static_values)
        {
            if field.name.starts_with("ARG_") && printed_values < 8 {
                println!("{name}.{}={value:?}", field.name);
                printed_values += 1;
            }
        }
    }
    println!(
        "classes={} try_regions={regions} explicit_static_values={values} methods_declaring_throws={methods_declaring_throws} declared_exceptions={declared_exceptions}",
        project.classes.len()
    );
    assert!(regions > 0);
    assert!(values > 0);
}
