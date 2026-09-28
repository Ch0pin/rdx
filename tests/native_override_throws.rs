//! Exact external API override contracts recover missing DEX Throws metadata.
use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;

fn restore_class(parent: &str) -> DexClass {
    let descriptor: Arc<str> = "Lsample/Agent;".into();
    DexClass {
        descriptor: descriptor.clone(),
        superclass: Some(parent.into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            types: vec!["Ljava/io/IOException;".into()],
            strings: vec!["<init>".into()],
            protos: vec![("V".into(), vec![])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: descriptor,
            name: "onRestore".into(),
            return_type: "V".into(),
            parameters: vec![
                "Landroid/app/backup/BackupDataInput;".into(),
                "I".into(),
                "Landroid/os/ParcelFileDescriptor;".into(),
            ],
            thrown_types: vec![],
            access_flags: 0x11,
            code: Some(DexCode {
                registers: 5,
                ins: 4,
                outs: 1,
                tries: 0,
                try_regions: vec![],
                instructions: vec![0x0022, 0, 0x1070, 0, 0, 0x0027],
                offset: 0,
            }),
        }],
    }
}

#[test]
fn exact_backup_override_infers_printed_exception_and_accepts_checked_throw() {
    for parent in [
        "Landroid/app/backup/BackupAgent;",
        "Landroid/app/backup/BackupAgentHelper;",
    ] {
        let class = restore_class(parent);
        let code = native_java::render_method("sample.Agent", &class, &class.methods[0]).unwrap();
        assert!(
            code.source.contains(") throws java.io.IOException {"),
            "{}",
            code.source
        );
        assert!(code.source.contains("throw v0;"));
        let link = code
            .links
            .iter()
            .find(|link| link.label == "java.io.IOException")
            .unwrap();
        assert_eq!(
            code.source
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect::<String>(),
            "java.io.IOException"
        );
    }
}

#[test]
fn exact_onbackup_contract_also_infers_ioexception() {
    let mut class = restore_class("Landroid/app/backup/BackupAgentHelper;");
    class.methods[0].name = "onBackup".into();
    class.methods[0].parameters = vec![
        "Landroid/os/ParcelFileDescriptor;".into(),
        "Landroid/app/backup/BackupDataOutput;".into(),
        "Landroid/os/ParcelFileDescriptor;".into(),
    ];
    let code = native_java::render_method("sample.Agent", &class, &class.methods[0]).unwrap();
    assert!(code.source.contains(") throws java.io.IOException {"));
}

#[test]
fn unrelated_or_intervening_ancestors_do_not_inherit_an_unproven_contract() {
    let mut root = restore_class("Ljava/lang/Object;");
    root.access_flags |= 0x10;
    root.symbols
        .hierarchy
        .set(Arc::new(TypeHierarchy::from_classes([&root]).unwrap()))
        .unwrap();
    let source = native_java::render_method("sample.Agent", &root, &root.methods[0])
        .unwrap()
        .source;
    assert!(source.contains("throws java.io.IOException"), "{source}");
    let class = restore_class("Lvendor/BackupAgentHelper;");
    assert!(native_java::render_method("sample.Agent", &class, &class.methods[0]).is_err());
    let class = restore_class("Lsample/Base;");
    let mut base = restore_class("Landroid/app/backup/BackupAgentHelper;");
    base.descriptor = "Lsample/Base;".into();
    base.methods.clear();
    class
        .symbols
        .hierarchy
        .set(Arc::new(
            TypeHierarchy::from_classes([&class, &base]).unwrap(),
        ))
        .unwrap();
    assert!(native_java::render_method("sample.Agent", &class, &class.methods[0]).is_err());
}

#[test]
fn static_visibility_name_and_descriptor_mismatches_do_not_infer_throws() {
    for flags in [4, 9] {
        let mut class = restore_class("Landroid/app/backup/BackupAgentHelper;");
        class.methods[0].access_flags = flags;
        if flags & 8 != 0 {
            class.methods[0].code.as_mut().unwrap().ins = 3;
        }
        assert!(native_java::render_method("sample.Agent", &class, &class.methods[0]).is_err());
    }
    let mut class = restore_class("Landroid/app/backup/BackupAgentHelper;");
    class.methods[0].name = "restore".into();
    assert!(native_java::render_method("sample.Agent", &class, &class.methods[0]).is_err());
    class.methods[0].name = "onRestore".into();
    class.methods[0].parameters[0] = "Ljava/lang/Object;".into();
    assert!(native_java::render_method("sample.Agent", &class, &class.methods[0]).is_err());
    class.methods[0].parameters[0] = "Landroid/app/backup/BackupDataInput;".into();
    class.methods[0].return_type = "I".into();
    assert!(native_java::render_method("sample.Agent", &class, &class.methods[0]).is_err());
}

