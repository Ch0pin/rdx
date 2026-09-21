//! Native adaptations of MIT-licensed palettes. See third_party/themes/README.md.
use syntect::highlighting::{
    Color, FontStyle, StyleModifier, Theme, ThemeItem, ThemeSet, ThemeSettings,
};

fn rgb(value: u32) -> Color {
    Color {
        r: (value >> 16) as u8,
        g: (value >> 8) as u8,
        b: value as u8,
        a: 255,
    }
}

pub(super) fn extend(set: &mut ThemeSet) {
    // Background, foreground, comments, keyword, string, number, type, function, variable.
    for (name, colors) in [
        (
            "Atom One Light",
            [
                0xfafafa, 0x383a42, 0x6a6b70, 0xa626a4, 0x397f32, 0x986801, 0x986801, 0x3568d4,
                0xb43e44,
            ],
        ),
        (
            "Quiet Light",
            [
                0xf5f5f5, 0x333333, 0x707070, 0x4b69c6, 0x3d7b23, 0x9c5d27, 0x7a3e9d, 0xaa3731,
                0x7a3e9d,
            ],
        ),
        (
            "One Dark",
            [
                0x282c34, 0xabb2bf, 0x929aab, 0xc678dd, 0x98c379, 0xd19a66, 0xe5c07b, 0x61afef,
                0xe57780,
            ],
        ),
        (
            "Dracula",
            [
                0x282a36, 0xf8f8f2, 0x8797c8, 0xff79c6, 0xf1fa8c, 0xbd93f9, 0x8be9fd, 0x50fa7b,
                0xf8f8f2,
            ],
        ),
    ] {
        let mut theme = Theme {
            name: Some(name.into()),
            settings: ThemeSettings {
                background: Some(rgb(colors[0])),
                foreground: Some(rgb(colors[1])),
                ..Default::default()
            },
            ..Default::default()
        };
        for (scope, index) in [
            ("comment", 2),
            ("keyword, storage", 3),
            ("string", 4),
            ("constant.numeric, constant.language, constant.character", 5),
            (
                "entity.name.type, entity.name.class, support.type, support.class",
                6,
            ),
            ("entity.name.function, support.function", 7),
            ("variable, entity.other.attribute-name", 8),
            ("entity.name.tag", 3),
        ] {
            theme.scopes.push(ThemeItem {
                scope: scope.parse().expect("embedded theme scopes"),
                style: StyleModifier {
                    foreground: Some(rgb(colors[index])),
                    font_style: Some(if index == 2 {
                        FontStyle::ITALIC
                    } else {
                        FontStyle::empty()
                    }),
                    ..Default::default()
                },
            });
        }
        set.themes.insert(name.into(), theme);
    }
}
