use rdx::{
    call_graph,
    engine::{DecompilerEngine, NativeEngine},
    native_engine::NativeDexEngine,
};
use std::{path::Path, sync::atomic::AtomicBool};
#[test]
fn graph_from_dex_and_exact_navigation() {
    let mut engine = NativeEngine::default();
    engine
        .open(Path::new("tests/fixtures/navigation.apk"))
        .unwrap();
    let source = engine.decompile_with_metadata("sample.Caller").unwrap();
    let offset = source
        .definitions
        .iter()
        .find(|d| d.name == "compute")
        .unwrap()
        .start;
    let cancel = AtomicBool::new(false);
    let graph = engine
        .call_graph_at("sample.Caller", offset, &source.source_hash, 3, &cancel)
        .unwrap();
    assert!(graph.nodes[0].method.starts_with("sample.Caller.compute("));
    let index = graph
        .nodes
        .iter()
        .position(|n| n.method == "sample.Target.doubleValue(I)I")
        .unwrap();
    assert!(graph.edges.iter().any(|e| e.from == 0 && e.to == index));
    let target = engine
        .navigate_graph_method(&graph.nodes[index].method)
        .unwrap();
    assert_eq!(target.class, "sample.Target");
    assert!(
        target
            .code
            .definitions
            .iter()
            .any(|d| d.start == target.position && d.name == "doubleValue")
    );
    assert!(
        engine
            .call_graph_at("sample.Caller", offset, "stale", 3, &cancel)
            .is_err()
    );
    assert!(
        engine
            .call_graph_at("sample.Caller", offset, &source.source_hash, 0, &cancel)
            .is_err()
    );
    assert!(
        engine
            .call_graph_at(
                "sample.Caller",
                offset,
                &source.source_hash,
                3,
                &AtomicBool::new(true)
            )
            .is_err()
    );
}
#[test]
fn graph_depth_bounds_and_external_nodes() {
    let mut engine = NativeDexEngine::default();
    engine
        .open(Path::new("tests/fixtures/navigation.apk"))
        .unwrap();
    let method = engine
        .class("sample.Caller")
        .unwrap()
        .methods
        .iter()
        .find(|m| m.name.as_ref() == "compute")
        .unwrap();
    let id = format!(
        "sample.Caller.compute({}){}",
        method.parameters.join(""),
        method.return_type
    );
    let graph = call_graph::build(&engine, &id, 1, &AtomicBool::new(false)).unwrap();
    let deep = call_graph::build(&engine, &id, 20, &AtomicBool::new(false)).unwrap();
    assert!(deep.nodes.len() >= graph.nodes.len());
    assert!(graph.nodes.iter().all(|n| n.depth <= 1));
    assert!(graph.edges.iter().all(|e| e.from == 0));
    assert!(graph.nodes.len() <= 5000 && graph.edges.len() <= 20000);
}
#[test]
#[ignore = "requires RDX_VENDING_APK"]
fn real_activity_graph_is_bounded_and_classified() {
    let path = std::env::var("RDX_VENDING_APK").unwrap();
    let mut engine = NativeDexEngine::default();
    engine.open(Path::new(&path)).unwrap();
    let graph=call_graph::build(&engine,"com.google.android.finsky.screenshotsactivity.ScreenshotsActivityV2.onCreate(Landroid/os/Bundle;)V",20,&AtomicBool::new(false)).unwrap();
    assert_eq!(
        graph.nodes[0].component,
        Some(call_graph::Component::Activity)
    );
    assert!(
        graph
            .edges
            .iter()
            .any(|e| graph.nodes[e.to].component == Some(call_graph::Component::Activity))
    );
    assert!(graph.nodes.len() <= 5000 && graph.edges.len() <= 20000);
    eprintln!(
        "{} nodes, {} edges, truncated={}",
        graph.nodes.len(),
        graph.edges.len(),
        graph.truncated
    );
}

#[test]
#[ignore = "requires RDX_VENDING_APK"]
fn inherited_webview_callers_are_found() {
    let mut engine = NativeDexEngine::default();
    engine
        .open(Path::new(&std::env::var("RDX_VENDING_APK").unwrap()))
        .unwrap();
    let graph = call_graph::build_direction(
        &engine,
        "bhnh.loadUrl(Ljava/lang/String;)V",
        1,
        "callers",
        &AtomicBool::new(false),
    )
    .unwrap();
    let caller = graph
        .nodes
        .iter()
        .position(|n| n.method == "bhnh.c()V")
        .unwrap();
    assert!(graph.edges.iter().any(|e| e.from == caller && e.to == 0));
    assert!(!graph.nodes[0].available);
}