#[test]
fn explicit_throws_are_preserved_without_inferred_widening() {
    let mut class = restore_class("Landroid/app/backup/BackupAgentHelper;");
    class.methods[0].thrown_types = vec!["Ljava/io/IOException;".into()];
    let code = native_java::render_method("sample.Agent", &class, &class.methods[0]).unwrap();
    assert!(code.source.contains("throws java.io.IOException {"));
    assert!(!code.source.contains("throws java.io.IOException,"));
    class.methods[0].thrown_types = vec!["Ljava/io/FileNotFoundException;".into()];
    assert!(native_java::render_method("sample.Agent", &class, &class.methods[0]).is_err());
}

fn file_provider(parent: &str) -> DexClass {
    let mut class = restore_class(parent);
    Arc::get_mut(&mut class.symbols).unwrap().types[0] = "Ljava/io/FileNotFoundException;".into();
    let method = &mut class.methods[0];
    method.name = "openFile".into();
    method.return_type = "Landroid/os/ParcelFileDescriptor;".into();
    method.parameters = vec!["Landroid/net/Uri;".into(), "Ljava/lang/String;".into()];
    let code = method.code.as_mut().unwrap();
    code.ins = 3;
    class
}

fn document_provider(parent: &str) -> DexClass {
    let mut class = file_provider(parent);
    let method = &mut class.methods[0];
    method.name = "queryDocument".into();
    method.return_type = "Landroid/database/Cursor;".into();
    method.parameters = vec!["Ljava/lang/String;".into(), "[Ljava/lang/String;".into()];
    class
}

#[test]
fn exact_documents_provider_override_emits_file_exception() {
    let class = document_provider("Landroid/provider/DocumentsProvider;");
    let source = native_java::render_method("sample.Agent", &class, &class.methods[0])
        .unwrap()
        .source;
    assert!(
        source.contains("throws java.io.FileNotFoundException"),
        "{source}"
    );
}

#[test]
fn documents_provider_contract_crosses_only_nonoverriding_loaded_parent() {
    let mut parent = document_provider("Landroid/provider/DocumentsProvider;");
    parent.descriptor = "Lsample/BaseDocs;".into();
    parent.methods.clear();
    let class = document_provider("Lsample/BaseDocs;");
    class
        .symbols
        .hierarchy
        .set(Arc::new(
            TypeHierarchy::from_classes([&class, &parent]).unwrap(),
        ))
        .unwrap();
    let source = native_java::render_method("sample.Agent", &class, &class.methods[0])
        .unwrap()
        .source;
    assert!(
        source.contains("throws java.io.FileNotFoundException"),
        "{source}"
    );
}

#[test]
fn documents_provider_wrong_signature_or_narrowed_parent_stays_rejected() {
    let mut wrong = document_provider("Landroid/provider/DocumentsProvider;");
    wrong.methods[0].parameters[1] = "Ljava/lang/Object;".into();
    assert!(native_java::render_method("sample.Agent", &wrong, &wrong.methods[0]).is_err());
    let mut parent = document_provider("Landroid/provider/DocumentsProvider;");
    parent.descriptor = "Lsample/BaseDocs;".into();
    parent.methods[0].declaring_type = parent.descriptor.clone();
    let class = document_provider("Lsample/BaseDocs;");
    class
        .symbols
        .hierarchy
        .set(Arc::new(
            TypeHierarchy::from_classes([&class, &parent]).unwrap(),
        ))
        .unwrap();
    assert!(native_java::render_method("sample.Agent", &class, &class.methods[0]).is_err());
}

#[test]
fn private_checked_throw_in_nonroot_class_is_declared() {
    let mut class = document_provider("Landroid/provider/DocumentsProvider;");
    class.methods[0].access_flags = 2;
    class.methods[0].name = "helper".into();
    let source = native_java::render_method("sample.Agent", &class, &class.methods[0])
        .unwrap()
        .source;
    assert!(
        source.contains("throws java.io.FileNotFoundException"),
        "{source}"
    );
}

