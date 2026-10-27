//! Bounded Android drawable preview. Convert supported vectors into an internal SVG;
//! the generated SVG contains paths/groups only, with no external resource loaders.
use super::*;
use quick_xml::{events::Event, name::ResolveResult, reader::NsReader};
use std::collections::BTreeMap;
const ANDROID: &str = "http://schemas.android.com/apk/res/android";
const SIZE: u32 = 108;
#[derive(Default)]
struct Node {
    tag: String,
    attrs: BTreeMap<String, String>,
    children: Vec<Node>,
}
impl Node {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.get(name).map(String::as_str)
    }
    fn number(&self, name: &str, default: f32) -> Option<f32> {
        let value = self
            .attr(name)
            .map(str::parse::<f32>)
            .transpose()
            .ok()?
            .unwrap_or(default);
        (value.is_finite() && value.abs() <= 1_000_000.0).then_some(value)
    }
}
fn parse(xml: &str) -> Option<Node> {
    if xml.len() > 1024 * 1024 {
        return None;
    }
    let mut reader = NsReader::from_str(xml);
    let mut stack: Vec<Node> = Vec::new();
    let mut root = None;
    let mut count = 0;
    loop {
        match reader.read_event().ok()? {
            event @ (Event::Start(_) | Event::Empty(_)) => {
                count += 1;
                if count > 2048 || stack.len() >= 16 {
                    return None;
                }
                let empty = matches!(event, Event::Empty(_));
                let element = match event {
                    Event::Start(e) | Event::Empty(e) => e,
                    _ => unreachable!(),
                };
                let (ns, name) = reader.resolver().resolve_element(element.name());
                if !matches!(ns, ResolveResult::Unbound) {
                    return None;
                }
                let mut node = Node {
                    tag: name.as_ref().to_owned(),
                    ..Default::default()
                };
                for attr in element.attributes() {
                    let attr = attr.ok()?;
                    let (ns, name) = reader.resolver().resolve_attribute(attr.key);
                    if matches!(ns, ResolveResult::Bound(uri) if uri.as_ref() == ANDROID) {
                        node.attrs.insert(
                            name.as_ref().to_owned(),
                            attr.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                                .ok()?
                                .into_owned(),
                        );
                    }
                }
                if empty {
                    if let Some(parent) = stack.last_mut() {
                        parent.children.push(node);
                    } else if root.replace(node).is_some() {
                        return None;
                    }
                } else {
                    stack.push(node);
                }
            }
            Event::End(_) => {
                let node = stack.pop()?;
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(node);
                } else if root.replace(node).is_some() {
                    return None;
                }
            }
            Event::DocType(_) | Event::GeneralRef(_) => return None,
            Event::Eof => return if stack.is_empty() { root } else { None },
            _ => {}
        }
    }
}
fn color(raw: &str, package: &str, table: &ResourceTable, depth: usize) -> Option<[u8; 4]> {
    if depth >= 8 {
        return None;
    }
    if raw.starts_with('@') {
        let resource = table
            .entries
            .get(&super::resource_id(raw, package, table)?)?;
        let variant = resource
            .variants
            .iter()
            .find(|v| v.configuration == "default")
            .or(resource.variants.first())?;
        return if let Some(id) = variant.reference {
            color(&format!("@{id:08x}"), package, table, depth + 1)
        } else {
            color(&variant.value, package, table, depth + 1)
        };
    }
    let hex = raw.strip_prefix('#')?;
    let value = u32::from_str_radix(hex, 16).ok()?;
    Some(match hex.len() {
        3 => [
            ((value >> 8) as u8 & 15) * 17,
            ((value >> 4) as u8 & 15) * 17,
            (value as u8 & 15) * 17,
            255,
        ],
        4 => [
            ((value >> 8) as u8 & 15) * 17,
            ((value >> 4) as u8 & 15) * 17,
            (value as u8 & 15) * 17,
            ((value >> 12) as u8 & 15) * 17,
        ],
        6 => [(value >> 16) as u8, (value >> 8) as u8, value as u8, 255],
        8 => [
            (value >> 16) as u8,
            (value >> 8) as u8,
            value as u8,
            (value >> 24) as u8,
        ],
        _ => return None,
    })
}
struct VectorContext<'a> {
    package: &'a str,
    table: &'a ResourceTable,
    container: Option<&'a PackageContainer>,
    budget: &'a mut usize,
    definitions: String,
    next_id: usize,
}
impl VectorContext<'_> {
    fn id(&mut self) -> String {
        let id = format!("paint{}", self.next_id);
        self.next_id += 1;
        id
    }
    fn paint(&mut self, node: &Node, name: &str) -> Option<String> {
        let raw = node.attr(name).unwrap_or("#00000000");
        if let Some(c) = color(raw, self.package, self.table, 0) {
            return Some(format!("#{:02x}{:02x}{:02x}{:02x}", c[0], c[1], c[2], c[3]));
        }
        let container = self.container?;
        for path in super::candidates(raw, self.package, self.table) {
            if *self.budget == 0 {
                return None;
            }
            *self.budget -= 1;
            let Some(gradient) = container
                .read_path(&path, 1024 * 1024)
                .ok()
                .and_then(|bytes| crate::native_resources::decode(&bytes).ok())
                .and_then(|xml| parse(&xml))
            else {
                continue;
            };
            if let Some(paint) = self.linear_gradient(&gradient) {
                return Some(paint);
            }
        }
        None
    }
    fn linear_gradient(&mut self, node: &Node) -> Option<String> {
        if node.tag != "gradient"
            || !matches!(node.attr("type").unwrap_or("0"), "0" | "0x0" | "linear")
        {
            return None;
        }
        let spread = match node.attr("tileMode").unwrap_or("0") {
            "0" | "0x0" | "clamp" => "pad",
            "1" | "0x1" | "repeat" => "repeat",
            "2" | "0x2" | "mirror" => "reflect",
            _ => return None,
        };
        let x1 = node.number("startX", 0.0)?;
        let y1 = node.number("startY", 0.0)?;
        let x2 = node.number("endX", 0.0)?;
        let y2 = node.number("endY", 0.0)?;
        if x1 == x2 && y1 == y2 {
            return None;
        }
        let mut stops = Vec::new();
        if node.children.is_empty() {
            stops.push((
                0.0,
                color(node.attr("startColor")?, self.package, self.table, 0)?,
            ));
            if let Some(center) = node.attr("centerColor") {
                stops.push((0.5, color(center, self.package, self.table, 0)?));
            }
            stops.push((
                1.0,
                color(node.attr("endColor")?, self.package, self.table, 0)?,
            ));
        } else {
            for item in &node.children {
                if item.tag != "item" || !item.children.is_empty() || item.attr("offset").is_none()
                {
                    return None;
                }
                let offset = item.number("offset", 0.0)?;
                if !(0.0..=1.0).contains(&offset)
                    || stops.last().is_some_and(|(previous, _)| offset < *previous)
                {
                    return None;
                }
                stops.push((
                    offset,
                    color(item.attr("color")?, self.package, self.table, 0)?,
                ));
            }
        }
        if stops.len() < 2 {
            return None;
        }
        let id = self.id();
        self.definitions.push_str(&format!("<linearGradient id=\"{id}\" gradientUnits=\"userSpaceOnUse\" x1=\"{x1}\" y1=\"{y1}\" x2=\"{x2}\" y2=\"{y2}\" spreadMethod=\"{spread}\">"));
        for (offset, c) in stops {
            self.definitions.push_str(&format!("<stop offset=\"{offset}\" stop-color=\"#{:02x}{:02x}{:02x}\" stop-opacity=\"{}\"/>", c[0], c[1], c[2], c[3] as f32 / 255.0));
        }
        self.definitions.push_str("</linearGradient>");
        Some(format!("url(#{id})"))
    }
}

