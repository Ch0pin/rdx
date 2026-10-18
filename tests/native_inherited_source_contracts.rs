use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;
fn class(name: &str, parent: &str, symbols: Arc<DexSymbols>) -> DexClass {
    DexClass {
        symbols,
        descriptor: name.into(),
        superclass: Some(parent.into()),
        interfaces: vec![],
        access_flags: 0x11,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![],
    }
}
fn backup_fixture() -> (DexClass, DexClass, Arc<DexSymbols>) {
    let symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Agent;".into()],
        strings: vec!["onBackup".into()],
        protos: vec![(
            "V".into(),
            vec![
                "Landroid/os/ParcelFileDescriptor;".into(),
                "Landroid/app/backup/BackupDataOutput;".into(),
                "Landroid/os/ParcelFileDescriptor;".into(),
            ],
        )],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let mut agent = class(
        "Lsample/Agent;",
        "Landroid/app/backup/BackupAgent;",
        symbols.clone(),
    );
    agent.methods = vec![DexMethod {
        declaring_type: agent.descriptor.clone(),
        name: "onBackup".into(),
        return_type: "V".into(),
        parameters: symbols.protos[0].1.clone(),
        thrown_types: vec![],
        access_flags: 1,
        code: Some(DexCode {
            registers: 4,
            ins: 4,
            outs: 0,
            tries: 0,
            try_regions: vec![],
            instructions: vec![0x000e],
            offset: 0,
        }),
    }];
    let mut caller = class("Lsample/Calls;", "Ljava/lang/Object;", symbols.clone());
    caller.methods = vec![DexMethod {
        declaring_type: caller.descriptor.clone(),
        name: "run".into(),
        return_type: "V".into(),
        parameters: vec![agent.descriptor.clone()],
        thrown_types: vec![],
        access_flags: 9,
        code: Some(DexCode {
            registers: 2,
            ins: 1,
            outs: 4,
            tries: 1,
            try_regions: vec![DexTryRegion {
                start: 0,
                end: 4,
                catches: vec![(None, 5)].into(),
            }],
            instructions: vec![0x0012, 0x406e, 0, 0x0001, 0x000e, 0x000d, 0x0027],
            offset: 0,
        }),
    }];
    (agent, caller, symbols)
}

#[test]
fn backup_header_contract_matches_emitted_source() {
    let (agent, mut caller, symbols) = backup_fixture();
    let h = TypeHierarchy::from_classes([&agent, &caller]).unwrap();
    assert_eq!(
        h.call_checked_effects(
            &agent.descriptor,
            "onBackup",
            &agent.methods[0].parameters,
            "V",
            false
        )
        .unwrap(),
        vec![Arc::<str>::from("Ljava/io/IOException;")]
    );
    symbols.hierarchy.set(Arc::new(h)).unwrap();
    let a = native_java::render_method("sample.Agent", &agent, &agent.methods[0])
        .unwrap()
        .source;
    assert!(a.contains("throws java.io.IOException"));
    let unchanged = native_java::render_method("sample.Calls", &caller, &caller.methods[0])
        .unwrap()
        .source;
    assert!(
        unchanged.contains("throw _rdx_caught_throw_"),
        "{unchanged}"
    );
    assert!(
        !unchanged
            .lines()
            .find(|line| line.contains(" run("))
            .unwrap()
            .contains(" throws "),
        "{unchanged}"
    );
    caller.methods[0].thrown_types = vec!["Ljava/io/IOException;".into()];
    let c = native_java::render_method("sample.Calls", &caller, &caller.methods[0])
        .unwrap()
        .source;
    assert!(c.contains("throw e1;"));
    assert!(
        !c.contains("_rdx_caught_throw_"),
        "already-declared rethrow keeps its original source"
    );
}

