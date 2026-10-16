use rdx::{
    engine::{DecompilerEngine, NativeEngine},
    native_resources::MAX_XML_BYTES,
    package_container::PackageContainer,
    resource_table::{Resource, ResourceTable, Variant},
};
use std::io::{Cursor, Write};

fn package(label: &str, manifest_size: usize) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "rdx-icon-manifest-{label}-{}.apk",
        std::process::id()
    ));
    let mut manifest = String::from(
        r#"<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="test.large"><application android:icon="@7f010001"/></manifest>"#,
    );
    manifest.push_str(&" ".repeat(manifest_size.saturating_sub(manifest.len())));
    let mut png = Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(48, 48, image::Rgba([255, 90, 95, 255]))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    for (name, bytes) in [
        ("AndroidManifest.xml", manifest.as_bytes()),
        ("res/mipmap/icon.png", png.get_ref().as_slice()),
        (
            "classes.dex",
            include_bytes!("fixtures/hello.dex").as_slice(),
        ),
    ] {
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
    path
}

fn table() -> ResourceTable {
    let mut table = ResourceTable::default();
    table.entries.insert(
        0x7f010001,
        Resource {
            id: 0x7f010001,
            package: "test.large".into(),
            kind: "mipmap".into(),
            name: "icon".into(),
            variants: vec![Variant {
                configuration: "default".into(),
                value: "res/mipmap/icon.png".into(),
                file: Some("res/mipmap/icon.png".into()),
                reference: None,
            }],
        },
    );
    table
}

#[test]
fn manifest_over_two_mib_supports_icon_package_name_and_source() {
    let path = package("large", 3 * 1024 * 1024);
    let container = PackageContainer::open(&path).unwrap();
    let icon = rdx::app_icon::load(&container, &table()).expect("large manifest icon");
    assert_eq!(icon.dimensions(), (48, 48));
    assert_eq!(icon.get_pixel(24, 24).0, [255, 90, 95, 255]);
    let mut engine = NativeEngine::start().unwrap();
    engine.open_prepared(container).unwrap();
    let xml = engine.read_resource("AndroidManifest.xml").unwrap();
    assert_eq!(
        rdx::manifest_summary::package_name(&xml).unwrap(),
        "test.large"
    );
    assert_eq!(
        engine
            .read_resource_with_metadata("AndroidManifest.xml")
            .unwrap()
            .source,
        xml
    );
    drop(engine);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn manifest_over_xml_cap_retains_icon_fallback_and_resource_error() {
    let path = package("oversized", MAX_XML_BYTES + 1);
    let container = PackageContainer::open(&path).unwrap();
    assert!(rdx::app_icon::load(&container, &table()).is_none());
    let mut engine = NativeEngine::start().unwrap();
    engine.open_prepared(container).unwrap();
    assert!(
        engine
            .read_resource("AndroidManifest.xml")
            .unwrap_err()
            .to_string()
            .contains("exceeds")
    );
    drop(engine);
    std::fs::remove_file(path).unwrap();
}
