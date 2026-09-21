//! Category regression for ancestor-owner invoke-super calls in the reported APK.
use rdx::engine::{DecompilerEngine, NativeEngine};

#[test]
#[ignore = "Set RDX_TEST_APK to the reported Zoom APK and run --ignored"]
fn meeting_comment_ancestor_callbacks_render_and_keep_exact_links() {
    let path = std::env::var_os("RDX_TEST_APK").expect("RDX_TEST_APK required");
    let name = "us.zoom.zmeetingmsg.MeetingCommentActivity";
    let mut engine = NativeEngine::start().unwrap();
    engine.open(std::path::Path::new(&path)).unwrap();
    let code = engine.decompile_with_metadata(name).unwrap();
    for method in [
        "attachBaseContext",
        "onActivityResult",
        "onBackPressed",
        "onSaveInstanceState",
        "onStart",
        "onStop",
        "onDestroy",
        "onPause",
        "onRestoreInstanceState",
        "onResume",
    ] {
        let definition = code
            .definitions
            .iter()
            .find(|d| d.kind == "method" && d.name == method)
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
        assert!(!body.contains(".end method"), "{method}: {body}");
        assert_eq!(
            body.matches(&format!("super.{method}(")).count(),
            1,
            "{body}"
        );
        let owner = if method == "onActivityResult" {
            "androidx.fragment.app.FragmentActivity"
        } else {
            "us.zoom.uicommon.activity.ZMActivity"
        };
        let link = code
            .links
            .iter()
            .find(|l| {
                l.start > definition.start
                    && l.start < end
                    && l.label.starts_with(&format!("{owner}.{method}("))
            })
            .unwrap();
        assert_eq!(
            code.source
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect::<String>(),
            method
        );
        let symbol = engine
            .resolve_usage_target(name, link.start, &code.source_hash)
            .unwrap();
        assert_eq!(symbol.id, link.label);
        if method == "attachBaseContext" {
            assert!(body.find(".setLocalNightMode(").unwrap() < body.find("super.").unwrap());
        } else if body.contains(".mHasContextSession") {
            assert!(body.find("super.").unwrap() < body.find(".mHasContextSession").unwrap());
        }
    }
}
