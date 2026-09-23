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
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut b = Vec::new();
    archive
        .by_name("AndroidManifest.xml")
        .unwrap()
        .read_to_end(&mut b)
        .unwrap();
    let text = native_resources::decode(&b).unwrap();
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
