//! Opt-in bounded cold renderer profiling; never runs as part of regular tests.
use rdx::{engine::DecompilerEngine, native_engine::NativeDexEngine};
use std::{io::Write, time::Instant};
#[test]
#[ignore = "Set RDX_TEST_APK; cold profile stops admitting classes after45s"]
fn cold_renderer_class_trace() {
    let path = std::env::var_os("RDX_TEST_APK").expect("RDX_TEST_APK required");
    let mut engine = NativeDexEngine::default();
    let project = engine.open(std::path::Path::new(&path)).unwrap();
    let offset = std::env::var("RDX_PROFILE_OFFSET")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0usize);
    let standard = [
        "android.",
        "androidx.",
        "com.android.",
        "kotlin.",
        "kotlinx.",
        "java.",
        "javax.",
        "jdk.",
        "sun.",
        "com.sun.",
        "dalvik.",
        "libcore.",
    ];
    let classes: Vec<_> = project
        .classes
        .iter()
        .filter(|name| !standard.iter().any(|root| name.starts_with(root)))
        .collect();
    let limit = std::env::var("RDX_PROFILE_LIMIT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(usize::MAX);
    let start = Instant::now();
    let mut scanned = 0;
    let mut slow = Vec::new();
    for (index, name) in classes.iter().enumerate().skip(offset) {
        if start.elapsed().as_secs() >= 45 || scanned >= limit {
            break;
        }
        println!("START\t{index}\t{name}");
        std::io::stdout().flush().unwrap();
        let before = Instant::now();
        let code = engine.render(name);
        let elapsed = before.elapsed().as_micros();
        println!(
            "DONE\t{index}\t{name}\t{elapsed}\t{}",
            code.as_ref().map(|c| c.source.len()).unwrap_or(0)
        );
        std::io::stdout().flush().unwrap();
        if elapsed >= 20_000 {
            slow.push((elapsed, index, name));
        }
        scanned += 1;
    }
    slow.sort_unstable_by(|a, b| b.cmp(a));
    println!(
        "SUMMARY\t{}",
        serde_json::json!({"scanned":scanned,"start_offset":offset,"total_classes":classes.len(),"elapsed_ms":start.elapsed().as_millis(),"slowest":slow.into_iter().take(20).collect::<Vec<_>>()})
    );
}