// Android clips subsequent siblings within the current group; nesting the SVG
// groups preserves clip intersections and restores the parent clip on exit.
fn vector_children(node: &Node, out: &mut String, context: &mut VectorContext<'_>) -> Option<()> {
    let mut clips = 0;
    for child in &node.children {
        if child.tag == "clip-path" && child.children.is_empty() {
            let data = quick_xml::escape::escape(child.attr("pathData")?);
            let rule = match child.attr("fillType").unwrap_or("0") {
                "0" | "0x0" | "nonZero" => "nonzero",
                "1" | "0x1" | "evenOdd" => "evenodd",
                _ => return None,
            };
            let id = context.id();
            context.definitions.push_str(&format!("<clipPath id=\"{id}\" clipPathUnits=\"userSpaceOnUse\"><path d=\"{data}\" clip-rule=\"{rule}\"/></clipPath>"));
            out.push_str(&format!("<g clip-path=\"url(#{id})\">"));
            clips += 1;
        } else {
            vector_node(child, out, context)?;
        }
    }
    for _ in 0..clips {
        out.push_str("</g>");
    }
    Some(())
}
fn vector_node(node: &Node, out: &mut String, context: &mut VectorContext<'_>) -> Option<()> {
    match node.tag.as_str() {
        "group" => {
            let px = node.number("pivotX", 0.0)?;
            let py = node.number("pivotY", 0.0)?;
            let tx = node.number("translateX", 0.0)?;
            let ty = node.number("translateY", 0.0)?;
            let sx = node.number("scaleX", 1.0)?;
            let sy = node.number("scaleY", 1.0)?;
            let rotation = node.number("rotation", 0.0)?;
            out.push_str(&format!("<g transform=\"translate({} {}) rotate({rotation}) scale({sx} {sy}) translate({} {})\">", tx+px, ty+py, -px, -py));
            vector_children(node, out, context)?;
            out.push_str("</g>");
        }
        "path" if node.children.is_empty() => {
            // Unsupported trims must fall back rather than silently drawing a different shape.
            if node.number("trimPathStart", 0.0)? != 0.0
                || node.number("trimPathEnd", 1.0)? != 1.0
                || node.number("trimPathOffset", 0.0)? != 0.0
            {
                return None;
            }
            let data = quick_xml::escape::escape(node.attr("pathData")?);
            let fill = context.paint(node, "fillColor")?;
            let stroke = context.paint(node, "strokeColor")?;
            let fill_alpha = node.number("fillAlpha", 1.0)?.clamp(0.0, 1.0);
            let stroke_alpha = node.number("strokeAlpha", 1.0)?.clamp(0.0, 1.0);
            let width = node.number("strokeWidth", 0.0)?.max(0.0);
            let rule = match node.attr("fillType").unwrap_or("0") {
                "0" | "0x0" | "nonZero" => "nonzero",
                "1" | "0x1" | "evenOdd" => "evenodd",
                _ => return None,
            };
            let cap = match node.attr("strokeLineCap").unwrap_or("0") {
                "0" | "0x0" | "butt" => "butt",
                "1" | "0x1" | "round" => "round",
                "2" | "0x2" | "square" => "square",
                _ => return None,
            };
            let join = match node.attr("strokeLineJoin").unwrap_or("0") {
                "0" | "0x0" | "miter" => "miter",
                "1" | "0x1" | "round" => "round",
                "2" | "0x2" | "bevel" => "bevel",
                _ => return None,
            };
            let miter = node.number("strokeMiterLimit", 4.0)?.max(1.0);
            out.push_str(&format!("<path d=\"{data}\" fill=\"{fill}\" fill-opacity=\"{fill_alpha}\" fill-rule=\"{rule}\" stroke=\"{stroke}\" stroke-opacity=\"{stroke_alpha}\" stroke-width=\"{width}\" stroke-linecap=\"{cap}\" stroke-linejoin=\"{join}\" stroke-miterlimit=\"{miter}\"/>"));
        }
        _ => return None,
    }
    Some(())
}
fn vector(
    node: &Node,
    package: &str,
    table: &ResourceTable,
    container: Option<&PackageContainer>,
    budget: &mut usize,
) -> Option<RgbaImage> {
    if node.attr("tint").is_some() {
        return None;
    }
    let w = node.number("viewportWidth", 0.0)?;
    let h = node.number("viewportHeight", 0.0)?;
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let alpha = node.number("alpha", 1.0)?.clamp(0.0, 1.0);
    let mut context = VectorContext {
        package,
        table,
        container,
        budget,
        definitions: String::new(),
        next_id: 0,
    };
    let mut body = String::new();
    vector_children(node, &mut body, &mut context)?;
    let svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{SIZE}\" height=\"{SIZE}\" viewBox=\"0 0 {w} {h}\"><defs>{}</defs><g opacity=\"{alpha}\">{body}</g></svg>",
        context.definitions
    );
    let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default()).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(SIZE, SIZE)?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    let mut image = RgbaImage::new(SIZE, SIZE);
    for (dest, src) in image.pixels_mut().zip(pixmap.pixels()) {
        let c = src.demultiply();
        *dest = image::Rgba([c.red(), c.green(), c.blue(), c.alpha()]);
    }
    Some(image)
}

