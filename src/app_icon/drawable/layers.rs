//! Bounded layer-list preview in a 1px-per-dp, left-to-right icon canvas.
//! Geometry follows Android LayerDrawable.resolveGravity and Gravity.apply.
use super::*;

pub(super) fn dimension(raw: &str) -> Option<i32> {
    let number = raw
        .strip_suffix("dip")
        .or_else(|| raw.strip_suffix("dp"))
        .or_else(|| raw.strip_suffix("px"))
        .unwrap_or(raw)
        .parse::<f32>()
        .ok()?;
    (number.is_finite() && (0.0..=2048.0).contains(&number)).then(|| number.round() as i32)
}

fn gravity(raw: &str) -> Option<u32> {
    let parsed = if let Some(hex) = raw.strip_prefix("0x") {
        u32::from_str_radix(hex, 16).ok()?
    } else if let Ok(number) = raw.parse::<u32>() {
        number
    } else {
        let mut flags = 0;
        for token in raw.split('|') {
            flags |= match token.trim() {
                "none" | "no_gravity" => 0,
                "left" | "start" => 3,
                "right" | "end" => 5,
                "top" => 0x30,
                "bottom" => 0x50,
                "center_horizontal" => 1,
                "center_vertical" => 0x10,
                "center" => 0x11,
                "fill_horizontal" => 7,
                "fill_vertical" => 0x70,
                "fill" => 0x77,
                "clip_horizontal" => 8,
                "clip_vertical" => 0x80,
                _ => return None,
            };
        }
        flags
    };
    (parsed & !0x0080_00ff == 0).then_some(parsed & 0xff)
}

fn axis(
    low: i32,
    high: i32,
    flags: u32,
    explicit: Option<i32>,
    intrinsic: Option<i32>,
    intrinsic_known: bool,
) -> Option<(i32, u32)> {
    if high < low {
        return None;
    }
    let mut alignment = flags & 7;
    if alignment == 0 {
        alignment = if explicit.is_some() { 3 } else { 7 };
    }
    let size = explicit.or(intrinsic);
    if size.is_none() && intrinsic_known {
        alignment = 7;
    }
    let size = if alignment == 7 { high - low } else { size? };
    if !(0..=512).contains(&size) {
        return None;
    }
    let mut start = match alignment {
        1 => low + (high - low - size) / 2,
        3 | 7 => low,
        5 => high - size,
        _ => return None,
    };
    let mut end = start + size;
    if flags & 8 != 0 {
        start = start.max(low);
        end = end.min(high);
    }
    Some((start, end.saturating_sub(start).max(0) as u32))
}

fn intrinsic(node: &Node, name: &str) -> Option<Option<i32>> {
    let source = match node.tag.as_str() {
        "vector" => Some(node),
        "shape" => node.children.iter().find(|n| n.tag == "size"),
        // Nested layer lists require explicit geometry for non-fill axes.
        _ => return Some(None),
    };
    match source.and_then(|n| n.attr(name)) {
        Some(raw) => dimension(raw).map(Some),
        None => Some(None),
    }
}

