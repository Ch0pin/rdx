#[path = "../src/native_resources.rs"]
mod native_resources;
use std::io::Read;
fn chunk(ty: u16, header: u16, body: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend(ty.to_le_bytes());
    b.extend(header.to_le_bytes());
    b.extend(((body.len() + 8) as u32).to_le_bytes());
    b.extend(body);
    b
}
fn words(v: &[u32]) -> Vec<u8> {
    v.iter().flat_map(|n| n.to_le_bytes()).collect()
}
fn compiled(wide: bool) -> Vec<u8> {
    compiled_attribute(wide, "enabled", 1, 0x12, 1)
}
fn compiled_attribute(wide: bool, attribute: &str, namespace: u32, ty: u8, value: u32) -> Vec<u8> {
    let strings = [
        "android",
        "http://schemas.android.com/apk/res/android",
        "manifest",
        "package",
        "test.native",
        "application",
        attribute,
        "label",
        "A & B",
    ];
    let mut offsets = Vec::new();
    let mut data = Vec::new();
    for s in strings {
        offsets.extend((data.len() as u32).to_le_bytes());
        if wide {
            data.extend((s.len() as u16).to_le_bytes());
            for c in s.encode_utf16() {
                data.extend(c.to_le_bytes());
            }
            data.extend([0, 0]);
        } else {
            data.extend([s.len() as u8, s.len() as u8]);
            data.extend(s.bytes());
            data.push(0);
        }
    }
    let mut body = words(&[
        strings.len() as u32,
        0,
        if wide { 0 } else { 256 },
        28 + 4 * strings.len() as u32,
        0,
    ]);
    body.extend(offsets);
    body.extend(data);
    let mut chunks = chunk(1, 28, &body);
    chunks.extend(chunk(0x100, 16, &words(&[1, u32::MAX, 0, 1])));
    fn start(ns: u32, id: u32, attrs: &[[u32; 5]]) -> Vec<u8> {
        let mut b = words(&[1, u32::MAX, ns, id]);
        for n in [20u16, 20, attrs.len() as u16, 0, 0, 0] {
            b.extend(n.to_le_bytes());
        }
        for a in attrs {
            b.extend(words(a));
        }
        chunk(0x102, 16, &b)
    }
    chunks.extend(start(u32::MAX, 2, &[[u32::MAX, 3, 4, 0x03000008, 4]]));
    chunks.extend(start(
        u32::MAX,
        5,
        &[
            [namespace, 6, u32::MAX, ((ty as u32) << 24) | 8, value],
            [1, 7, u32::MAX, 0x03000008, 8],
        ],
    ));
    chunks.extend(chunk(0x103, 16, &words(&[1, u32::MAX, u32::MAX, 5])));
    chunks.extend(chunk(0x103, 16, &words(&[1, u32::MAX, u32::MAX, 2])));
    chunks.extend(chunk(0x101, 16, &words(&[1, u32::MAX, 0, 1])));
    chunk(3, 8, &chunks)
}
#[test]
fn compiled_utf8_and_utf16_xml_decode_namespaces_and_values() {
    for wide in [false, true] {
        let text = native_resources::decode(&compiled(wide)).unwrap();
        assert!(text.contains("xmlns:android=\"http://schemas.android.com/apk/res/android\""));
        assert!(text.contains("package=\"test.native\""));
        assert!(text.contains("android:enabled=\"true\""));
        assert!(text.contains("android:label=\"A &amp; B\""));
        let mut reader = quick_xml::Reader::from_str(&text);
        loop {
            if let quick_xml::events::Event::Eof = reader.read_event().unwrap() {
                break;
            }
        }
    }
}

fn mapped_attribute(wide: bool, namespace: u32, id: u32) -> Vec<u8> {
    let mut bytes = compiled_attribute(wide, "", namespace, 0x10, 1);
    let pool_end = 8 + u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    let mut ids = [0; 7];
    ids[6] = id;
    bytes.splice(pool_end..pool_end, chunk(0x180, 8, &words(&ids)));
    let size = bytes.len() as u32;
    bytes[4..8].copy_from_slice(&size.to_le_bytes());
    bytes
}