pub(super) fn load_reference(
    raw: &str,
    package: &str,
    table: &ResourceTable,
    container: &PackageContainer,
    depth: usize,
    budget: &mut usize,
) -> Option<RgbaImage> {
    if depth >= 8 || *budget == 0 {
        return None;
    }
    *budget -= 1;
    if let Some(c) = color(raw, package, table, 0) {
        return Some(RgbaImage::from_pixel(SIZE, SIZE, image::Rgba(c)));
    }
    for path in super::candidates(raw, package, table) {
        if *budget == 0 {
            return None;
        }
        *budget -= 1;
        let Ok(bytes) = container.read_path(&path, 8 * 1024 * 1024) else {
            continue;
        };
        if let Some(image) = super::decode(&bytes) {
            return Some(image);
        }
        if bytes.len() > 1024 * 1024 {
            continue;
        }
        let Some(node) = crate::native_resources::decode(&bytes)
            .ok()
            .and_then(|xml| parse(&xml))
        else {
            continue;
        };
        if let Some(image) = render_node(&node, package, table, container, depth + 1, budget) {
            return Some(image);
        }
    }
    None
}
// Preview the rectangular solid/radial fills used by adaptive launcher layers.
// Other shape effects remain unsupported rather than silently disappearing.
fn shape(node: &Node, package: &str, table: &ResourceTable) -> Option<RgbaImage> {
    if !matches!(node.attr("shape").unwrap_or("0"), "0" | "0x0" | "rectangle")
        || node.children.len() != 1
    {
        return None;
    }
    let fill = &node.children[0];
    if fill.tag == "solid" {
        return Some(RgbaImage::from_pixel(
            SIZE,
            SIZE,
            image::Rgba(color(fill.attr("color")?, package, table, 0)?),
        ));
    }
    if fill.tag != "gradient"
        || !matches!(fill.attr("type")?, "1" | "0x1" | "radial")
        || fill.attr("centerColor").is_some()
        || fill
            .attr("useLevel")
            .is_some_and(|v| v != "false" && v != "0")
    {
        return None;
    }
    let start = color(fill.attr("startColor")?, package, table, 0)?;
    let end = color(fill.attr("endColor")?, package, table, 0)?;
    let cx = fill.number("centerX", 0.5)? * SIZE as f32;
    let cy = fill.number("centerY", 0.5)? * SIZE as f32;
    let value = fill.attr("gradientRadius")?;
    let radius: f32 = if let Some(n) = value.strip_suffix("%p").or_else(|| value.strip_suffix('%'))
    {
        n.parse::<f32>().ok()? * SIZE as f32 / 100.0
    } else {
        value
            .strip_suffix("dip")
            .or_else(|| value.strip_suffix("dp"))
            .or_else(|| value.strip_suffix("px"))
            .unwrap_or(value)
            .parse()
            .ok()?
    };
    if !radius.is_finite() || radius <= 0.0 || radius > 1_000_000.0 {
        return None;
    }
    Some(RgbaImage::from_fn(SIZE, SIZE, |x, y| {
        let t = (((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt() / radius)
            .clamp(0.0, 1.0);
        image::Rgba(std::array::from_fn(|i| {
            (start[i] as f32 * (1.0 - t) + end[i] as f32 * t).round() as u8
        }))
    }))
}

fn render_node(
    node: &Node,
    package: &str,
    table: &ResourceTable,
    container: &PackageContainer,
    depth: usize,
    budget: &mut usize,
) -> Option<RgbaImage> {
    if depth >= 8 {
        return None;
    }
    match node.tag.as_str() {
        "vector" => vector(node, package, table, Some(container), budget),
        "shape" => shape(node, package, table),
        "adaptive-icon" => {
            let mut layers = Vec::new();
            for tag in ["background", "foreground"] {
                let load = |layer: &Node, budget: &mut usize| {
                    if let Some(raw) = layer.attr("drawable") {
                        load_reference(raw, package, table, container, depth + 1, budget)
                    } else {
                        render_node(
                            layer.children.first()?,
                            package,
                            table,
                            container,
                            depth + 1,
                            budget,
                        )
                    }
                };
                let image = node
                    .children
                    .iter()
                    .find(|n| n.tag == tag)
                    .and_then(|layer| load(layer, budget))
                    .or_else(|| {
                        // Standalone base APKs can omit density-split foregrounds.
                        // Prefer the app's actual monochrome launcher artwork when available.
                        (tag == "foreground")
                            .then(|| {
                                node.children
                                    .iter()
                                    .find(|n| n.tag == "monochrome")
                                    .and_then(|layer| load(layer, budget))
                            })
                            .flatten()
                    })?;
                layers.push(image::imageops::resize(
                    &image,
                    SIZE,
                    SIZE,
                    image::imageops::FilterType::Lanczos3,
                ));
            }
            let mut background = layers.remove(0);
            image::imageops::overlay(&mut background, &layers[0], 0, 0);
            // Android reserves the outer 18dp of each 108dp layer for launcher effects.
            let crop = image::imageops::crop_imm(&background, 18, 18, 72, 72).to_image();
            let mut icon =
                image::imageops::resize(&crop, 48, 48, image::imageops::FilterType::Lanczos3);
            for (x, y, pixel) in icon.enumerate_pixels_mut() {
                let distance =
                    ((x as f32 + 0.5 - 24.0).powi(2) + (y as f32 + 0.5 - 24.0).powi(2)).sqrt();
                pixel.0[3] = (pixel.0[3] as f32 * (24.0 - distance).clamp(0.0, 1.0)) as u8;
            }
            Some(icon)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vector_gradient_resource_and_group_clips_preserve_pixels() {
        use std::io::Write;
        let path =
            std::env::temp_dir().join(format!("rdx-gradient-icon-{}.apk", std::process::id()));
        let gradient = r##"<gradient xmlns:a="http://schemas.android.com/apk/res/android" a:startX="0" a:endX="100"><item a:offset="0" a:color="#ff0000"/><item a:offset="1" a:color="#0000ff"/></gradient>"##;
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        for (name, bytes) in [
            (
                "classes.dex",
                include_bytes!("../../tests/fixtures/hello.dex").as_slice(),
            ),
            ("res/drawable/gradient.xml", gradient.as_bytes()),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
        let container = PackageContainer::open(&path).unwrap();
        let mut table = ResourceTable::default();
        table.entries.insert(
            0x7f010001,
            crate::resource_table::Resource {
                id: 0x7f010001,
                package: "demo".into(),
                kind: "drawable".into(),
                name: "gradient".into(),
                variants: vec![crate::resource_table::Variant {
                    configuration: "default".into(),
                    value: String::new(),
                    reference: None,
                    file: Some("res/drawable/gradient.xml".into()),
                }],
            },
        );
        let xml = r##"<vector xmlns:a="http://schemas.android.com/apk/res/android" a:viewportWidth="100" a:viewportHeight="100"><group><clip-path a:pathData="M20,0h60v100h-60z"/><clip-path a:pathData="M0,20h100v60h-100z"/><path a:fillColor="@drawable/gradient" a:pathData="M0,0h100v100h-100z"/></group><path a:fillColor="#00ff00" a:pathData="M0,90h100v10h-100z"/></vector>"##;
        let node = parse(xml).unwrap();
        let icon = vector(&node, "demo", &table, Some(&container), &mut 64).unwrap();
        assert_eq!(icon.get_pixel(5, 54).0[3], 0);
        assert_eq!(icon.get_pixel(54, 5).0[3], 0);
        assert_eq!(icon.get_pixel(5, 104).0, [0, 255, 0, 255]);
        let left = icon.get_pixel(30, 54).0;
        let right = icon.get_pixel(78, 54).0;
        assert!(left[0] > left[2] && right[2] > right[0]);
        assert_eq!(left[3], 255);
        assert!(vector(&node, "demo", &table, Some(&container), &mut 0).is_none());
        let mut context = VectorContext {
            package: "demo",
            table: &table,
            container: None,
            budget: &mut 64,
            definitions: String::new(),
            next_id: 0,
        };
        for invalid in [
            gradient.replace("a:startX=", "a:type=\"2\" a:startX="),
            gradient.replace("a:endX=\"100\"", "a:endX=\"NaN\""),
            gradient.replace("a:offset=\"1\"", "a:offset=\"-1\""),
            gradient.replace("a:offset=\"1\"", ""),
        ] {
            assert!(context.linear_gradient(&parse(&invalid).unwrap()).is_none());
        }
        drop(container);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn radial_shape_background_retains_center_edge_and_rejects_unsupported_effects() {
        let xml = r##"<shape xmlns:a="http://schemas.android.com/apk/res/android" a:shape="0"><gradient a:startColor="#d0e7f8" a:endColor="#aad0f3" a:type="1" a:centerX="0.5" a:centerY="0.5" a:gradientRadius="50%"/></shape>"##;
        let icon = shape(&parse(xml).unwrap(), "demo", &ResourceTable::default()).unwrap();
        assert_eq!(icon.dimensions(), (108, 108));
        assert!(icon.get_pixel(54, 54).0[0] >= 207);
        assert_eq!(icon.get_pixel(0, 0).0, [170, 208, 243, 255]);
        assert!(
            shape(
                &parse(&xml.replace("50%", "NaN")).unwrap(),
                "demo",
                &ResourceTable::default()
            )
            .is_none()
        );
        assert!(
            shape(
                &parse(&xml.replace("</shape>", "<corners a:radius=\"4dp\"/></shape>")).unwrap(),
                "demo",
                &ResourceTable::default()
            )
            .is_none()
        );
    }
    #[test]
    fn app_icon_vector_colors_groups_and_alpha() {
        let xml = r##"<vector xmlns:a="http://schemas.android.com/apk/res/android" a:viewportWidth="10" a:viewportHeight="10"><path a:fillColor="#ff8652ff" a:pathData="M0,0h10v10H0z"/><group a:translateX="2" a:translateY="2"><path a:fillColor="#80ffffff" a:pathData="M0,0h6v6H0z"/></group></vector>"##;
        let icon = vector(
            &parse(xml).unwrap(),
            "demo",
            &ResourceTable::default(),
            None,
            &mut 64,
        )
        .unwrap();
        assert_eq!(icon.get_pixel(5, 5).0, [134, 82, 255, 255]);
        let center = icon.get_pixel(54, 54).0;
        assert!(
            center[0] > 180 && center[1] > 150 && center[2] == 255 && center[3] == 255,
            "{center:?}"
        );
        assert_eq!(
            color("#8f00", "", &ResourceTable::default(), 0),
            Some([255, 0, 0, 136])
        );
        assert!(parse("<vector><path></vector>").is_none());
        let trimmed = parse(r##"<vector xmlns:a="http://schemas.android.com/apk/res/android" a:viewportWidth="10" a:viewportHeight="10"><path a:fillColor="#fff" a:trimPathEnd="0.5" a:pathData="M0 0h10v10z"/></vector>"##).unwrap();
        assert!(vector(&trimmed, "", &ResourceTable::default(), None, &mut 64).is_none());
    }
    #[test]
    fn app_icon_adaptive_inline_layers_and_reference_cycle() {
        let container =
            PackageContainer::open(std::path::Path::new("tests/fixtures/navigation.apk")).unwrap();
        let xml = r##"<adaptive-icon xmlns:a="http://schemas.android.com/apk/res/android"><background><vector a:viewportWidth="108" a:viewportHeight="108"><path a:fillColor="#8652ff" a:pathData="M0 0h108v108H0z"/></vector></background><foreground><vector a:viewportWidth="108" a:viewportHeight="108"><path a:fillColor="#fff" a:pathData="M40 40h28v28H40z"/></vector></foreground></adaptive-icon>"##;
        let mut table = ResourceTable::default();
        let icon = render_node(&parse(xml).unwrap(), "", &table, &container, 0, &mut 64).unwrap();
        assert_eq!(icon.get_pixel(24, 24).0, [255, 255, 255, 255]);
        assert_eq!(icon.get_pixel(24, 5).0, [134, 82, 255, 255]);
        assert_eq!(icon.get_pixel(0, 0).0[3], 0);
        let missing_foreground = xml
            .replace(
                "<foreground>",
                "<foreground a:drawable=\"@mipmap/missing\"/><monochrome>",
            )
            .replace("</foreground>", "</monochrome>");
        let fallback = render_node(
            &parse(&missing_foreground).unwrap(),
            "",
            &table,
            &container,
            0,
            &mut 64,
        )
        .unwrap();
        assert_eq!(fallback, icon);

        table.entries.insert(
            0x7f010001,
            crate::resource_table::Resource {
                id: 0x7f010001,
                package: "demo".into(),
                kind: "color".into(),
                name: "cycle".into(),
                variants: vec![crate::resource_table::Variant {
                    configuration: "default".into(),
                    value: String::new(),
                    file: None,
                    reference: Some(0x7f010001),
                }],
            },
        );
        assert!(color("@0x7f010001", "demo", &table, 0).is_none());
        assert!(load_reference("@0x7f010001", "demo", &table, &container, 0, &mut 64).is_none());

        assert!(load_reference("@mipmap/missing", "", &table, &container, 0, &mut 0).is_none());
    }
    #[test]
    #[ignore = "Set RDX_ICON_APK to validate a local adaptive-icon APK"]
    fn app_icon_local_apk() {
        let path = std::env::var("RDX_ICON_APK").expect("RDX_ICON_APK");
        let container = PackageContainer::open(std::path::Path::new(&path)).unwrap();
        let bytes = container
            .read_path("resources.arsc", 64 * 1024 * 1024)
            .unwrap();
        let table = ResourceTable::parse(&bytes).unwrap();
        let icon = super::super::load(&container, &table).expect("launcher icon");
        assert_eq!(icon.dimensions(), (48, 48));
        assert!(icon.pixels().any(|p| p.0[3] > 200));
        if let Ok(output) = std::env::var("RDX_ICON_OUTPUT") {
            icon.save(output).unwrap();
        }
    }
}
