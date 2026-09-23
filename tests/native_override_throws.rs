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
    for parent in ["Ljava/lang/Object;", "Lvendor/BackupAgentHelper;"] {
        let class = restore_class(parent);
        assert!(native_java::render_method("sample.Agent", &class, &class.methods[0]).is_err());
    }
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
    for flags in [2, 4, 9] {
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
