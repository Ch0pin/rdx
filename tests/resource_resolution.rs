use rdx::{
    engine::{
        CodeDefinition, CodeLink, DecompiledCode, DecompilerEngine, NativeEngine, source_identity,
    },
    resource_table::ResourceTable,
};
fn put16(b: &mut [u8], p: usize, v: u16) {
    b[p..p + 2].copy_from_slice(&v.to_le_bytes());
}
fn put32(b: &mut [u8], p: usize, v: u32) {
    b[p..p + 4].copy_from_slice(&v.to_le_bytes());
}
fn chunk(kind: u16, header: usize, len: usize) -> Vec<u8> {
    let mut b = vec![0; len];
    put16(&mut b, 0, kind);
    put16(&mut b, 2, header as u16);
    put32(&mut b, 4, len as u32);
    b
}
fn pool(strings: &[&str]) -> Vec<u8> {
    let start = 28 + 4 * strings.len();
    let mut b = chunk(1, 28, start);
    put32(&mut b, 8, strings.len() as u32);
    put32(&mut b, 16, 256);
    put32(&mut b, 20, start as u32);
    for (i, s) in strings.iter().enumerate() {
        let off = b.len() - start;
        put32(&mut b, 28 + i * 4, off as u32);
        b.push(s.encode_utf16().count() as u8);
        b.push(s.len() as u8);
        b.extend(s.as_bytes());
        b.push(0);
    }
    while !b.len().is_multiple_of(4) {
        b.push(0);
    }
    let n = b.len();
    put32(&mut b, 4, n as u32);
    b
}
fn table(flags: u8, compact: bool) -> Vec<u8> {
    let global = pool(&["Hello café", "res/layout/main.xml", "res/xml/settings.xml"]);
    let types = pool(&["string", "layout", "xml"]);
    let keys = pool(&["title", "main", "settings"]);
    let mut package = chunk(0x200, 288, 288);
    put32(&mut package, 8, 127);
    for (i, c) in "sample.app".encode_utf16().enumerate() {
        put16(&mut package, 12 + i * 2, c);
    }
    put32(&mut package, 268, 288);
    put32(&mut package, 276, (288 + types.len()) as u32);
    package.extend(types);
    package.extend(keys);
    for type_id in 1..=3u8 {
        let mut t = chunk(0x201, 28, 32);
        t[8] = type_id;
        t[9] = flags;
        put32(&mut t, 12, 1);
        put32(&mut t, 16, 32);
        put32(&mut t, 20, 8);
        // Offsets are zero in dense, sparse, and offset16 encodings.
        let mut e = vec![0; if compact { 8 } else { 16 }];
        if compact {
            put16(&mut e, 0, type_id as u16 - 1);
            put16(&mut e, 2, 0x308);
            put32(&mut e, 4, type_id as u32 - 1);
        } else {
            put16(&mut e, 0, 8);
            put32(&mut e, 4, type_id as u32 - 1);
            put16(&mut e, 8, 8);
            e[11] = 3;
            put32(&mut e, 12, type_id as u32 - 1);
        }
        t.extend(e);
        let n = t.len();
        put32(&mut t, 4, n as u32);
        package.extend(t);
    }
    let n = package.len();
    put32(&mut package, 4, n as u32);
    let mut all = chunk(2, 12, 12);
    put32(&mut all, 8, 1);
    all.extend(global);
    all.extend(package);
    let n = all.len();
    put32(&mut all, 4, n as u32);
    all
}
#[test]
fn normal_sparse_offset16_and_compact_tables_resolve_values() {
    for flags in [0, 1, 2] {
        for compact in [false, true] {
            let t = ResourceTable::parse(&table(flags, compact)).unwrap();
            assert_eq!(t.entries.len(), 3);
            assert_eq!(t.entries[&0x7f010000].variants[0].value, "Hello café");
            assert_eq!(
                t.entries[&0x7f020000].variants[0].file.as_deref(),
                Some("res/layout/main.xml")
            );
        }
    }
}
#[test]
fn malformed_table_is_rejected_without_panics() {
    let b = table(0, false);
    for end in 0..b.len() {
        assert!(ResourceTable::parse(&b[..end]).is_err());
    }
    let mut b = table(0, false);
    put32(&mut b, 8, 2);
    assert!(ResourceTable::parse(&b).is_err());
}
#[test]
fn resource_annotations_preserve_unicode_offsets_and_skip_strings_comments() {
    let t = ResourceTable::parse(&table(0, false)).unwrap();
    let s = "// café 2130771968\nString s = \"2130771968\"; int title = 2130771968; target();";
    let start = s
        .chars()
        .collect::<Vec<_>>()
        .windows(6)
        .position(|s| s.iter().collect::<String>() == "target")
        .unwrap();
    let mut c = DecompiledCode {
        source: s.into(),
        source_hash: source_identity(s),
        links: vec![CodeLink {
            start,
            end: start + 6,
            label: "sample.Target.target()V".into(),
        }],
        definitions: vec![CodeDefinition {
            start,
            end: start + 6,
            kind: "method".into(),
            name: "target".into(),
        }],
    };
    t.decorate(&mut c, false);
    assert_eq!(c.source.matches("sample.app.R.string.title").count(), 1);
    assert!(c.source.contains("\"2130771968\""));
    let l = c
        .links
        .iter()
        .find(|l| l.label == "sample.Target.target()V")
        .unwrap();
    assert_eq!(
        c.source
            .chars()
            .skip(l.start)
            .take(l.end - l.start)
            .collect::<String>(),
        "target"
    );
    assert_eq!(c.definitions[0].start, l.start);
    let resource = c
        .links
        .iter()
        .find(|l| l.label.starts_with("resource://"))
        .unwrap();
    assert!(resource.label.contains("Hello café"));
}
#[test]
fn xml_and_resource_documents_link_files_and_ids() {
    let t = ResourceTable::parse(&table(0, false)).unwrap();
    let c = t.document(0x7f020000).unwrap();
    assert!(
        c.links
            .iter()
            .any(|l| l.label == "resource://file/res/layout/main.xml")
    );
    let mut c = DecompiledCode {
        source: "<view text=\"@0x7f010000\" unknown=\"@0x01010000\"/>".into(),
        source_hash: String::new(),
        links: vec![],
        definitions: vec![],
    };
    t.decorate(&mut c, true);
    assert!(c.source.contains("@sample.app:string/title"));
    assert!(c.source.contains("@0x01010000"));
    assert_eq!(c.links.len(), 1);
}
#[test]
#[ignore = "Set RDX_TEST_APK to a real APK"]
fn real_apk_resource_names_and_navigation() {
    let mut e = NativeEngine::start().unwrap();
    e.open(std::path::Path::new(
        &std::env::var_os("RDX_TEST_APK").unwrap(),
    ))
    .unwrap();
    assert!(e.resource_error().is_none(), "{:?}", e.resource_error());
    println!("Indexed {} resources", e.resource_table().entries.len());
    assert!(e.resource_table().entries.len() > 100);
    for kind in ["string", "layout", "xml"] {
        let id = e
            .resource_table()
            .entries
            .values()
            .find(|r| r.kind == kind)
            .unwrap()
            .id;
        let target = format!("resource://{id:08x}");
        let code = e.decompile_with_metadata(&target).unwrap();
        assert!(code.source.contains("<resources>"));
        if kind != "string" {
            let link = code
                .links
                .iter()
                .find(|l| l.label.starts_with("resource://file/"))
                .unwrap();
            let next = e.navigate(&target, link.start, &code.source_hash).unwrap();
            assert!(next.code.source.contains('<'));
        }
    }
    let name = "com.google.android.finsky.screenshotsactivity.ScreenshotsActivityV2";
    let code = e.decompile_with_metadata(name).unwrap();
    let link = code
        .links
        .iter()
        .find(|l| l.label.starts_with("resource://"))
        .unwrap();
    let next = e.navigate(name, link.start, &code.source_hash).unwrap();
    assert!(next.code.source.contains("<resources>"));
    assert!(e.navigate(name, link.start, "stale").is_err());
}

