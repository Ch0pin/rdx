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
fn call_link(source: &str, name: &str, label: &str) -> CodeLink {
    let byte = source.find(&format!("{name}(")).unwrap();
    let start = source[..byte].chars().count();
    CodeLink {
        start,
        end: start + name.chars().count(),
        label: label.into(),
    }
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
fn resource_symbols_preserve_unicode_offsets_and_skip_strings_comments() {
    let t = ResourceTable::parse(&table(0, false)).unwrap();
    let s = "// café 2130771968\nString s = \"2130771968\"; text.setText(2130771968); target();";
    let start = s
        .chars()
        .collect::<Vec<_>>()
        .windows(6)
        .position(|s| s.iter().collect::<String>() == "target")
        .unwrap();
    let mut c = DecompiledCode {
        source: s.into(),
        source_hash: source_identity(s),
        links: vec![
            call_link(s, "setText", "android.widget.TextView.setText(I)V"),
            CodeLink {
                start,
                end: start + 6,
                label: "sample.Target.target()V".into(),
            },
        ],
        definitions: vec![CodeDefinition {
            start,
            end: start + 6,
            kind: "method".into(),
            name: "target".into(),
        }],
    };
    t.decorate(&mut c, false);
    assert_eq!(c.source.matches("sample.app.R.string.title").count(), 1);
    assert!(
        c.source
            .contains("text.setText(sample.app.R.string.title);")
    );
    assert!(!c.source.contains("/* sample.app.R"));
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
    assert_eq!(
        c.source
            .chars()
            .skip(resource.start)
            .take(resource.end - resource.start)
            .collect::<String>(),
        "sample.app.R.string.title"
    );
    assert_eq!(c.source_hash, source_identity(&c.source));
}

#[test]
fn qualified_resources_replace_arguments_and_preserve_raw_dex_regions() {
    let t = ResourceTable::parse(&table(0, false)).unwrap();
    let source = concat!(
        "void build() { inflater.inflate(2130837504, null); resources.getXml(0x7f030000); }\n",
        ".field raw:I // encoded=2130771968\n",
        ".method raw()V\n",
        "    0000: 0014 0000 7f01 const v0, 2130771968\n",
        ".end method\n",
        "text.setText(2130771968);\n",
    );
    let mut code = DecompiledCode {
        source: source.into(),
        source_hash: source_identity(source),
        links: vec![
            call_link(
                source,
                "inflate",
                "android.view.LayoutInflater.inflate(ILandroid/view/ViewGroup;)Landroid/view/View;",
            ),
            call_link(
                source,
                "getXml",
                "android.content.res.Resources.getXml(I)Landroid/content/res/XmlResourceParser;",
            ),
            call_link(source, "setText", "android.widget.TextView.setText(I)V"),
        ],
        definitions: vec![],
    };
    t.decorate(&mut code, false);
    assert!(
        code.source
            .contains("inflater.inflate(sample.app.R.layout.main, null)")
    );
    assert!(
        code.source
            .contains("resources.getXml(sample.app.R.xml.settings)")
    );
    assert!(code.source.contains(".field raw:I // encoded=2130771968"));
    assert!(code.source.contains("const v0, 2130771968"));
    assert!(
        code.source
            .contains("text.setText(sample.app.R.string.title);")
    );
    assert_eq!(code.links.len(), 6);
    let once = code.clone();
    t.decorate(&mut code, false);
    assert_eq!(code.source, once.source);
    assert_eq!(code.links.len(), once.links.len());
}

#[test]
fn unsafe_resource_identifiers_and_non_integer_tokens_stay_literal() {
    let mut t = ResourceTable::parse(&table(0, false)).unwrap();
    t.entries.get_mut(&0x7f010000).unwrap().name = "title.invalid".into();
    let source = "int a = 2130771968; long b = 2130837504L; double c = 2130837504.0; int d = 02130837504; float e = 2130837504f; String block = \"\"\"\n\"example\" 2130837504\n\"\"\"; text.setText(2130771968);";
    let mut code = DecompiledCode {
        source: source.into(),
        source_hash: source_identity(source),
        links: vec![call_link(
            source,
            "setText",
            "android.widget.TextView.setText(I)V",
        )],
        definitions: vec![],
    };
    t.decorate(&mut code, false);
    assert_eq!(code.source, source);
    assert_eq!(code.links.len(), 1);
    t.entries.get_mut(&0x7f010000).unwrap().name = "class".into();
    t.decorate(&mut code, false);
    assert_eq!(code.source, source);
}

#[test]
fn resource_field_initializer_stays_numeric_while_uses_are_symbolic() {
    let t = ResourceTable::parse(&table(0, false)).unwrap();
    let source = "public static final int title = 2130771968;\nint value = -2130771968;\nswitch (value) { case 2130771968: break; }\ntext.setText(2130771968);";
    let start = source.find("title").unwrap();
    let mut code = DecompiledCode {
        source: source.into(),
        source_hash: source_identity(source),
        links: vec![
            call_link(source, "setText", "android.widget.TextView.setText(I)V"),
            CodeLink {
                start,
                end: start + 5,
                label: "sample.app.R$string.title:I".into(),
            },
        ],
        definitions: vec![CodeDefinition {
            start,
            end: start + 5,
            kind: "field".into(),
            name: "title".into(),
        }],
    };
    t.decorate(&mut code, false);
    assert!(code.source.contains("int title = 2130771968;"));
    assert!(code.source.contains("int value = -2130771968;"));
    assert!(code.source.contains("case 2130771968: break;"));
    assert_eq!(code.links.len(), 3);
}

#[test]
fn only_exact_android_resource_arguments_gain_resource_identity() {
    let t = ResourceTable::parse(&table(0, false)).unwrap();
    for (source, label, expected) in [
        (
            "text\n.setText(\n(int) (2130771968));",
            "android.widget.TextView.setText(I)V",
            "text\n.setText(\n(int) (sample.app.R.string.title));",
        ),
        (
            "text.setText(2130837504);",
            "android.widget.TextView.setText(I)V",
            "text.setText(2130837504);",
        ),
        (
            "text.setText(2130771968);",
            "sample.Lookalike.setText(I)V",
            "text.setText(2130771968);",
        ),
        (
            "text.setText(2130771968);",
            "android.widget.TextView.setText(Ljava/lang/CharSequence;)V",
            "text.setText(2130771968);",
        ),
        (
            "text.setText(-2130771968);",
            "android.widget.TextView.setText(I)V",
            "text.setText(-2130771968);",
        ),
        (
            "text.setText(2130771968 ^ 7);",
            "android.widget.TextView.setText(I)V",
            "text.setText(2130771968 ^ 7);",
        ),
        (
            "text.setText(2130771968L);",
            "android.widget.TextView.setText(I)V",
            "text.setText(2130771968L);",
        ),
        (
            "text.setText(2130771968f);",
            "android.widget.TextView.setText(I)V",
            "text.setText(2130771968f);",
        ),
        (
            "text.setText(02130771968);",
            "android.widget.TextView.setText(I)V",
            "text.setText(02130771968);",
        ),
        (
            "int hash = 2130771968; text.setText(2130771968);",
            "android.widget.TextView.setText(I)V",
            "int hash = 2130771968; text.setText(sample.app.R.string.title);",
        ),
    ] {
        let mut code = DecompiledCode {
            source: source.into(),
            source_hash: source_identity(source),
            links: vec![call_link(source, "setText", label)],
            definitions: vec![],
        };
        t.decorate(&mut code, false);
        assert_eq!(code.source, expected, "{source} / {label}");
        for resource in code
            .links
            .iter()
            .filter(|l| l.label.starts_with("resource://"))
        {
            assert!(resource.label.contains("ID 2130771968 / 0x7f010000"));
        }
    }
}

#[test]
fn straight_single_use_local_propagates_and_ambiguous_flow_stays_numeric() {
    let t = ResourceTable::parse(&table(0, false)).unwrap();
    for (source, replace) in [
        (
            "void show() { int id = (int) 2130771968; this.setText(id); }",
            true,
        ),
        (
            "void show() { int id = 2130771968; this.setText(id); hash(id); }",
            false,
        ),
        (
            "void show() { int id = 2130771968; id = 1; this.setText(id); }",
            false,
        ),
        (
            "void show() { int id = 2130771968; if (flag) this.setText(id); }",
            false,
        ),
        (
            "void show() { int id = -2130771968; this.setText(id); }",
            false,
        ),
    ] {
        let mut code = DecompiledCode {
            source: source.into(),
            source_hash: source_identity(source),
            links: vec![call_link(
                source,
                "setText",
                "android.widget.TextView.setText(I)V",
            )],
            definitions: vec![],
        };
        t.decorate(&mut code, false);
        assert_eq!(
            code.source.contains("sample.app.R.string.title"),
            replace,
            "{source}"
        );
        if !replace {
            assert_eq!(code.source, source);
        }
    }
}

#[test]
fn unknown_and_malformed_calls_have_no_resource_identity() {
    let t = ResourceTable::parse(&table(0, false)).unwrap();
    for source in [
        "text.setText(2130771968, 1);",
        "text.setText(2130771968;",
        "text.setText([2130771968));",
    ] {
        let mut code = DecompiledCode {
            source: source.into(),
            source_hash: source_identity(source),
            links: vec![call_link(
                source,
                "setText",
                "android.widget.TextView.setText(I)V",
            )],
            definitions: vec![],
        };
        t.decorate(&mut code, false);
        assert_eq!(code.source, source);
    }
    let source = "int hash = 2130771968; int flags = 0x7f010000; text.setText(2130771968);";
    let mut code = DecompiledCode {
        source: source.into(),
        source_hash: source_identity(source),
        links: vec![],
        definitions: vec![],
    };
    t.decorate(&mut code, false);
    assert_eq!(code.source, source);
    assert!(code.links.is_empty());
}

fn rendered_text_resource_call(owner: &str) -> DecompiledCode {
    use rdx::{
        native_dex::{self, DexSymbols},
        native_java,
    };
    use std::sync::Arc;
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class
        .methods
        .retain(|method| method.name.as_ref() == "answer");
    let descriptor = format!("L{};", owner.replace('.', "/"));
    class.symbols = Arc::new(DexSymbols {
        types: vec![descriptor.clone().into()],
        strings: vec!["setText".into()],
        protos: vec![("V".into(), vec!["I".into()])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let method = &mut class.methods[0];
    method.name = "show".into();
    method.return_type = "V".into();
    method.parameters = vec![descriptor.into()];
    method.access_flags = 9;
    let code = method.code.as_mut().unwrap();
    code.registers = 2;
    code.ins = 1;
    code.outs = 2;
    code.instructions = vec![0x0014, 0, 0x7f01, 0x206e, 0, 0x0001, 0x000e];
    native_java::render_method("sample.Hello", &class, &class.methods[0]).unwrap()
}

#[test]
fn native_dex_method_links_prove_resource_uses_and_reject_lookalikes() {
    let t = ResourceTable::parse(&table(0, false)).unwrap();
    let mut code = rendered_text_resource_call("android.widget.TextView");
    assert!(
        code.links
            .iter()
            .any(|link| link.label == "android.widget.TextView.setText(I)V"),
        "{}",
        code.source
    );
    t.decorate(&mut code, false);
    assert!(
        code.source.contains("setText(sample.app.R.string.title)"),
        "{}",
        code.source
    );
    let mut custom = rendered_text_resource_call("sample.Lookalike");
    let before = custom.source.clone();
    t.decorate(&mut custom, false);
    assert_eq!(custom.source, before);
}

#[test]
#[ignore = "Set RDX_TEST_APK to the Airbnb APK"]
fn airbnb_animation_resource_arguments_are_inline_symbols() {
    let mut engine = NativeEngine::start().unwrap();
    let project = engine
        .open(std::path::Path::new(
            &std::env::var_os("RDX_TEST_APK").unwrap(),
        ))
        .unwrap();
    assert!(
        engine.resource_error().is_none(),
        "{:?}",
        engine.resource_error()
    );
    let resource = &engine.resource_table().entries[&2130772070];
    assert_eq!(resource.package, "com.airbnb.android");
    assert_eq!(resource.kind, "anim");
    assert_eq!(resource.name, "n2_fade_in_fast");
    let resource = &engine.resource_table().entries[&2130772073];
    assert_eq!(resource.package, "com.airbnb.android");
    assert_eq!(resource.kind, "anim");
    assert_eq!(resource.name, "n2_fade_out_fast");
    let source = "overridePendingTransition(2130772070, 2130772073);";
    let mut code = DecompiledCode {
        source: source.into(),
        source_hash: source_identity(source),
        links: vec![call_link(
            source,
            "overridePendingTransition",
            "android.app.Activity.overridePendingTransition(II)V",
        )],
        definitions: vec![],
    };
    engine.resource_table().decorate(&mut code, false);
    assert_eq!(
        code.source,
        "overridePendingTransition(com.airbnb.android.R.anim.n2_fade_in_fast, com.airbnb.android.R.anim.n2_fade_out_fast);"
    );
    let resources: Vec<_> = code
        .links
        .iter()
        .filter(|link| link.label.starts_with("resource://"))
        .collect();
    assert_eq!(resources.len(), 2);
    for (link, name, id) in [
        (resources[0], "n2_fade_in_fast", 2130772070u32),
        (resources[1], "n2_fade_out_fast", 2130772073u32),
    ] {
        assert_eq!(
            code.source
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect::<String>(),
            format!("com.airbnb.android.R.anim.{name}")
        );
        assert!(link.label.starts_with(&format!("resource://{id:08x}/")));
    }
    if project
        .classes
        .iter()
        .any(|class| class == "com.airbnb.android.R$anim")
    {
        let own = engine
            .decompile_with_metadata("com.airbnb.android.R$anim")
            .unwrap();
        for (name, id) in [
            ("n2_fade_in_fast", 2130772070),
            ("n2_fade_out_fast", 2130772073),
        ] {
            let definition = own
                .definitions
                .iter()
                .find(|d| d.kind == "field" && d.name == name)
                .unwrap();
            assert!(own.links.iter().any(|link| link.start == definition.start
                && link.end == definition.end
                && link.label == format!("com.airbnb.android.R$anim.{name}:I")));
            let suffix: String = own
                .source
                .chars()
                .skip(definition.end)
                .take_while(|c| *c != ';')
                .collect();
            assert_eq!(suffix.trim(), format!("= {id}"));
        }
    }
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

#[test]
#[ignore = "Set RDX_TEST_APK to the Airbnb APK"]
fn airbnb_splash_resources_use_verified_contracts_in_all_source_paths() {
    let mut engine = NativeEngine::start().unwrap();
    engine
        .open(std::path::Path::new(
            &std::env::var_os("RDX_TEST_APK").unwrap(),
        ))
        .unwrap();
    let owner = "com.airbnb.android.feat.splashscreen.SplashScreenActivity";
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let start = std::time::Instant::now();
    let (method, fallback) = engine
        .decompile_method_source(&format!("{owner}.onCreate(Landroid/os/Bundle;)V"), &cancel)
        .unwrap();
    assert!(fallback.is_none(), "{fallback:?}");
    println!("method rendering: {:?}", start.elapsed());
    let class = engine.decompile_with_metadata(owner).unwrap();
    let search = engine.decompile_search_cancellable(owner, &cancel).unwrap();
    for code in [&method, &class, &search] {
        for (id, name) in [
            (2131623992u32, "layout.activity_splash_screen"),
            (
                2132017477u32,
                "string.app_not_fully_installed_dialog_message",
            ),
        ] {
            let symbol = format!("com.airbnb.android.R.{name}");
            assert!(
                code.source.contains(&symbol),
                "{symbol} missing\n{}",
                code.source
            );
            let aliases =
                rdx::resource_search::byte_aliases(&code.source, &code.links, &id.to_string());
            assert!(!aliases.is_empty());
            assert!(
                aliases
                    .iter()
                    .all(|range| code.source[range.clone()] == symbol)
            );
            let link = code
                .links
                .iter()
                .find(|link| rdx::resource_search::link_id(link) == Some(id))
                .unwrap();
            assert!(link.label.contains(&format!("ID {id} / 0x{id:08x}")));
        }
        assert!(code.source.contains("getDrawable(2131231142)"));
        let missing = code
            .links
            .iter()
            .find(|link| rdx::resource_search::link_id(link) == Some(2131231142))
            .unwrap();
        assert!(
            missing
                .label
                .contains("No entry in the loaded APK's resource table")
        );
        assert!(code.source.contains("null, 62)"));
    }
    // This APK retains the install-check drawable literal, but no resource entry.
    assert!(!engine.resource_table().entries.contains_key(&2131231142));
    println!("{}", method.source);
}

#[test]
fn missing_resources_keep_numeric_literals_with_explanatory_hover() {
    let t = ResourceTable::parse(&table(0, false)).unwrap();
    let source = "int hash = 2131231142; context.getDrawable(2131231142);";
    let mut code = DecompiledCode {
        source: source.into(),
        source_hash: source_identity(source),
        links: vec![call_link(
            source,
            "getDrawable",
            "android.content.Context.getDrawable(I)Landroid/graphics/drawable/Drawable;",
        )],
        definitions: vec![],
    };
    t.decorate(&mut code, false);
    assert_eq!(code.source, source);
    let resource_links = code
        .links
        .iter()
        .filter(|l| l.label.starts_with("resource://"))
        .collect::<Vec<_>>();
    assert_eq!(resource_links.len(), 1);
    assert_eq!(
        &source[resource_links[0].start..resource_links[0].end],
        "2131231142"
    );
    assert!(
        resource_links[0]
            .label
            .contains("No entry in the loaded APK's resource table")
    );
    assert!(t.document(2131231142).is_err());
    let mut engine = NativeEngine::start().unwrap();
    engine
        .open(std::path::Path::new("tests/fixtures/hello.dex"))
        .unwrap();
    assert!(
        engine
            .decompile_with_metadata("resource://7f0801a6/unresolved")
            .unwrap()
            .source
            .contains("No entry exists")
    );
}
