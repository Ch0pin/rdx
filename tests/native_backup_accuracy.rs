//! Real-APK regression for restore loops with several continue edges.
use rdx::{engine::DecompilerEngine, native_engine::NativeDexEngine, native_java};

#[test]
#[ignore = "Set RDX_TEST_APK to the reported Play Store APK and run --ignored"]
fn restore_preserves_each_setting_loop_and_post_loop_handler() {
    let path = std::env::var_os("RDX_TEST_APK").expect("RDX_TEST_APK required");
    let mut engine = NativeDexEngine::default();
    engine.open(std::path::Path::new(&path)).unwrap();
    let name = "com.google.android.finsky.setup.VendingBackupAgent";
    let class = engine.class(name).unwrap();
    let method = class
        .methods
        .iter()
        .find(|m| m.name.as_ref() == "onRestore")
        .unwrap();
    let code =
        native_java::render_method(name, class, method).expect("restore Java reconstruction");
    assert!(!code.source.contains(".end method"));
    assert!(code.source.contains(") throws java.io.IOException {"));
    assert_eq!(code.source.matches("while (").count(), 1, "{}", code.source);
    for key in [
        "vending",
        "auto_update_enabled",
        "update_over_wifi_only",
        "auto_add_shortcuts",
        "notify_updates",
        "notify_updates_completion",
        "content-filter-level",
        "verify-apps-consent",
        "auto_revoke_modified_settings",
    ] {
        assert_eq!(
            code.source.matches(&format!("\"{key}\"")).count(),
            1,
            "{key}: {}",
            code.source
        );
    }
    for call in [
        ".readNextHeader(",
        ".readEntityData(",
        ".readLong(",
        ".readInt(",
    ] {
        assert_eq!(
            code.source.matches(call).count(),
            1,
            "{call}: {}",
            code.source
        );
    }
    assert_eq!(code.source.matches(".readBoolean(").count(), 7);
    for field in [
        "c:J", "d:Z", "e:Z", "f:Z", "g:Z", "h:Z", "i:I", "j:Z", "k:Z",
    ] {
        let symbol = format!("aghm.{field}");
        assert_eq!(
            code.links.iter().filter(|l| l.label == symbol).count(),
            1,
            "{symbol}"
        );
    }
    let protected = code.source.find("try {").unwrap();
    assert!(
        code.source
            .find("\"auto_revoke_modified_settings\"")
            .unwrap()
            < protected
    );
    let rpc = code
        .links
        .iter()
        .find(|l| l.label.starts_with("cgct.a("))
        .unwrap();
    assert!(rpc.start > code.source[..protected].chars().count());
    assert!(
        code.source
            .contains("catch (io.grpc.StatusRuntimeException "),
        "{}",
        code.source
    );
    assert!(code.source.contains("new java.io.IOException("));
    assert_eq!(code.source_hash, rdx::engine::source_identity(&code.source));
    for (symbol, visible) in [
        (
            "android.app.backup.BackupDataInput.readNextHeader()Z",
            "readNextHeader",
        ),
        (
            "android.app.backup.BackupDataInput.readEntityData([BII)I",
            "readEntityData",
        ),
        ("java.io.DataInputStream.readLong()J", "readLong"),
    ] {
        let link = code.links.iter().find(|l| l.label == symbol).expect(symbol);
        assert_eq!(
            code.source
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect::<String>(),
            visible
        );
    }
    std::fs::create_dir_all("target/validation").unwrap();
    std::fs::write("target/validation/backup-after-method.java", &code.source).unwrap();
}