#[test]
fn resource_map_recovers_stripped_attribute_names_namespaces_and_symbolic_values() {
    for wide in [false, true] {
        for namespace in [1, u32::MAX] {
            // screenOrientation=1: recover the enum name as well as the attribute.
            let xml =
                native_resources::decode(&mapped_attribute(wide, namespace, 0x0101001e)).unwrap();
            assert!(xml.contains("android:screenOrientation=\"portrait\""));
            assert!(xml.contains("android:label=\"A &amp; B\""));
            assert_eq!(xml.matches("xmlns:android=").count(), 1);
        }
    }
}

#[test]
fn recovered_attributes_declare_a_namespace_when_missing_or_prefix_is_occupied() {
    let bytes = mapped_attribute(false, u32::MAX, 0x0101001e);
    // Repoint the existing declaration and literal label to another namespace;
    // the recovered framework attribute must not inherit that namespace.
    let mut occupied = bytes.clone();
    let mut p = 8;
    while p < occupied.len() {
        let ty = u16::from_le_bytes(occupied[p..p + 2].try_into().unwrap());
        let size = u32::from_le_bytes(occupied[p + 4..p + 8].try_into().unwrap()) as usize;
        if matches!(ty, 0x100 | 0x101) {
            occupied[p + 20..p + 24].copy_from_slice(&4u32.to_le_bytes());
        } else if ty == 0x102 && occupied[p + 20..p + 24] == 5u32.to_le_bytes() {
            occupied[p + 56..p + 60].copy_from_slice(&4u32.to_le_bytes());
        }
        p += size;
    }
    let xml = native_resources::decode(&occupied).unwrap();
    assert!(xml.contains("xmlns:android1=\"http://schemas.android.com/apk/res/android\""));
    assert!(xml.contains("android1:screenOrientation=\"portrait\""));
    assert!(xml.contains("xmlns:android=\"test.native\""));

    // Strip both namespace chunks, and leave the ordinary label unqualified.
    let mut bare = bytes[..8].to_vec();
    let mut p = 8;
    while p < bytes.len() {
        let ty = u16::from_le_bytes(bytes[p..p + 2].try_into().unwrap());
        let size = u32::from_le_bytes(bytes[p + 4..p + 8].try_into().unwrap()) as usize;
        if !matches!(ty, 0x100 | 0x101) {
            let start = bare.len();
            bare.extend_from_slice(&bytes[p..p + size]);
            if ty == 0x102 && bytes[p + 20..p + 24] == 5u32.to_le_bytes() {
                bare[start + 56..start + 60].copy_from_slice(&u32::MAX.to_le_bytes());
            }
        }
        p += size;
    }
    let size = bare.len() as u32;
    bare[4..8].copy_from_slice(&size.to_le_bytes());
    let xml = native_resources::decode(&bare).unwrap();
    assert!(xml.contains("xmlns:android=\"http://schemas.android.com/apk/res/android\""));
    assert!(xml.contains("android:screenOrientation=\"portrait\""));
    assert!(xml.contains(" label=\"A &amp; B\""));
}

#[test]
fn unknown_resource_ids_do_not_invent_attribute_names_or_accept_duplicates() {
    for id in [0, 0x7f01001e, 0x0101ffff] {
        assert!(native_resources::decode(&mapped_attribute(false, 1, id)).is_err());
    }
    // Recovering label would collide with the existing label attribute.
    assert!(
        native_resources::decode(&mapped_attribute(false, 1, 0x01010001))
            .unwrap_err()
            .to_string()
            .contains("Duplicate XML attribute")
    );
    // Blank names without any resource identity remain invalid.
    assert!(native_resources::decode(&compiled_attribute(false, "", 1, 0x10, 1)).is_err());
}