#[test]
fn backup_helper_and_restore_keep_exact_header_contracts() {
    for (parent, name, params) in [
        (
            "Landroid/app/backup/BackupAgentHelper;",
            "onBackup",
            vec![
                "Landroid/os/ParcelFileDescriptor;",
                "Landroid/app/backup/BackupDataOutput;",
                "Landroid/os/ParcelFileDescriptor;",
            ],
        ),
        (
            "Landroid/app/backup/BackupAgent;",
            "onRestore",
            vec![
                "Landroid/app/backup/BackupDataInput;",
                "I",
                "Landroid/os/ParcelFileDescriptor;",
            ],
        ),
    ] {
        let (mut agent, _, symbols) = backup_fixture();
        agent.superclass = Some(parent.into());
        agent.methods[0].name = name.into();
        agent.methods[0].parameters = params.iter().map(|p| (*p).into()).collect();
        let h = TypeHierarchy::from_classes([&agent]).unwrap();
        assert_eq!(
            h.call_checked_effects(
                &agent.descriptor,
                name,
                &agent.methods[0].parameters,
                "V",
                false
            )
            .unwrap(),
            vec![Arc::<str>::from("Ljava/io/IOException;")]
        );
        symbols.hierarchy.set(Arc::new(h)).unwrap();
        assert!(
            native_java::render_method("sample.Agent", &agent, &agent.methods[0])
                .unwrap()
                .source
                .contains("throws java.io.IOException")
        );
    }
}

#[test]
fn backup_prototype_access_static_and_indirect_parent_do_not_gain_header() {
    for variant in 0..4 {
        let (mut agent, _, _) = backup_fixture();
        match variant {
            0 => agent.methods[0].parameters[1] = "Ljava/lang/Object;".into(),
            1 => agent.methods[0].access_flags = 4,
            2 => agent.methods[0].access_flags = 9,
            _ => agent.superclass = Some("Lsample/Middle;".into()),
        }
        let h = TypeHierarchy::from_classes([&agent]).unwrap();
        assert!(h
            .call_checked_effects(
                &agent.descriptor,
                "onBackup",
                &agent.methods[0].parameters,
                "V",
                variant == 2
            )
            .unwrap()
            .is_empty());
    }
}

#[test]
fn inherited_io_headers_match_empty_body_lookup_and_loaded_shadow() {
    for shadow in [false, true] {
        let (mut agent, _, symbols) = backup_fixture();
        agent.superclass = Some("Ljava/io/InputStream;".into());
        agent.methods[0].name = "close".into();
        agent.methods[0].parameters.clear();
        let code = agent.methods[0].code.as_mut().unwrap();
        code.registers = 1;
        code.ins = 1;
        let mut parent = class(
            "Ljava/io/InputStream;",
            "Ljava/lang/Object;",
            symbols.clone(),
        );
        let mut declaration = DexMethod {
            declaring_type: agent.descriptor.clone(),
            name: "close".into(),
            return_type: "V".into(),
            parameters: vec![],
            thrown_types: vec![],
            access_flags: 1,
            code: None,
        };
        declaration.declaring_type = parent.descriptor.clone();
        declaration.code = None;
        parent.methods = vec![declaration];
        let h = if shadow {
            TypeHierarchy::from_classes([&agent, &parent])
        } else {
            TypeHierarchy::from_classes([&agent])
        }
        .unwrap();
        let contract = h
            .call_checked_effects(&agent.descriptor, "close", &[], "V", false)
            .unwrap();
        symbols.hierarchy.set(Arc::new(h)).unwrap();
        let source = native_java::render_method("sample.Agent", &agent, &agent.methods[0])
            .unwrap()
            .source;
        assert_eq!(source.contains("throws java.io.IOException"), !shadow);
        assert_eq!(contract.is_empty(), shadow);
    }
}

#[test]
fn content_provider_file_header_requires_exact_prototype() {
    for mismatch in [false, true] {
        let (mut agent, _, symbols) = backup_fixture();
        agent.superclass = Some("Landroid/content/ContentProvider;".into());
        let method = &mut agent.methods[0];
        method.name = "openFile".into();
        method.return_type = "Landroid/os/ParcelFileDescriptor;".into();
        method.parameters = vec![
            "Landroid/net/Uri;".into(),
            if mismatch {
                "Ljava/lang/Object;"
            } else {
                "Ljava/lang/String;"
            }
            .into(),
        ];
        let code = method.code.as_mut().unwrap();
        code.registers = 3;
        code.ins = 3;
        code.instructions = vec![0x0012, 0x0011];
        let h = TypeHierarchy::from_classes([&agent]).unwrap();
        let effects = h
            .call_checked_effects(
                &agent.descriptor,
                "openFile",
                &agent.methods[0].parameters,
                "Landroid/os/ParcelFileDescriptor;",
                false,
            )
            .unwrap();
        symbols.hierarchy.set(Arc::new(h)).unwrap();
        let source = native_java::render_method("sample.Agent", &agent, &agent.methods[0])
            .unwrap()
            .source;
        assert_eq!(
            source.contains("throws java.io.FileNotFoundException"),
            !mismatch
        );
        assert_eq!(effects.is_empty(), mismatch);
    }
}

