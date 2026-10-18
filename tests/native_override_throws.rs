//! Exact external API override contracts recover missing DEX Throws metadata.
use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;

fn explicit_bridge_without_new_header(class: &DexClass) -> String {
    let source = native_java::render_method("sample.Agent", class, &class.methods[0])
        .unwrap()
        .source;
    assert!(source.contains("throw _rdx_checked_throw_"), "{source}");
    assert!(
        !source.lines().next().unwrap().contains(" throws "),
        "{source}"
    );
    source
}

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
    explicit_bridge_without_new_header(&class);
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
    explicit_bridge_without_new_header(&class);
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
    explicit_bridge_without_new_header(&class);
    class.methods[0].name = "onRestore".into();
    class.methods[0].parameters[0] = "Ljava/lang/Object;".into();
    explicit_bridge_without_new_header(&class);
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
    let source = native_java::render_method("sample.Agent", &class, &class.methods[0])
        .unwrap()
        .source;
    assert!(
        source
            .lines()
            .next()
            .unwrap()
            .contains("throws java.io.FileNotFoundException"),
        "{source}"
    );
    assert!(
        !source
            .lines()
            .next()
            .unwrap()
            .contains("java.io.IOException"),
        "{source}"
    );
    assert!(source.contains("throw _rdx_checked_throw_"), "{source}");
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
fn documents_provider_overload_or_narrowed_parent_uses_explicit_bridge_without_header_inference() {
    let mut wrong = document_provider("Landroid/provider/DocumentsProvider;");
    wrong.methods[0].parameters[1] = "Ljava/lang/Object;".into();
    explicit_bridge_without_new_header(&wrong);
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
    explicit_bridge_without_new_header(&class);
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
fn file_provider_missing_or_narrowed_parent_keeps_header_empty_with_explicit_bridge() {
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
        explicit_bridge_without_new_header(&class);
    }
}

#[test]
fn file_provider_invalid_static_declines_while_overloads_and_framework_shadow_keep_headers() {
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
        if change == 0 {
            assert!(native_java::render_method("sample.Agent", &class, &class.methods[0]).is_err());
        } else {
            explicit_bridge_without_new_header(&class);
        }
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
        explicit_bridge_without_new_header(&class);
    }
}

#[test]
fn pinned_override_declaration_conflicts_decline_even_without_any_throw() {
    for variant in 0..3 {
        let mut class = restore_class("Landroid/app/backup/BackupAgentHelper;");
        let method = &mut class.methods[0];
        method.code.as_mut().unwrap().instructions = vec![0x000e];
        match variant {
            0 => method.access_flags = 4,
            1 => {
                method.access_flags = 9;
                method.code.as_mut().unwrap().ins = 3;
            }
            _ => {
                method.return_type = "I".into();
                method.code.as_mut().unwrap().instructions = vec![0x0012, 0x000f];
            }
        }
        assert!(!native_java::ordinary_method_declaration_authority(
            &class,
            &class.methods[0]
        ));
        let error = native_java::render_method("sample.Agent", &class, &class.methods[0])
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("declaration conflicts with exact inherited SDK member"),
            "{variant}: {error}"
        );
    }
}

#[test]
fn no_throw_legal_overload_and_loaded_platform_shadow_keep_their_declarations() {
    let mut overload = restore_class("Landroid/app/backup/BackupAgentHelper;");
    overload.methods[0].parameters[0] = "Ljava/lang/Object;".into();
    overload.methods[0].code.as_mut().unwrap().instructions = vec![0x000e];
    let source = native_java::render_method("sample.Agent", &overload, &overload.methods[0])
        .unwrap()
        .source;
    assert!(
        !source.lines().next().unwrap().contains("throws"),
        "{source}"
    );
    let mut child = restore_class("Landroid/app/backup/BackupAgentHelper;");
    child.methods[0].access_flags = 4;
    child.methods[0].code.as_mut().unwrap().instructions = vec![0x000e];
    let mut shadow = restore_class("Ljava/lang/Object;");
    shadow.descriptor = "Landroid/app/backup/BackupAgentHelper;".into();
    shadow.methods.clear();
    child
        .symbols
        .hierarchy
        .set(Arc::new(
            TypeHierarchy::from_classes([&child, &shadow]).unwrap(),
        ))
        .unwrap();
    assert!(native_java::ordinary_method_declaration_authority(
        &child,
        &child.methods[0]
    ));
    native_java::render_method("sample.Agent", &child, &child.methods[0]).unwrap();
}