#[test]
#[ignore = "Set RDX_TEST_APK to the reported Play Store APK and run --ignored"]
fn backup_keeps_wide_writes_outside_the_grpc_handler() {
    let path = std::env::var_os("RDX_TEST_APK").expect("RDX_TEST_APK required");
    let mut engine = NativeDexEngine::default();
    engine.open(std::path::Path::new(&path)).unwrap();
    let name = "com.google.android.finsky.setup.VendingBackupAgent";
    let class = engine.class(name).unwrap();
    let method = class
        .methods
        .iter()
        .find(|m| m.name.as_ref() == "onBackup")
        .unwrap();
    let code = native_java::render_method(name, class, method).expect("backup Java reconstruction");
    assert!(!code.source.contains(".end method"));
    assert!(code.source.contains("throws java.io.IOException"));
    assert_eq!(code.source.matches("try {").count(), 1);
    let catch = code
        .source
        .find("catch (io.grpc.StatusRuntimeException")
        .unwrap();
    let wrap = code.source.find("new java.io.IOException(").unwrap();
    let write = code.source.find(".writeLong(").unwrap();
    assert!(catch < wrap && wrap < write, "{}", code.source);
    for key in [
        "vending",
        "auto_update_enabled",
        "update_over_wifi_only",
        "auto_add_shortcuts",
        "notify_updates",
        "notify_updates_completion",
        "content-filter-level",
        "verify-apps-consent",
        "auto_revoke_modified_settings",
    ] {
        assert_eq!(
            code.source.matches(&format!("\"{key}\"")).count(),
            1,
            "{key}: {}",
            code.source
        );
    }
    assert_eq!(code.source.matches(".writeLong(").count(), 1);
    assert_eq!(code.source.matches(".writeInt(").count(), 1);
    let target = code
        .links
        .iter()
        .find(|l| l.label == "java.io.DataOutputStream.writeLong(J)V")
        .unwrap();
    assert_eq!(
        code.source
            .chars()
            .skip(target.start)
            .take(target.end - target.start)
            .collect::<String>(),
        "writeLong"
    );
    assert_eq!(code.source_hash, rdx::engine::source_identity(&code.source));
    std::fs::write("target/validation/onbackup-after.java", &code.source).unwrap();
}

#[test]
#[ignore = "Requires RDX_TEST_APK"]
fn click_dispatch_preserves_all_arms() {
    let path = std::env::var_os("RDX_TEST_APK").unwrap();
    let mut engine = NativeDexEngine::default();
    engine.open(std::path::Path::new(&path)).unwrap();
    let class = engine.class("txt").unwrap();
    let method = class
        .methods
        .iter()
        .find(|m| m.name.as_ref() == "onClick")
        .unwrap();
    let code = native_java::render_method("txt", class, method).expect("click reconstruction");
    for call in [
        "wuf.b(",
        ".bj();",
        ".setCurrentItem(",
        ".setResult(",
        ".finish();",
        "new txy(",
        "new wmw(",
        "new bdwf(",
        ".putBoolean(",
    ] {
        assert!(code.source.contains(call), "missing {call}");
    }
    assert_eq!(code.source.matches("wuf.b(").count(), 1);
    assert!(code.source.contains("((byte[]) null)"));
    assert!(!code.links.iter().any(|link| link.label.is_empty()));
    assert_eq!(code.source_hash, rdx::engine::source_identity(&code.source));
    std::fs::write("target/validation/txt-onclick-after.java", &code.source).unwrap();
}

#[test]
#[ignore = "Requires RDX_TEST_APK"]
fn component_activity_callbacks_render_with_effects() {
    let path = std::env::var_os("RDX_TEST_APK").unwrap();
    let mut engine = NativeDexEngine::default();
    engine.open(std::path::Path::new(&path)).unwrap();
    let class = engine.class("pj").unwrap();
    for (name, argc) in [
        ("<init>", 0),
        ("onPanelClosed", 2),
        ("reportFullyDrawn", 0),
        ("onPictureInPictureModeChanged", 2),
        ("onPictureInPictureUiStateChanged", 1),
    ] {
        let method = class
            .methods
            .iter()
            .find(|m| m.name.as_ref() == name && m.parameters.len() == argc)
            .unwrap();
        let code = native_java::render_method("pj", class, method)
            .unwrap_or_else(|e| panic!("{name}: {e:#}"));
        if name == "onPictureInPictureUiStateChanged" {
            assert_eq!(code.source.matches(".isStashed()").count(), 2);
            assert!(code.source.contains("ExternalSyntheticApiModelOutline0.m("));
        }
        std::fs::write(format!("target/validation/pj-{name}.java"), &code.source).unwrap();
    }
}

