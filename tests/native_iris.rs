use rdx::{engine::DecompilerEngine, native_engine::NativeDexEngine, native_java};

#[test]
#[ignore = "Set RDX_IRIS_APK to the IRIS helper APK"]
fn dispatch_receiver_reconstructs() {
    let mut engine = NativeDexEngine::default();
    engine
        .open(std::path::Path::new(
            &std::env::var_os("RDX_IRIS_APK").unwrap(),
        ))
        .unwrap();
    let name = "com.iris.intentmon.DispatchReceiver";
    let class = engine.class(name).unwrap();
    let mut failures = Vec::new();
    for method in &class.methods {
        match native_java::render_method(name, class, method) {
            Ok(result) => {
                println!("{}\n{}", method.name, result.source);
                let expected_catches = match method.name.as_ref() {
                    "onReceive" => 4,
                    "parseRequestId" => 1,
                    _ => 0,
                };
                assert_eq!(result.source.matches("catch (").count(), expected_catches);
                if method.name.as_ref() == "dispatch" {
                    for call in ["startActivity(", "startService(", "sendBroadcast("] {
                        assert_eq!(result.source.matches(call).count(), 1);
                    }
                }
                for link in result.links {
                    assert!(link.start < link.end && link.end <= result.source.chars().count());
                }
            }
            Err(error) => failures.push(format!("{}: {error:#}", method.name)),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    let source = engine.decompile(name).unwrap();
    assert!(!source.contains(".method "), "{source}");
    assert!(source.contains("public class DispatchReceiver"));
}