#[test]
fn pinned_reference_return_accepts_a_proven_covariant_descriptor() {
    let mut child = document_provider("Landroid/provider/DocumentsProvider;");
    child.methods[0].return_type = "Lsample/CursorImpl;".into();
    child.methods[0].code.as_mut().unwrap().instructions = vec![0x0012, 0x0011];
    let mut implementation = restore_class("Ljava/lang/Object;");
    implementation.descriptor = "Lsample/CursorImpl;".into();
    implementation.interfaces = vec!["Landroid/database/Cursor;".into()];
    implementation.methods.clear();
    child
        .symbols
        .hierarchy
        .set(Arc::new(
            TypeHierarchy::from_classes([&child, &implementation]).unwrap(),
        ))
        .unwrap();
    assert!(native_java::ordinary_method_declaration_authority(
        &child,
        &child.methods[0]
    ));
    let source = native_java::render_method("sample.Agent", &child, &child.methods[0])
        .unwrap()
        .source;
    assert!(
        source.contains("sample.CursorImpl queryDocument("),
        "{source}"
    );
}

#[test]
#[ignore = "requires javac/java; root runs serially"]
fn jvm_explicit_bridge_keeps_narrow_declared_override_and_original_broader_throw() {
    use std::{fs, process::Command};
    let mut class = restore_class("Landroid/app/backup/BackupAgentHelper;");
    class.methods[0].thrown_types = vec!["Ljava/io/FileNotFoundException;".into()];
    let source = native_java::render("sample.Agent", &class).unwrap().source;
    let dir = std::env::temp_dir().join(format!("rdx-override-explicit-{}", std::process::id()));
    for path in ["sample", "android/app/backup", "android/os"] {
        fs::create_dir_all(dir.join(path)).unwrap();
    }
    fs::write(dir.join("sample/Agent.java"), source).unwrap();
    fs::write(
        dir.join("android/app/backup/BackupDataInput.java"),
        "package android.app.backup;public class BackupDataInput {}",
    )
    .unwrap();
    fs::write(
        dir.join("android/os/ParcelFileDescriptor.java"),
        "package android.os;public class ParcelFileDescriptor {}",
    )
    .unwrap();
    fs::write(dir.join("android/app/backup/BackupAgentHelper.java"),"package android.app.backup;public class BackupAgentHelper {public void onRestore(BackupDataInput input,int version,android.os.ParcelFileDescriptor data)throws java.io.IOException {}}").unwrap();
    fs::write(dir.join("sample/Verify.java"),r#"package sample;public class Verify {
public static void main(String[] args)throws Exception {
 java.lang.reflect.Method method=Agent.class.getMethod("onRestore",android.app.backup.BackupDataInput.class,int.class,android.os.ParcelFileDescriptor.class);
 if(!java.util.Arrays.equals(method.getExceptionTypes(),new Class<?>[]{java.io.FileNotFoundException.class}))throw new AssertionError("header");
 android.app.backup.BackupAgentHelper receiver=new Agent();Throwable seen=null;
 try{receiver.onRestore(null,7,null);}catch(Throwable failure){seen=failure;}
 if(seen==null||seen.getClass()!=java.io.IOException.class)throw new AssertionError("original throwable",seen);
 StackTraceElement allocation=seen.getStackTrace()[0];if(!allocation.getClassName().equals("sample.Agent")||!allocation.getMethodName().equals("onRestore"))throw new AssertionError("allocation stack",seen);
}}
"#).unwrap();
    let compile = Command::new("javac")
        .args([
            "-Xlint:all",
            "-Werror",
            "-d",
            dir.to_str().unwrap(),
            "sample/Agent.java",
            "sample/Verify.java",
            "android/app/backup/BackupAgentHelper.java",
            "android/app/backup/BackupDataInput.java",
            "android/os/ParcelFileDescriptor.java",
        ])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new("java")
        .args(["-cp", dir.to_str().unwrap(), "sample.Verify"])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