#[test]
#[ignore = "Requires RDX_TEST_APK"]
fn large_provider_switch_preserves_construction_and_checks() {
    let path = std::env::var_os("RDX_TEST_APK").unwrap();
    let mut engine = NativeDexEngine::default();
    engine.open(std::path::Path::new(&path)).unwrap();
    let class = engine.class("aapj").unwrap();
    let method = class
        .methods
        .iter()
        .find(|m| m.name.as_ref() == "a")
        .unwrap();
    let code =
        native_java::render_method("aapj", class, method).expect("provider switch reconstruction");
    assert!(code.source.contains("switch ("));
    for ty in ["aaqz", "aarf", "aase", "aaqx"] {
        assert!(code.source.contains(&format!("new {ty}(")));
    }
    let decoded = rdx::native_ir::DecodedMethod::decode(method.code.as_ref().unwrap()).unwrap();
    let get_class_calls = decoded
        .instructions
        .iter()
        .filter(|instruction| {
            let Some(reference) = instruction.reference else {
                return false;
            };
            if reference.kind != rdx::native_ir::PoolKind::Method {
                return false;
            }
            let (owner, _, name) = class.symbols.methods[reference.index as usize];
            class.symbols.types[owner as usize].as_ref() == "Ljava/lang/Object;"
                && class.symbols.strings[name as usize] == "getClass"
        })
        .count();
    assert_eq!(code.source.matches(".getClass()").count(), get_class_calls);
    assert_eq!(code.source.matches("case ").count(), 20);
    assert_eq!(code.source_hash, rdx::engine::source_identity(&code.source));
    std::fs::write("target/validation/aapj-after.java", &code.source).unwrap();
    let displayed = engine.render("aapj").unwrap();
    assert!(
        displayed
            .source
            .contains("public final class aapj implements cgns {"),
        "{}",
        displayed.source
    );
    for forbidden in [
        ".class ",
        ".super ",
        ".implements ",
        "java.lang.String",
        "java.lang.Object",
        "java.lang.Boolean",
    ] {
        assert!(
            !displayed.source.contains(forbidden),
            "unexpected {forbidden}"
        );
    }
    assert!(displayed.source.contains("public final Object a()"));
    assert_eq!(
        displayed.source_hash,
        rdx::engine::source_identity(&displayed.source)
    );
    std::fs::write("target/validation/aapj-displayed.java", &displayed.source).unwrap();
}

#[test]
#[ignore = "Requires RDX_TEST_APK"]
fn system_job_service_preserves_existing_java_and_links() {
    let path = std::env::var_os("RDX_TEST_APK").unwrap();
    let mut engine = NativeDexEngine::default();
    engine.open(std::path::Path::new(&path)).unwrap();
    let name = "androidx.work.impl.background.systemjob.SystemJobService";
    let class = engine.class(name).unwrap();
    for method_name in ["<init>", "<clinit>", "c", "a", "onDestroy"] {
        let method = class
            .methods
            .iter()
            .find(|method| method.name.as_ref() == method_name)
            .unwrap();
        let code = native_java::render_method(name, class, method)
            .unwrap_or_else(|error| panic!("{method_name}: {error:#}"));
        assert!(!code.source.contains(".end method"));
        for link in &code.links {
            assert!(link.start < link.end && link.end <= code.source.chars().count());
        }
        if method_name == "onDestroy" {
            assert!(code.source.contains("super.onDestroy()"));
        }
    }
}

#[test]
#[ignore = "Requires RDX_TEST_APK"]
fn system_job_service_monitor_regions_render() {
    let path = std::env::var_os("RDX_TEST_APK").unwrap();
    let mut engine = NativeDexEngine::default();
    engine.open(std::path::Path::new(&path)).unwrap();
    let name = "androidx.work.impl.background.systemjob.SystemJobService";
    let class = engine.class(name).unwrap();
    for method_name in ["onStartJob", "onStopJob", "b", "onCreate"] {
        let method = class
            .methods
            .iter()
            .find(|method| method.name.as_ref() == method_name)
            .unwrap();
        let code = native_java::render_method(name, class, method)
            .unwrap_or_else(|error| panic!("{method_name}: {error:#}"));
        if matches!(method_name, "onStartJob" | "onStopJob") {
            assert_eq!(
                code.source.matches("synchronized (").count(),
                1,
                "{}",
                code.source
            );
        } else {
            assert!(code.source.contains("catch ("), "{}", code.source);
        }
        assert!(!code.source.contains("monitor-exit"));
        std::fs::write(
            format!("target/validation/SystemJobService-{method_name}.java"),
            code.source,
        )
        .unwrap();
    }
}