#[test]
fn ordinary_resource_map_preserves_output_and_invalid_maps_fail() {
    let original = compiled(false);
    let pool_end = 8 + u32::from_le_bytes(original[12..16].try_into().unwrap()) as usize;
    let mut ids = [0; 9];
    ids[6] = 0x0101000e; // enabled
    ids[7] = 0x01010001; // label
    let map = chunk(0x180, 8, &words(&ids));
    let insert = |maps: &[u8]| {
        let mut b = original.clone();
        b.splice(pool_end..pool_end, maps.iter().copied());
        let size = b.len() as u32;
        b[4..8].copy_from_slice(&size.to_le_bytes());
        b
    };
    assert_eq!(
        native_resources::decode_with_references(&insert(&map)).unwrap(),
        native_resources::decode_with_references(&original).unwrap()
    );
    assert!(native_resources::decode(&insert(&[map.clone(), map].concat())).is_err());
    assert!(native_resources::decode(&insert(&chunk(0x180, 8, &[0; 40]))).is_err());
    assert!(native_resources::decode(&insert(&chunk(0x180, 8, &[0; 3]))).is_err());
    // An unknown identity with valid text is preserved, not reclassified.
    ids[6] = 0x7f01000e;
    assert_eq!(
        native_resources::decode(&insert(&chunk(0x180, 8, &words(&ids)))).unwrap(),
        native_resources::decode(&original).unwrap()
    );
}
#[test]
fn fixture_plain_manifest_is_preserved() {
    for file in ["navigation.apk", "preview.apk"] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(file);
        let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
        let mut b = Vec::new();
        archive
            .by_name("AndroidManifest.xml")
            .unwrap()
            .read_to_end(&mut b)
            .unwrap();
        assert_eq!(native_resources::decode(&b).unwrap().as_bytes(), b);
    }
}
#[test]
fn malformed_compiled_xml_fails_without_panicking() {
    let bytes = compiled(false);
    for end in 0..bytes.len() {
        assert!(native_resources::decode(&bytes[..end]).is_err());
    }
    for offset in [8usize, 10, 12, 16, 28, 36] {
        let mut corrupt = bytes.clone();
        corrupt[offset..offset + 4].fill(255);
        assert!(
            native_resources::decode(&corrupt).is_err(),
            "offset {offset}"
        );
    }
}
#[test]
fn optional_real_apk_manifest() {
    let Some(path) = std::env::var_os("RDX_NATIVE_XML_APK") else {
        return;
    };
    let archive_view = rdx::apk::Archive::open(std::path::Path::new(&path)).unwrap();
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut b = Vec::new();
    archive
        .by_name("AndroidManifest.xml")
        .unwrap()
        .read_to_end(&mut b)
        .unwrap();
    let text = native_resources::decode(&b).unwrap();
    let entry = archive_view
        .entries
        .iter()
        .find(|entry| entry.path == "AndroidManifest.xml")
        .unwrap();
    let rdx::apk::Preview::Text {
        text: preview,
        syntax,
        note,
    } = archive_view.preview(entry.index).unwrap()
    else {
        panic!("manifest preview fell back to binary");
    };
    assert_eq!(preview, text);
    assert_eq!(syntax, "xml");
    assert!(note.is_none());
    let package = rdx::manifest_summary::package_name(&text).unwrap();
    let components = rdx::manifest_summary::components(&text).unwrap();
    eprintln!(
        "Decoded {package}: {} components, {} XML bytes",
        components.len(),
        text.len()
    );
    if let Some(path) = std::env::var_os("RDX_NATIVE_XML_OUTPUT") {
        std::fs::write(path, &text).unwrap();
    }
    assert!(text.contains("<manifest") && text.contains("</manifest>"));
    assert!(text.contains("android:name="));
    let mut reader = quick_xml::Reader::from_str(&text);
    loop {
        if let quick_xml::events::Event::Eof = reader.read_event().unwrap() {
            break;
        }
    }
}

#[test]
fn archive_previews_compiled_xml_without_loading_any_dex_engine() {
    use std::io::Write;
    for (index, name) in [
        "AndroidManifest.xml",
        "res/layout/main.xml",
        "assets/compiled.bin",
    ]
    .iter()
    .enumerate()
    {
        let path = std::env::temp_dir().join(format!(
            "rdx-compiled-preview-{}-{index}.apk",
            std::process::id()
        ));
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        zip.start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&compiled(index == 1)).unwrap();
        zip.finish().unwrap();
        let archive = rdx::apk::Archive::open(&path).unwrap();
        let preview = archive.preview(0).unwrap();
        std::fs::remove_file(path).unwrap();
        let rdx::apk::Preview::Text { text, syntax, note } = preview else {
            panic!("compiled XML became a binary preview")
        };
        assert_eq!(syntax, "xml");
        assert!(text.contains("package=\"test.native\""));
        assert!(note.is_none());
    }
}

