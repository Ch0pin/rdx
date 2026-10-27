//! Optional real-APK regression; the input stays outside the repository.
use rdx::{
    engine::{MethodSourceFormat, NativeEngine},
    package_container::PackageContainer,
};

#[test]
#[ignore = "requires RDX_HEY_APK and RDX_HEY_OUTPUT"]
fn hey_launcher_and_navigator() {
    let apk = std::path::PathBuf::from(std::env::var_os("RDX_HEY_APK").unwrap());
    let out = std::path::PathBuf::from(std::env::var_os("RDX_HEY_OUTPUT").unwrap());
    std::fs::create_dir_all(&out).unwrap();
    let container = PackageContainer::open(&apk).unwrap();
    let mut engine = NativeEngine::start().unwrap();
    engine.open_prepared(container.clone()).unwrap();
    for path in [
        "AndroidManifest.xml",
        "res/mipmap-anydpi-v26/ic_launcher.xml",
        "res/drawable/ic_launcher_background.xml",
        "res/drawable/$ic_launcher_background__0.xml",
        "res/drawable/ic_launcher_foreground.xml",
        "res/drawable/ic_launcher_monochrome.xml",
    ] {
        let xml = engine.read_resource_with_metadata(path).unwrap().source;
        std::fs::write(out.join(path.replace('/', "_")), xml).unwrap();
    }
    let id = "com.basecamp.heyshared.feature.login.ui.StartActivity.navigatorConfigurations()Ljava/util/List;";
    let method = engine
        .method_source(id, MethodSourceFormat::Auto, &false.into())
        .unwrap();
    std::fs::write(out.join("navigator.java"), &method.code.source).unwrap();
    let class = engine
        .decompile_with_metadata("com.basecamp.heyshared.feature.login.ui.StartActivity")
        .unwrap();
    std::fs::write(out.join("StartActivity.java"), &class.source).unwrap();
    assert!(class.source.contains(" navigatorConfigurations()"));
    assert!(!class.source.contains(&format!(".method {id}")));
    let icon = rdx::app_icon::load(&container, engine.resource_table()).expect("HEY launcher icon");
    assert_eq!(icon.dimensions(), (48, 48));
    icon.save(out.join("icon.png")).unwrap();
    assert_eq!(method.format, "java", "{:?}", method.fallback_reason);
    assert!(method.fallback_reason.is_none());
    for expected in [
        "add_identity",
        "/native/login/password",
        "/native/start/clear",
        "start_nav_host",
        "getValue()",
        "listOf(",
    ] {
        assert!(method.code.source.contains(expected), "missing {expected}");
    }
}
