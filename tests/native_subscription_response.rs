use rdx::{engine::DecompilerEngine, native_engine::NativeDexEngine, native_java};
#[test]
#[ignore = "Requires RDX_TEST_APK"]
fn subscription_response_reconstructs() {
    let path = std::env::var_os("RDX_TEST_APK").unwrap();
    let mut engine = NativeDexEngine::default();
    engine.open(std::path::Path::new(&path)).unwrap();
    let name = "com.google.android.finsky.billing.updatesubscriptioninstrument.UpdateSubscriptionInstrumentActivity";
    let class = engine.class(name).unwrap();
    let method = class
        .methods
        .iter()
        .find(|m| m.name.as_ref() == "x" && m.parameters.iter().map(|x| x.as_ref()).eq(["Lcbjz;"]))
        .unwrap();
    let code = native_java::render_method(name, class, method).unwrap();
    std::fs::write("/tmp/rdx-subscription-response.java", &code.source).unwrap();
    assert!(!code.source.contains(".method"));
    assert_eq!(code.source.matches("a.ag(").count(), 2);
    assert!(code.source.contains("!= 0 ?"));
    assert_eq!(code.source.matches(".append(").count(), 1);
    assert!(code.source.find(".append(").unwrap() < code.source.find(".toString()").unwrap());
    assert!(
        code.source.find(".toString()").unwrap()
            < code
                .source
                .find("new java.lang.IllegalStateException")
                .unwrap()
    );
    assert!(code.source.contains("this.y(2)"));
    assert!(code.source.contains("this.D("));
    assert!(code.source.contains("show_success"));
    assert!(code.source.contains("this.k(-1)"));
    assert!(code.links.iter().any(|link| link.label == "java.lang.IllegalStateException.<init>(Ljava/lang/String;)V"));
    let rendered = engine.render(name).unwrap();
    assert!(!rendered.source.contains(&format!(".method {name}.x(")));
    std::fs::write("/tmp/rdx-subscription-class.java", &rendered.source).unwrap();
}
