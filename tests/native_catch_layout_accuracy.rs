//! Regressions for protected exits and handlers interleaved in bytecode order.
use rdx::engine::{DecompilerEngine, NativeEngine};

#[test]
#[ignore = "Set RDX_TEST_APK to the reported Zoom APK and run --ignored"]
fn market_notice_interleaved_json_handler_keeps_catch_and_navigation() {
    let path = std::env::var_os("RDX_TEST_APK").expect("RDX_TEST_APK required");
    let mut engine = NativeEngine::start().unwrap();
    engine.open(std::path::Path::new(&path)).unwrap();
    let code = engine
        .decompile_with_metadata("com.zipow.videobox.fragment.marketnotice.f")
        .unwrap();
    let definition = code
        .definitions
        .iter()
        .find(|d| d.kind == "method" && d.name == "h7")
        .unwrap();
    let end = code
        .definitions
        .iter()
        .filter(|d| d.kind == "method" && d.start > definition.start)
        .map(|d| d.start)
        .min()
        .unwrap_or(code.source.chars().count());
    let body: String = code
        .source
        .chars()
        .skip(definition.start)
        .take(end - definition.start)
        .collect();
    assert!(!body.contains(".end method"), "{body}");
    assert_eq!(body.matches("catch (JSONException ").count(), 1, "{body}");
    let catch = body.find("catch (JSONException ").unwrap();
    let log = body.find("q1.i(").expect("catch logging preserved");
    assert!(catch < log, "{body}");
    assert_eq!(body.matches("q1.i(").count(), 1, "{body}");
    assert!(body[..catch].contains("new JSONObject("), "{body}");
    let link = code
        .links
        .iter()
        .find(|l| {
            l.start > definition.start
                && l.start < end
                && l.label == "us.zoom.libtools.utils.q1.i(Ljava/lang/Throwable;)V"
        })
        .unwrap();
    assert_eq!(
        code.source
            .chars()
            .skip(link.start)
            .take(link.end - link.start)
            .collect::<String>(),
        "i"
    );
}
