//! Opt-in checks for allocation expressions reported in the Play Store APK.
use rdx::engine::{DecompilerEngine, NativeEngine};

#[test]
#[ignore = "Set RDX_TEST_APK to the reported Play Store APK and run --ignored"]
fn allocation_casts_and_messages_render_with_ordered_temporary_values() {
    let path = std::env::var_os("RDX_TEST_APK").expect("RDX_TEST_APK required");
    let mut engine = NativeEngine::start().unwrap();
    engine.open(std::path::Path::new(&path)).unwrap();
    for (class, method) in [
        (
            "com.google.android.finsky.application.classic.ClassicApplication",
            "e",
        ),
        (
            "com.google.android.finsky.billing.subscription.SubscriptionAskToPauseActivity",
            "onClick",
        ),
        (
            "com.google.android.finsky.screenshotsactivity.ScreenshotsActivityV2",
            "x",
        ),
    ] {
        let code = engine.decompile_with_metadata(class).unwrap();
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
        if method == "x" {
            let checks: Vec<_> = body
                .lines()
                .filter(|line| line.contains("= ((bdzh)") || line.contains("= ((wrt)"))
                .collect();
            assert_eq!(checks.len(), 4, "{body}");
            for pair in checks.as_chunks::<2>().0 {
                assert!(pair[0].contains("bdzh"));
                assert!(pair[1].contains("wrt"));
            }
            for (allocation, constructor) in [
                (
                    "asxl",
                    "asxl.<init>(Lonm;Ljava/util/List;Lbvwx;Lbomy;Lwrt;Lbdzh;Ljava/util/Map;ZLanau;)V",
                ),
                (
                    "asxm",
                    "asxm.<init>(Lonm;Ljava/util/List;Lbvwx;Lbomy;Lwrt;Lbdzh;Lanau;)V",
                ),
            ] {
                assert_eq!(
                    body.matches(&format!("new {allocation}(")).count(),
                    1,
                    "{body}"
                );
                let link = code
                    .links
                    .iter()
                    .find(|l| l.start > definition.start && l.start < end && l.label == constructor)
                    .expect("constructor navigation");
                assert_eq!(
                    engine
                        .resolve_usage_target(class, link.start, &code.source_hash)
                        .unwrap()
                        .id,
                    constructor
                );
            }
            for target in ["bdzh", "wrt"] {
                assert!(
                    code.links
                        .iter()
                        .filter(|l| l.start > definition.start
                            && l.start < end
                            && l.label == target)
                        .count()
                        >= 2
                );
            }
            continue;
        }
        let expected = if method == "e" {
            assert!(!body.contains(".end method"), "{method}: {body}");
            assert!(body.contains("aeey"), "runtime cast must remain: {body}");
            assert_eq!(body.matches("new rjf(").count(), 1, "{body}");
            vec!["rjf.<init>(Laefa;)V"]
        } else {
            assert_eq!(body.matches("String.valueOf(").count(), 2, "{body}");
            assert_eq!(body.matches(".concat(").count(), 1, "{body}");
            let second_call = body.rfind("String.valueOf(").unwrap();
            let literal = body.find("Unknown view clicked: ").unwrap();
            let concat = body.find(".concat(").unwrap();
            let allocation = body.find("new IllegalArgumentException(").unwrap();
            assert!(
                second_call < literal && literal < concat && concat < allocation,
                "{body}"
            );
            vec![
                "java.lang.IllegalArgumentException.<init>(Ljava/lang/String;)V",
                "java.lang.String.concat(Ljava/lang/String;)Ljava/lang/String;",
            ]
        };
        for raw in expected {
            let link = code
                .links
                .iter()
                .find(|l| l.start > definition.start && l.start < end && l.label == raw)
                .expect("original method link");
            assert_eq!(
                engine
                    .resolve_usage_target(class, link.start, &code.source_hash)
                    .unwrap()
                    .id,
                raw
            );
        }
    }
}

#[test]
#[ignore = "Set RDX_TEST_APK to the reported Play Store APK and run --ignored"]
fn app_discovery_readability_preserves_call_order_and_navigation() {
    let path = std::env::var_os("RDX_TEST_APK").expect("RDX_TEST_APK required");
    let mut engine = NativeEngine::start().unwrap();
    engine.open(std::path::Path::new(&path)).unwrap();
    let class = "com.google.android.finsky.appdiscoveryservice.AppDiscoveryLaunchActivity";
    let code = engine.decompile_with_metadata(class).unwrap();
    assert!(!code.source.contains(".end method"), "{}", code.source);
    assert!(code.source.contains("if (this.a.e())"), "{}", code.source);
    assert!(code.source.contains("if (v6 == null)"));
    assert!(code.source.contains("if (v8 == null)"));
    assert!(!code.source.contains("!= false"));
    assert!(!code.source.contains("== false"));
    assert!(!code.source.contains("((Object) v1)"));
    let first = code.source.find("AppDiscoveryVulnerabilityFix").unwrap();
    let second = code.source.find(".startsWith(").unwrap();
    let branch = code.source.find("if (v11)").unwrap();
    assert!(first < second && second < branch);
    let mut checked = 0;
    for link in &code.links {
        if link.label.contains("aqtw.e(") || link.label.contains("java.lang.String.startsWith(") {
            let visible: String = code
                .source
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect();
            assert!(visible == "e" || visible == "startsWith", "{visible}");
            assert_eq!(
                engine
                    .resolve_usage_target(class, link.start, &code.source_hash)
                    .unwrap()
                    .id,
                link.label
            );
            checked += 1;
        }
    }
    assert!(checked >= 2);
}
