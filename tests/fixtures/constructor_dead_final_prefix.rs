use crate::native_dex::{DexClass, DexCode, DexField, DexMethod, DexSymbols, DexTryRegion};
use std::sync::Arc;

pub(crate) fn fixture() -> DexClass {
    let owner: Arc<str> = "Lsample/Child;".into();
    DexClass {
        descriptor: owner.clone(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 0x11,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![DexField {
            declaring_type: owner.clone(),
            name: "x".into(),
            field_type: "I".into(),
            access_flags: 0x11,
            is_static: false,
        }],
        symbols: Arc::new(DexSymbols {
            strings: [
                "<init>", "step", "touch", "caught", "x", "publish", "record",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            types: [
                "Ljava/lang/Object;",
                "Lsample/Effects;",
                "Lsample/Child;",
                "I",
            ]
            .into_iter()
            .map(Arc::from)
            .collect(),
            protos: vec![
                ("V".into(), vec![]),
                ("I".into(), vec!["I".into()]),
                ("V".into(), vec!["Ljava/lang/Throwable;".into()]),
                ("V".into(), vec![owner.clone()]),
                ("V".into(), vec!["I".into()]),
            ],
            methods: vec![
                (0, 0, 0),
                (1, 1, 1),
                (1, 0, 2),
                (1, 2, 3),
                (1, 3, 5),
                (1, 4, 6),
            ],
            fields: vec![(2, 3, 4)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: owner,
            name: "<init>".into(),
            return_type: "V".into(),
            parameters: vec!["I".into()],
            thrown_types: vec![],
            access_flags: 0x10001,
            code: Some(DexCode {
                registers: 4,
                ins: 2,
                outs: 1,
                tries: 1,
                offset: 0,
                instructions: vec![
                    0x1070, 0, 2, // Object initializer
                    0x2012, 0x2059, 0, // initial final x=2
                    0x1071, 1, 3, 0x000a, // effectful step(p0)
                    0x2059, 0, // overwrite final x before any this observation
                    0x0071, 2, 0, 0x000e, // protected touch(); return
                    0x000d, 0x1071, 3, 0, 0x000e, // catch observes exact fault
                ],
                try_regions: vec![DexTryRegion {
                    start: 12,
                    end: 15,
                    catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 16)].into(),
                }],
            }),
        }],
    }
}