pub(super) fn render(
    node: &Node,
    package: &str,
    table: &ResourceTable,
    container: &PackageContainer,
    depth: usize,
    budget: &mut usize,
    bounds: [u32; 2],
) -> Option<RgbaImage> {
    if depth >= 8
        || node.children.is_empty()
        || node.children.len() > 32
        || bounds.iter().any(|n| *n == 0 || *n > 512)
        || node.attrs.iter().any(|(key, value)| {
            key != "paddingMode"
                || !matches!(value.as_str(), "0" | "1" | "0x0" | "0x1" | "nest" | "stack")
        })
    {
        return None;
    }
    let mut canvas = RgbaImage::new(bounds[0], bounds[1]);
    for item in &node.children {
        if *budget == 0 || item.tag != "item" {
            return None;
        }
        *budget -= 1;
        if item.attrs.keys().any(|key| {
            !matches!(
                key.as_str(),
                "id" | "drawable"
                    | "width"
                    | "height"
                    | "gravity"
                    | "left"
                    | "top"
                    | "right"
                    | "bottom"
                    | "start"
                    | "end"
            )
        }) {
            return None;
        }
        let length = |name| match item.attr(name) {
            Some(raw) => dimension(raw).map(Some),
            None => Some(None),
        };
        let left = length("start")?.or(length("left")?).unwrap_or(0);
        let right = length("end")?.or(length("right")?).unwrap_or(0);
        let top = length("top")?.unwrap_or(0);
        let bottom = length("bottom")?.unwrap_or(0);
        let flags = gravity(item.attr("gravity").unwrap_or("0"))?;
        let inline = match (item.attr("drawable"), item.children.as_slice()) {
            (Some(_), []) => None,
            (None, [child]) => Some(child),
            _ => return None,
        };
        let intrinsic_known = inline.is_some_and(|n| matches!(n.tag.as_str(), "shape" | "vector"))
            || item
                .attr("drawable")
                .is_some_and(|raw| color(raw, package, table, 0).is_some());
        let intrinsic_size = |name| match inline {
            Some(child) => intrinsic(child, name),
            None => Some(None),
        };
        let (x, width) = axis(
            left,
            bounds[0] as i32 - right,
            flags,
            length("width")?,
            intrinsic_size("width")?,
            intrinsic_known,
        )?;
        let (y, height) = axis(
            top,
            bounds[1] as i32 - bottom,
            flags >> 4,
            length("height")?,
            intrinsic_size("height")?,
            intrinsic_known,
        )?;
        if width == 0 || height == 0 {
            continue;
        }
        let image = if let Some(child) = inline {
            if child.tag == "layer-list" {
                render(
                    child,
                    package,
                    table,
                    container,
                    depth + 1,
                    budget,
                    [width, height],
                )?
            } else {
                let pixels = render_node(child, package, table, container, depth + 1, budget)?;
                image::imageops::resize(
                    &pixels,
                    width,
                    height,
                    image::imageops::FilterType::Lanczos3,
                )
            }
        } else {
            let pixels = load_reference(
                item.attr("drawable")?,
                package,
                table,
                container,
                depth + 1,
                budget,
            )?;
            image::imageops::resize(
                &pixels,
                width,
                height,
                image::imageops::FilterType::Lanczos3,
            )
        };
        image::imageops::overlay(&mut canvas, &image, i64::from(x), i64::from(y));
    }
    Some(canvas)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_list_preserves_insets_gravity_intrinsic_size_and_order() {
        let container = PackageContainer::open(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.apk"),
        )
        .unwrap();
        let table = ResourceTable::default();
        let xml = r##"<layer-list xmlns:a="http://schemas.android.com/apk/res/android">
            <item a:gravity="0x77" a:width="96dp" a:height="96dp" a:left="22dp" a:top="22dp" a:right="22dp" a:bottom="22dp" a:drawable="#ff0000"/>
            <item a:gravity="bottom" a:height="24dp"><shape><size a:width="10dp" a:height="10dp"/><solid a:color="#00ff00"/></shape></item>
            <item a:gravity="0x51" a:bottom="14dp"><vector a:width="10dp" a:height="10dp" a:viewportWidth="10" a:viewportHeight="10"><path a:fillColor="@0x0106000b" a:pathData="M0,0h10v10H0z"/></vector></item>
        </layer-list>"##;
        let image =
            render_node(&parse(xml).unwrap(), "demo", &table, &container, 0, &mut 64).unwrap();
        assert_eq!(image.get_pixel(0, 0).0, [0; 4]);
        assert_eq!(image.get_pixel(30, 30).0, [255, 0, 0, 255]);
        assert_eq!(image.get_pixel(90, 40).0, [0; 4]);
        assert_eq!(image.get_pixel(5, 90).0, [0, 255, 0, 255]);
        assert_eq!(image.get_pixel(54, 88).0, [255; 4]);
        assert_eq!(image.get_pixel(54, 100).0, [0, 255, 0, 255]);
        assert!(render_node(&parse(xml).unwrap(), "demo", &table, &container, 0, &mut 1).is_none());
        for bad in [
            xml.replace("96dp", "999999dp"),
            xml.replace("96dp", "NaNdp"),
            xml.replace("<solid", "<stroke"),
            xml.replace("bottom\"", "unknown\""),
        ] {
            assert!(
                render_node(
                    &parse(&bad).unwrap(),
                    "demo",
                    &table,
                    &container,
                    0,
                    &mut 64
                )
                .is_none()
            );
        }
    }

    #[test]
    fn layer_geometry_defaults_clips_and_framework_colors_are_bounded() {
        assert_eq!(axis(22, 86, 7, Some(96), None, false), Some((22, 64)));
        assert_eq!(axis(0, 108, 5, Some(24), Some(10), true), Some((84, 24)));
        assert_eq!(axis(0, 108, 0, None, Some(10), true), Some((0, 108)));
        assert_eq!(axis(20, 80, 9, Some(100), None, true), Some((20, 60)));
        assert!(axis(0, 108, 1, None, None, false).is_none());
        assert_eq!(gravity("bottom|center_horizontal"), Some(0x51));
        let table = ResourceTable::default();
        assert_eq!(
            color("@android:color/white", "demo", &table, 0),
            Some([255; 4])
        );
        assert_eq!(
            color("@0x0106000c", "demo", &table, 0),
            Some([0, 0, 0, 255])
        );
        assert!(color("@android:color/arbitrary", "demo", &table, 0).is_none());
    }
}
