//! Opt-in regression against the reported APK; no Java tools are used.
use rdx::engine::{DecompilerEngine, NativeEngine};

#[test]
#[ignore = "Set RDX_TEST_APK to the reported Zoom APK and run --ignored"]
fn pbx_path_and_items_emit_java_with_exact_links() {
    let path = std::env::var_os("RDX_TEST_APK").expect("RDX_TEST_APK required");
    let name = "com.zipow.videobox.view.ptvideo.PBXVideoRecordActivity";
    let mut engine = NativeEngine::start().unwrap();
    engine.open(std::path::Path::new(&path)).unwrap();
    let code = engine.decompile_with_metadata(name).unwrap();
    for method in ["getRecordPath", "initItems", "onBtnSwitchClick"] {
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
        if method == "getRecordPath" {
            assert_eq!(body.matches(".append(").count(), 2, "{body}");
            assert!(body.contains(".mkdir()"), "{body}");
            assert!(body.contains("return null;"), "{body}");
            assert!(body.contains("\"Record_Video\""));
            assert!(body.contains("\".mp4\""));
            let link = code
                .links
                .iter()
                .find(|l| {
                    l.start > definition.start
                        && l.start < end
                        && l.label == format!("{name}.mRecordPath:Ljava/lang/String;")
                })
                .unwrap();
            let symbol = engine
                .resolve_usage_target(name, link.start, &code.source_hash)
                .unwrap();
            assert_eq!(symbol.id, link.label);
        } else if method == "initItems" {
            assert_eq!(body.matches(".getString(").count(), 4, "{body}");
            assert_eq!(body.matches(".getDrawable(").count(), 4, "{body}");
            let ctor = "us.zoom.zphone.zpbx.ui.SipInCallPanelView$d.<init>(ILjava/lang/String;Landroid/graphics/drawable/Drawable;)V";
            assert_eq!(
                code.links
                    .iter()
                    .filter(|l| l.start > definition.start && l.start < end && l.label == ctor)
                    .count(),
                4
            );
        } else {
            assert!(body.contains("while (true) {"), "{body}");
            assert_eq!(body.matches(".hasNext()").count(), 1, "{body}");
            assert_eq!(body.matches(".next()").count(), 1, "{body}");
            assert_eq!(body.matches(".setCameraId(").count(), 1, "{body}");
            assert_eq!(body.matches(".updateButtons()").count(), 1, "{body}");
            assert_eq!(body.matches("break;").count(), 2, "{body}");
            let loop_line = body
                .lines()
                .find(|line| line.contains("while (true)"))
                .unwrap();
            let update_line = body
                .lines()
                .find(|line| line.contains(".updateButtons()"))
                .unwrap();
            assert_eq!(
                loop_line.len() - loop_line.trim_start().len(),
                update_line.len() - update_line.trim_start().len(),
                "common update must be outside the loop: {body}"
            );
            assert!(body.find("continue;").unwrap() < body.find(".setCameraId(").unwrap());
            let link = code
                .links
                .iter()
                .find(|l| {
                    l.start > definition.start
                        && l.start < end
                        && l.label == format!("{name}.updateButtons()V")
                })
                .unwrap();
            let destination = engine
                .navigate(name, link.start, &code.source_hash)
                .unwrap();
            assert_eq!(destination.class, name);
            assert_eq!(destination.code.source_hash, code.source_hash);
        }
    }
}