#[test]
fn file_provider_contract_crosses_only_proven_nonoverriding_parents() {
    let mut parent = file_provider("Landroid/content/ContentProvider;");
    parent.descriptor = "Lsample/BaseProvider;".into();
    parent.methods.clear();
    let class = file_provider("Lsample/BaseProvider;");
    let hierarchy = TypeHierarchy::from_classes([&class, &parent]).unwrap();
    class.symbols.hierarchy.set(Arc::new(hierarchy)).unwrap();
    let source = native_java::render_method("sample.Agent", &class, &class.methods[0])
        .unwrap()
        .source;
    assert!(
        source.contains("throws java.io.FileNotFoundException"),
        "{source}"
    );
    assert!(source.contains("throw v0;"), "{source}");
}

#[test]
fn file_provider_missing_or_narrowed_parent_remains_rejected() {
    for narrow in [false, true] {
        let class = file_provider("Lsample/BaseProvider;");
        let mut parent = file_provider("Landroid/content/ContentProvider;");
        parent.descriptor = "Lsample/BaseProvider;".into();
        parent.methods[0].declaring_type = parent.descriptor.clone();
        let definitions = if narrow {
            vec![&class, &parent]
        } else {
            vec![&class]
        };
        class
            .symbols
            .hierarchy
            .set(Arc::new(TypeHierarchy::from_classes(definitions).unwrap()))
            .unwrap();
        assert!(native_java::render_method("sample.Agent", &class, &class.methods[0]).is_err());
    }
}

#[test]
fn file_provider_contract_rejects_wrong_signature_and_loaded_framework_shadow() {
    for change in 0..4 {
        let mut class = file_provider("Landroid/content/ContentProvider;");
        match change {
            0 => class.methods[0].access_flags |= 8,
            1 => class.methods[0].name = "different".into(),
            2 => class.methods[0].parameters[0] = "Ljava/lang/Object;".into(),
            _ => {
                let mut shadow = file_provider("Ljava/lang/Object;");
                shadow.descriptor = "Landroid/content/ContentProvider;".into();
                shadow.methods[0].declaring_type = shadow.descriptor.clone();
                let hierarchy = TypeHierarchy::from_classes([&class, &shadow]).unwrap();
                class.symbols.hierarchy.set(Arc::new(hierarchy)).unwrap();
            }
        }
        assert!(native_java::render_method("sample.Agent", &class, &class.methods[0]).is_err());
    }
}

#[test]
fn all_document_file_contracts_recover_exact_descriptors() {
    let signatures: &[(&str, &str, &[&str])] = &[
        (
            "createDocument",
            "Ljava/lang/String;",
            &[
                "Ljava/lang/String;",
                "Ljava/lang/String;",
                "Ljava/lang/String;",
            ],
        ),
        (
            "openDocument",
            "Landroid/os/ParcelFileDescriptor;",
            &[
                "Ljava/lang/String;",
                "Ljava/lang/String;",
                "Landroid/os/CancellationSignal;",
            ],
        ),
        (
            "openDocumentThumbnail",
            "Landroid/content/res/AssetFileDescriptor;",
            &[
                "Ljava/lang/String;",
                "Landroid/graphics/Point;",
                "Landroid/os/CancellationSignal;",
            ],
        ),
        (
            "queryChildDocuments",
            "Landroid/database/Cursor;",
            &[
                "Ljava/lang/String;",
                "[Ljava/lang/String;",
                "Ljava/lang/String;",
            ],
        ),
        (
            "queryDocument",
            "Landroid/database/Cursor;",
            &["Ljava/lang/String;", "[Ljava/lang/String;"],
        ),
        (
            "queryRecentDocuments",
            "Landroid/database/Cursor;",
            &["Ljava/lang/String;", "[Ljava/lang/String;"],
        ),
        (
            "querySearchDocuments",
            "Landroid/database/Cursor;",
            &[
                "Ljava/lang/String;",
                "Ljava/lang/String;",
                "[Ljava/lang/String;",
            ],
        ),
    ];
    for (name, returns, parameters) in signatures {
        let mut class = document_provider("Landroid/provider/DocumentsProvider;");
        let method = &mut class.methods[0];
        method.name = (*name).into();
        method.return_type = (*returns).into();
        method.parameters = parameters.iter().map(|p| (*p).into()).collect();
        method.code.as_mut().unwrap().ins = parameters.len() as u16 + 1;
        let source = native_java::render_method("sample.Agent", &class, &class.methods[0])
            .unwrap_or_else(|error| panic!("{name}: {error:#}"))
            .source;
        assert!(
            source.contains("throws java.io.FileNotFoundException"),
            "{name}: {source}"
        );
        class.methods[0].parameters[0] = "I".into();
        assert!(
            native_java::render_method("sample.Agent", &class, &class.methods[0]).is_err(),
            "{name}: wrong descriptor accepted"
        );
    }
}