#[test]
#[ignore = "Set RDX_EXPEDIA_APK to validate the reported APK preview independently of DEX loading"]
fn expedia_manifest_previews_without_dex_engine() {
    let path = std::env::var_os("RDX_EXPEDIA_APK").expect("RDX_EXPEDIA_APK required");
    let archive = rdx::apk::Archive::open(std::path::Path::new(&path)).unwrap();
    let entry = archive
        .entries
        .iter()
        .find(|e| e.path == "AndroidManifest.xml")
        .unwrap();
    let rdx::apk::Preview::Text { text, syntax, note } = archive.preview(entry.index).unwrap()
    else {
        panic!("Expedia manifest was not decoded")
    };
    assert_eq!(syntax, "xml");
    assert!(text.contains("package=\"com.expedia.bookings\""));
    assert!(text.contains("<application"));
    assert!(note.is_none());
}

#[test]
fn compiled_reference_metadata_excludes_literal_strings() {
    let mut bytes = compiled(false);
    let literal = words(&[1, 7, u32::MAX, 0x03000008, 8]);
    let offset = bytes
        .windows(literal.len())
        .position(|window| window == literal)
        .unwrap();
    let (_, refs) = native_resources::decode_with_references(&bytes).unwrap();
    assert!(refs.is_empty());
    bytes[offset + 12..offset + 16].copy_from_slice(&0x01000008u32.to_le_bytes());
    bytes[offset + 16..offset + 20].copy_from_slice(&0x7f010000u32.to_le_bytes());
    let (source, refs) = native_resources::decode_with_references(&bytes).unwrap();
    assert_eq!(refs.len(), 1);
    assert_eq!(
        source
            .chars()
            .skip(refs[0].start)
            .take(refs[0].len())
            .collect::<String>(),
        "@0x7f010000"
    );
}

#[test]
fn compiled_manifest_enum_and_flags_decode_by_namespace_and_attribute() {
    for wide in [false, true] {
        for (attribute, value, expected) in [
            ("protectionLevel", 2, "signature"),
            ("protectionLevel", 0x12, "signature|privileged"),
            ("windowSoftInputMode", 0x12, "stateHidden|adjustResize"),
            ("launchMode", 2, "singleTask"),
            ("protectionLevel", 0x80000002, "0x80000002"),
        ] {
            let xml =
                native_resources::decode(&compiled_attribute(wide, attribute, 1, 0x11, value))
                    .unwrap();
            assert!(
                xml.contains(&format!("android:{attribute}=\"{expected}\"")),
                "{xml}"
            );
        }
        let xml = native_resources::decode(&compiled_attribute(
            wide,
            "protectionLevel",
            u32::MAX,
            0x11,
            2,
        ))
        .unwrap();
        assert!(xml.contains("protectionLevel=\"0x2\""));
    }
}

#[test]
fn application_attribute_ids_recover_stripped_names_only_with_matching_table() {
    let bytes = mapped_attribute(false, u32::MAX, 0x7f040c19);
    assert!(native_resources::decode(&bytes).is_err());
    let (xml, _) = native_resources::decode_with_attribute_resolver(&bytes, &|id| {
        (id == 0x7f040c19).then_some("uri")
    })
    .unwrap();
    assert!(xml.contains("app:uri=\"1\""));
    assert!(xml.contains("xmlns:app=\"http://schemas.android.com/apk/res-auto\""));
    let (xml, _) = native_resources::decode_with_attribute_resolver(&bytes, &|id| {
        (id == 0x7f040c19).then_some("2130971673")
    })
    .unwrap();
    assert!(xml.contains("app:resource_0x7f040c19=\"1\""));
    let (unmapped, _) =
        native_resources::decode_with_attribute_resolver(&bytes, &|_| None).unwrap();
    assert!(unmapped.contains("app:resource_0x7f040c19=\"1\""));
    assert!(native_resources::decode_with_attribute_resolver(&bytes, &|_| Some("123")).is_err());
    assert!(
        native_resources::decode_with_attribute_resolver(&bytes, &|_| Some("bad name")).is_err()
    );
}
