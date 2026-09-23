use rdx::{engine::DecompilerEngine, native_engine::NativeDexEngine, native_java};

#[test]
#[ignore = "Set RDX_ZOOM_APK to the Zoom APK"]
fn reported_join_url_patterns_reconstruct_and_keep_call_links() {
    let mut engine = NativeDexEngine::default();
    engine
        .open(std::path::Path::new(
            &std::env::var_os("RDX_ZOOM_APK").unwrap(),
        ))
        .unwrap();
    let name = "com.zipow.videobox.JoinByURLActivity";
    let class = engine.class(name).unwrap();
    for method_name in [
        "onResume",
        "<clinit>",
        "handleClientURIWithExtHandler",
        "isExternalLinkOpened",
    ] {
        let method = class
            .methods
            .iter()
            .find(|m| m.name.as_ref() == method_name)
            .unwrap();
        let result = native_java::render_method(name, class, method)
            .unwrap_or_else(|e| panic!("{method_name}: {e:#}"));
        println!("{}\n{}", method_name, result.source);
        assert!(!result.source.contains(".method "));
        if method_name == "<clinit>" {
            assert!(result.source.contains("new java.lang.String[]"));
            assert_eq!(result.source.matches("java.util.Arrays.asList").count(), 1);
            assert_eq!(result.source.matches("new java.util.HashSet").count(), 1);
            for word in ["docs", "paper", "sheet", "present"] {
                assert_eq!(result.source.matches(&format!("\"{word}\"")).count(), 1);
            }
        } else if method_name == "onResume" {
            assert!(result.source.contains("catch (java.lang.RuntimeException"));
            assert!(result.source.contains("catch (java.lang.Exception"));
        } else {
            assert!(result.source.contains("catch ("));
            assert!(result.source.contains("return false;"));
        }
        assert!(!result.links.is_empty());
        for link in result.links {
            assert!(link.start < link.end && link.end <= result.source.chars().count());
        }
    }
}

#[test]
#[ignore = "Set RDX_ZOOM_APK to the Zoom APK"]
fn contacts_service_methods_reconstruct() {
    let mut engine = NativeDexEngine::default();
    engine
        .open(std::path::Path::new(
            &std::env::var_os("RDX_ZOOM_APK").unwrap(),
        ))
        .unwrap();
    let name = "com.zipow.videobox.ZmContactsServiceImpl";
    let class = engine.class(name).unwrap();
    for method in &class.methods {
        let result = native_java::render_method(name, class, method)
            .unwrap_or_else(|e| panic!("{}: {e:#}", method.name));
        if method.name.as_ref() == "onReceivedCall" {
            let source = &result.source;
            assert_eq!(source.matches(".parseFrom(").count(), 1);
            let catch = source
                .find("catch (com.google.protobuf.InvalidProtocolBufferException")
                .unwrap();
            let continuation = source.find(".getMeetingNumber()").unwrap();
            assert!(source.find(".parseFrom(").unwrap() < catch && catch < continuation);
            assert!(source[catch..continuation].contains("return;"));
            assert_eq!(source.matches(".onConfInvitation(").count(), 1);
            println!("{}", source);
        }
    }
}

#[test]
#[ignore = "Set RDX_ZOOM_APK to the Zoom APK"]
fn share_activity_resume_reconstructs() {
    let mut engine = NativeDexEngine::default();
    engine
        .open(std::path::Path::new(
            &std::env::var_os("RDX_ZOOM_APK").unwrap(),
        ))
        .unwrap();
    let name = "com.zipow.videobox.MMShareActivity";
    let class = engine.class(name).unwrap();
    let method = class
        .methods
        .iter()
        .find(|m| m.name.as_ref() == "onResume")
        .unwrap();
    let result = native_java::render_method(name, class, method).unwrap();
    assert!(result.source.contains("while ("));
    assert!(result.source.contains(".checkFileSize("));
    println!("{}", result.source);
}
