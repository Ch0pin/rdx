use rdx::engine::{DecompilerEngine, NativeEngine};
use serde_json::json;
use std::{collections::BTreeSet, sync::atomic::AtomicBool, time::Instant};

#[test]
#[ignore = "requires RDX_IDENTIFIER_APK and RDX_IDENTIFIER_OUTPUT"]
fn large_apk_scoped_identifiers_match_independent_invoke_scan() {
    let apk = std::env::var_os("RDX_IDENTIFIER_APK").unwrap();
    let output = std::path::PathBuf::from(std::env::var_os("RDX_IDENTIFIER_OUTPUT").unwrap());
    let mut engine = NativeEngine::default();
    let project = engine.open(std::path::Path::new(&apk)).unwrap();
    let cancel = AtomicBool::new(false);
    let prefix = "com.mercadolibre.android.mlwebkit";
    let args = json!({"query":"getAction","package_prefix":prefix,"limit":500});
    let start = Instant::now();
    let reported = engine.search_identifiers(&args, &cancel).unwrap();
    let reported_ms = start.elapsed().as_millis();
    assert_eq!(reported["execution_mode"], "streaming");
    assert_eq!(reported["coverage_complete"], true);
    assert_eq!(reported["skipped_classes"], 0);
    let args = json!({"query":"getAction","package_prefix":prefix,"symbol_kind":"method","occurrence":"reference","match_mode":"exact","limit":500});
    let exact = engine.search_identifiers(&args, &cancel).unwrap();
    assert!(exact["total"].as_u64().unwrap() > 0);
    assert!(exact["next_offset"].is_null());
    let mut expected = BTreeSet::new();
    for name in &project.classes {
        if !name
            .strip_prefix(prefix)
            .is_some_and(|s| s.starts_with('.'))
        {
            continue;
        }
        let class = engine.dex_class(name).unwrap();
        for site in rdx::native_xrefs::call_sites(class, None, &cancel).unwrap() {
            if site
                .callee
                .split_once('(')
                .unwrap()
                .0
                .ends_with(".getAction")
            {
                expected.insert((site.caller, site.callee, site.pc as u64));
            }
        }
    }
    let actual: BTreeSet<_> = exact["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            (
                item["method_id"].as_str().unwrap().to_owned(),
                item["symbol_id"].as_str().unwrap().to_owned(),
                item["code_unit_offset"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(actual, expected);
    let mut paged = vec![];
    let mut args = args;
    args["limit"] = json!(2);
    args["offset"] = json!(0);
    loop {
        let page = engine.search_identifiers(&args, &cancel).unwrap();
        if page["coverage_complete"] == true {
            assert_eq!(page["total"], exact["total"]);
        } else {
            assert!(page["total"].is_null());
        }
        paged.extend(page["items"].as_array().unwrap().iter().cloned());
        if page["next_cursor"].is_null() {
            break;
        }
        args["resume_cursor"] = page["next_cursor"].clone();
    }
    assert_eq!(paged, *exact["items"].as_array().unwrap());
    std::fs::create_dir_all(&output).unwrap();
    let report = json!({"classes":project.classes.len(),"reported_query_ms":reported_ms,"reported_query":reported,"exact_method_references":exact,"independent_invoke_matches":expected.len(),"pagination_verified":true});
    std::fs::write(
        output.join("scoped-identifiers.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    eprintln!(
        "classes={}, scoped query={}ms, reported matches={}, exact invoke matches={}",
        project.classes.len(),
        reported_ms,
        report["reported_query"]["total"],
        expected.len()
    );
}
