//! Opt-in checks for allocation expressions reported in the Play Store APK.
use rdx::engine::{DecompilerEngine, NativeEngine};

#[test]
#[ignore = "Set RDX_TEST_APK to the reported Play Store APK and run --ignored"]
fn allocation_cast_renders_and_reordered_exception_message_remains_explicit() {
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
        let expected = if method == "e" {
            assert!(!body.contains(".end method"), "{method}: {body}");
            assert!(body.contains("aeey"), "runtime cast must remain: {body}");
            assert_eq!(body.matches("new rjf(").count(), 1, "{body}");
            vec!["rjf.<init>(Laefa;)V"]
        } else {
            // Java concat evaluates its receiver first; folding the prefix
            // literal here would move resolution before the two DEX calls.
            // Keep the original instructions until ordered sequence lowering exists.
            assert!(body.contains(".end method"), "{body}");
            assert_eq!(
                body.matches("java.lang.String.valueOf(Ljava/lang/Object;)")
                    .count(),
                2,
                "{body}"
            );
            let second_call = body
                .rfind("java.lang.String.valueOf(Ljava/lang/Object;)")
                .unwrap();
            let literal = body.find("Unknown view clicked: ").unwrap();
            let concat = body.find("java.lang.String.concat(").unwrap();
            assert!(second_call < literal && literal < concat, "{body}");
            continue;
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