#[test]
#[ignore = "requires javac and java"]
fn inherited_header_precise_rethrow_preserves_original_fault_identity() {
    for declared in [false, true] {
        let (mut agent, mut caller, symbols) = backup_fixture();
        agent.access_flags = 0x401;
        agent.methods[0].access_flags = 0x401;
        agent.methods[0].code = None;
        if declared {
            caller.methods[0].thrown_types = vec!["Ljava/io/IOException;".into()];
        }
        let h = TypeHierarchy::from_classes([&agent, &caller]).unwrap();
        symbols.hierarchy.set(Arc::new(h)).unwrap();
        let declaration = native_java::render_method("sample.Agent", &agent, &agent.methods[0])
            .unwrap()
            .source;
        let body = native_java::render("sample.Calls", &caller).unwrap().source;
        assert_eq!(body.contains("throw _rdx_caught_throw_"), !declared);
        let dir = std::env::temp_dir().join(format!(
            "rdx-inherited-header-{}-{declared}",
            std::process::id()
        ));
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(dir.clone());
        for path in ["sample", "android/app/backup", "android/os"] {
            std::fs::create_dir_all(dir.join(path)).unwrap();
        }
        for (path, source) in [
        ("sample/Agent.java", format!("package sample; public abstract class Agent extends android.app.backup.BackupAgent {{\n{declaration}}}")),
        ("sample/Calls.java", body),
        ("android/app/backup/BackupAgent.java", "package android.app.backup; public abstract class BackupAgent { public abstract void onBackup(android.os.ParcelFileDescriptor a, BackupDataOutput b, android.os.ParcelFileDescriptor c) throws java.io.IOException; }".into()),
        ("android/app/backup/BackupDataOutput.java", "package android.app.backup; public class BackupDataOutput {}".into()),
        ("android/os/ParcelFileDescriptor.java", "package android.os; public class ParcelFileDescriptor {}".into()),
        ("sample/Main.java", "package sample; public class Main { static final class Fault extends Agent { Throwable error; public void onBackup(android.os.ParcelFileDescriptor a, android.app.backup.BackupDataOutput b, android.os.ParcelFileDescriptor c) throws java.io.IOException { if(error instanceof java.io.IOException) throw (java.io.IOException) error; if(error instanceof RuntimeException) throw (RuntimeException) error; if(error instanceof Error) throw (Error) error; } } public static void main(String[] args) throws Exception { Fault f=new Fault(); Calls.run(f); for(Throwable e:new Throwable[]{new java.io.IOException(),new RuntimeException(),new AssertionError()}) { f.error=e; try { Calls.run(f); throw new AssertionError(); } catch(Throwable actual) { if(actual!=e) throw new AssertionError(); } } System.out.println(\"identity-pass\"); } }".into()),
    ] { std::fs::write(dir.join(path), source).unwrap(); }
        let mut javac = std::process::Command::new("javac");
        javac.arg("-d").arg(&dir);
        for path in [
            "sample/Agent.java",
            "sample/Calls.java",
            "sample/Main.java",
            "android/app/backup/BackupAgent.java",
            "android/app/backup/BackupDataOutput.java",
            "android/os/ParcelFileDescriptor.java",
        ] {
            javac.arg(dir.join(path));
        }
        let result = javac.output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let result = std::process::Command::new("java")
            .arg("-cp")
            .arg(&dir)
            .arg("sample.Main")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(String::from_utf8_lossy(&result.stdout).contains("identity-pass"));
    }
}