#[test]
#[ignore = "Requires RDX_TEST_APK"]
fn lmd_overlay_callbacks_keep_discarded_cast_and_synchronized_loop() {
    let path = std::env::var_os("RDX_TEST_APK").unwrap();
    let mut engine = NativeDexEngine::default();
    engine.open(std::path::Path::new(&path)).unwrap();
    let name = "com.google.android.finsky.inlinedetails.lmd.service.LmdOverlayService";
    let class = engine.class(name).unwrap();
    for method_name in ["onCreate", "onDestroy"] {
        let method = class
            .methods
            .iter()
            .find(|m| m.name.as_ref() == method_name)
            .unwrap();
        let code = native_java::render_method(name, class, method).unwrap();
        if method_name == "onCreate" {
            let cast = code.source.find("((yac)").expect("discarded cast retained");
            let construct = code.source.find("new aekl(").unwrap();
            assert!(cast < construct);
            assert!(
                code.links
                    .iter()
                    .any(|link| link.label.starts_with("aekl.<init>("))
            );
        } else {
            let monitor = code.source.find("synchronized (").unwrap();
            let loop_start = code.source.find("while (").unwrap();
            let remove = code.source.find(".remove()").unwrap();
            assert!(monitor < loop_start && loop_start < remove);
            for target in [
                "aekb.d()V",
                "aekb.c()V",
                "aekw.b()V",
                "java.util.Iterator.remove()V",
            ] {
                assert_eq!(
                    code.links
                        .iter()
                        .filter(|link| link.label == target)
                        .count(),
                    1
                );
            }
        }
        for link in &code.links {
            assert!(link.start < link.end && link.end <= code.source.chars().count());
        }
        std::fs::create_dir_all("target/validation/lmd").unwrap();
        std::fs::write(
            format!("target/validation/lmd/{method_name}.java"),
            code.source,
        )
        .unwrap();
    }
}

#[test]
#[ignore = "Requires RDX_TEST_APK"]
fn android_callback_cleanup_and_activity_launch_catches_render() {
    let path = std::env::var_os("RDX_TEST_APK").unwrap();
    let mut engine = NativeDexEngine::default();
    engine.open(std::path::Path::new(&path)).unwrap();
    for (name, method_name) in [
        (
            "com.google.android.finsky.permissionrevocation.UnusedAppRestrictionsBackportService",
            "b",
        ),
        (
            "com.google.android.finsky.applaunch.LaunchAppDeepLinkActivity",
            "x",
        ),
    ] {
        let class = engine.class(name).unwrap();
        let method = class
            .methods
            .iter()
            .find(|m| m.name.as_ref() == method_name)
            .unwrap();
        let code = native_java::render_method(name, class, method).unwrap();
        if method_name == "b" {
            assert_eq!(code.source.matches("finally {").count(), 1);
            assert_eq!(code.source.matches(".recycle()").count(), 1);
            assert!(code.source.contains("catch (android.os.RemoteException"));
            assert!(code.source.contains("? 1 : 0"));
            for target in [
                "android.os.Parcel.recycle()V",
                "android.os.IBinder.transact(ILandroid/os/Parcel;Landroid/os/Parcel;I)Z",
            ] {
                assert!(code.links.iter().any(|link| link.label == target));
            }
        } else {
            assert!(
                code.source
                    .contains("catch (android.content.ActivityNotFoundException")
            );
            assert!(code.source.contains("Activity not found: %s"));
            assert!(code.source.contains("return true;"));
            assert!(code.source.contains("return false;"));
            assert!(code.links.iter().any(|link| {
                link.label
                    .ends_with(".startActivity(Landroid/content/Intent;)V")
            }));
        }
        for link in &code.links {
            assert!(link.start < link.end && link.end <= code.source.chars().count());
        }
        std::fs::create_dir_all("target/validation/nested-cleanup").unwrap();
        std::fs::write(
            format!("target/validation/nested-cleanup/{method_name}.java"),
            code.source,
        )
        .unwrap();
    }
}