#[test]
fn apk_resource_files_reopen_and_index_resets() {
    use std::io::Write;
    let path = std::env::temp_dir().join(format!("rdx-resource-test-{}.apk", std::process::id()));
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    let opts = zip::write::SimpleFileOptions::default();
    for (name, bytes) in [
        (
            "classes.dex",
            include_bytes!("fixtures/hello.dex").as_slice(),
        ),
        ("resources.arsc", table(0, false).as_slice()),
        (
            "res/layout/main.xml",
            b"<layout title=\"literal @0x7f010000\"/>".as_slice(),
        ),
        ("res/xml/settings.xml", b"<settings/>".as_slice()),
    ] {
        zip.start_file(name, opts).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
    let mut e = NativeEngine::start().unwrap();
    e.open(&path).unwrap();
    assert!(e.resource_error().is_none());
    let target = "resource://7f020000/layout/main";
    let code = e.decompile_with_metadata(target).unwrap();
    let link = &code.links[0];
    let nav = e.navigate(target, link.start, &code.source_hash).unwrap();
    assert_eq!(nav.class, "resource://file/res/layout/main.xml");
    assert!(nav.code.source.contains("literal @0x7f010000"));
    assert!(nav.code.links.is_empty());
    assert_eq!(
        e.decompile_with_metadata(&nav.class).unwrap().source_hash,
        nav.code.source_hash
    );
    e.open(std::path::Path::new("tests/fixtures/hello.dex"))
        .unwrap();
    assert!(e.resource_table().entries.is_empty());
    assert!(e.decompile_with_metadata(target).is_err());
    std::fs::remove_file(path).unwrap();
}

#[test]
fn cesu8_supplementary_characters_in_resource_values() {
    let old = table(0, false);
    let n = u32::from_le_bytes(old[16..20].try_into().unwrap()) as usize;
    let mut global = pool(&["abcdef", "res/layout/main.xml", "res/xml/settings.xml"]);
    let start = u32::from_le_bytes(global[20..24].try_into().unwrap()) as usize;
    global[start] = 2;
    global[start + 2..start + 8].copy_from_slice(&[0xed, 0xa0, 0xbd, 0xed, 0xb1, 0x8d]);
    let mut b = old[..12].to_vec();
    b.extend(global);
    b.extend(&old[12 + n..]);
    let len = b.len();
    put32(&mut b, 4, len as u32);
    let t = ResourceTable::parse(&b).unwrap();
    assert_eq!(t.entries[&0x7f010000].variants[0].value, "👍");
}

#[test]
fn configuration_variants_and_alias_navigation_are_preserved() {
    let mut b = table(0, false);
    let global = u32::from_le_bytes(b[16..20].try_into().unwrap()) as usize;
    let package = 12 + global;
    let type_start = package
        + 288
        + pool(&["string", "layout", "xml"]).len()
        + pool(&["title", "main", "settings"]).len();
    let size = u32::from_le_bytes(b[type_start + 4..type_start + 8].try_into().unwrap()) as usize;
    let mut variant = b[type_start..type_start + size].to_vec();
    variant[24] = 1;
    variant[43] = 1;
    put32(&mut variant, 44, 0x7f020000);
    b.extend(variant);
    let len = b.len();
    put32(&mut b, 4, len as u32);
    put32(&mut b, package + 4, (len - package) as u32);
    let t = ResourceTable::parse(&b).unwrap();
    let r = &t.entries[&0x7f010000];
    assert_eq!(r.variants.len(), 2);
    assert_eq!(r.variants[0].configuration, "default");
    assert!(r.variants[1].configuration.starts_with("config-"));
    let c = t.document(r.id).unwrap();
    assert!(
        c.links
            .iter()
            .any(|l| l.label.starts_with("resource://7f020000/"))
    );
}

#[test]
fn broken_resource_table_does_not_block_dex_navigation() {
    use std::io::Write;
    let path = std::env::temp_dir().join(format!("rdx-broken-resource-{}.apk", std::process::id()));
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    let opts = zip::write::SimpleFileOptions::default();
    zip.start_file("classes.dex", opts).unwrap();
    zip.write_all(include_bytes!("fixtures/hello.dex")).unwrap();
    zip.start_file("resources.arsc", opts).unwrap();
    zip.write_all(b"bad table").unwrap();
    zip.finish().unwrap();
    let mut engine = NativeEngine::start().unwrap();
    let project = engine.open(&path).unwrap();
    assert!(engine.resource_error().is_some());
    assert!(engine.resource_table().entries.is_empty());
    assert!(engine.navigate_class(&project.classes[0]).is_ok());
    std::fs::remove_file(path).unwrap();
}

#[test]
#[ignore = "Set RDX_ZOOM_APK to the Zoom APK"]
fn zoom_manifest_theme_resolution() {
    let mut engine = NativeEngine::start().unwrap();
    engine
        .open(std::path::Path::new(
            &std::env::var_os("RDX_ZOOM_APK").unwrap(),
        ))
        .unwrap();
    assert!(
        engine.resource_error().is_none(),
        "{:?}",
        engine.resource_error()
    );
    let code = engine
        .read_resource_with_metadata("AndroidManifest.xml")
        .unwrap();
    let label = engine.resource_table().label(0x7f1306f1).unwrap();
    println!("Theme: {label}");
    assert!(
        code.source
            .contains("android:protectionLevel=\"signature\"")
    );
    assert!(!code.source.contains("android:protectionLevel=\"0x2\""));

    let line = code
        .source
        .lines()
        .find(|line| line.contains("com.zipow.videobox.IMActivity"))
        .unwrap();
    println!("{line}");
    assert!(!line.contains("@0x7f1306f1"));
    assert!(line.contains("android:configChanges=\"keyboardHidden|orientation|screenLayout|uiMode|screenSize|smallestScreenSize|layoutDirection|colorMode\""));
    assert!(line.contains("android:windowSoftInputMode=\"stateHidden|adjustResize\""));
    assert!(
        code.links
            .iter()
            .any(|link| link.label.starts_with("resource://7f1306f1"))
    );
}
