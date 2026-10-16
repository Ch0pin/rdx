//! Best-effort bitmap, vector and adaptive launcher artwork.
mod drawable;
use crate::{package_container::PackageContainer, resource_table::ResourceTable};
use image::{ImageReader, Limits, RgbaImage};
use std::{collections::HashSet, io::Cursor};

/// Uses the same selected APK/split resource set as the decompiler. Call off the UI thread.
pub fn load(container: &PackageContainer, table: &ResourceTable) -> Option<RgbaImage> {
    let bytes = container
        .read_path("AndroidManifest.xml", 2 * 1024 * 1024)
        .ok()?;
    let xml = crate::native_resources::decode(&bytes).ok()?;
    let (package, icons) = crate::manifest_summary::application_icons(&xml)?;
    let mut budget = 64;
    for icon in icons {
        if let Some(image) =
            drawable::load_reference(&icon, &package, table, container, 0, &mut budget)
        {
            return Some(
                image::DynamicImage::ImageRgba8(image)
                    .resize(48, 48, image::imageops::FilterType::Lanczos3)
                    .to_rgba8(),
            );
        }
    }
    None
}

fn decode(bytes: &[u8]) -> Option<RgbaImage> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(2048);
    limits.max_image_height = Some(2048);
    limits.max_alloc = Some(32 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().ok()?;
    if image.width() == 0 || image.height() == 0 {
        return None;
    }
    Some(
        image
            .resize(48, 48, image::imageops::FilterType::Lanczos3)
            .to_rgba8(),
    )
}

fn resource_id(raw: &str, package: &str, table: &ResourceTable) -> Option<u32> {
    let reference = raw.strip_prefix('@')?;
    u32::from_str_radix(reference.trim_start_matches("0x"), 16)
        .ok()
        .or_else(|| {
            let (kind, name) = reference.rsplit_once('/')?;
            let (owner, kind) = kind.split_once(':').unwrap_or((package, kind));
            table
                .entries
                .values()
                .find(|r| r.package == owner && r.kind == kind && r.name == name)
                .map(|r| r.id)
        })
}

fn candidates(raw: &str, package: &str, table: &ResourceTable) -> Vec<String> {
    let id = resource_id(raw, package, table);
    let mut pending: Vec<u32> = id.into_iter().collect();
    let mut seen = HashSet::new();
    let mut paths = Vec::new();
    // Bound alias traversal and candidate decoding even for malformed resource tables.
    while let Some(id) = pending.pop() {
        if seen.len() >= 32 || paths.len() >= 32 {
            break;
        }
        if !seen.insert(id) {
            continue;
        }
        let Some(resource) = table.entries.get(&id) else {
            continue;
        };
        for variant in resource.variants.iter().take(64) {
            if let Some(path) = &variant.file {
                let extension = path
                    .rsplit('.')
                    .next()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if matches!(
                    extension.as_str(),
                    "png" | "webp" | "jpg" | "jpeg" | "gif" | "bmp" | "xml"
                ) && !paths.contains(path)
                {
                    paths.push(path.clone());
                    if paths.len() >= 32 {
                        break;
                    }
                }
            }
            if let Some(next) = variant.reference {
                pending.push(next);
            }
        }
    }
    paths.sort_by_key(|path| path.ends_with(".xml"));
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource_table::{Resource, Variant};

    #[test]
    fn app_icon_resolves_aliases_and_split_artwork_without_cycles() {
        let mut table = ResourceTable::default();
        let variant = |reference, file: Option<&str>| Variant {
            configuration: "default".into(),
            value: String::new(),
            reference,
            file: file.map(str::to_owned),
        };
        for (id, name, variants) in [
            (
                0x7f010001,
                "launcher",
                vec![variant(Some(0x7f010002), None)],
            ),
            (
                0x7f010002,
                "art",
                vec![
                    variant(Some(0x7f010001), None),
                    variant(None, Some("res/mipmap-anydpi-v26/icon.xml")),
                    variant(None, Some("splits/density/res/mipmap/icon.png")),
                ],
            ),
        ] {
            table.entries.insert(
                id,
                Resource {
                    id,
                    package: "example.app".into(),
                    kind: "mipmap".into(),
                    name: name.into(),
                    variants,
                },
            );
        }
        for reference in [
            "@7F010001",
            "@0x7f010001",
            "@mipmap/launcher",
            "@example.app:mipmap/launcher",
        ] {
            assert_eq!(
                candidates(reference, "example.app", &table),
                [
                    "splits/density/res/mipmap/icon.png",
                    "res/mipmap-anydpi-v26/icon.xml"
                ]
            );
        }
        assert!(candidates("@mipmap/missing", "example.app", &table).is_empty());
    }

    #[test]
    fn app_icon_uses_application_android_attributes_only() {
        let xml = r#"<manifest xmlns:a="http://schemas.android.com/apk/res/android" package="example.app"><application icon="wrong" a:icon="@mipmap/main" a:roundIcon="@mipmap/round"><activity a:icon="@mipmap/other"/></application></manifest>"#;
        let (package, icons) = crate::manifest_summary::application_icons(xml).unwrap();
        assert_eq!(package, "example.app");
        assert_eq!(icons, ["@mipmap/main", "@mipmap/round"]);
        assert!(crate::manifest_summary::application_icons("broken").is_none());
    }

    #[test]
    fn app_icon_decoding_preserves_aspect_ratio_and_rejects_corruption() {
        let image = RgbaImage::from_pixel(96, 48, image::Rgba([10, 20, 30, 255]));
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        let decoded = decode(bytes.get_ref()).unwrap();
        assert_eq!(decoded.dimensions(), (48, 24));
        assert_eq!(decoded.get_pixel(0, 0).0, [10, 20, 30, 255]);
        assert!(decode(b"corrupt").is_none());
    }
}
