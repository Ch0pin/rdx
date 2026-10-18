use rdx::{
    native_dex::{self, DexSymbols},
    native_java,
};
use std::sync::Arc;
fn fixture(index: usize) -> native_dex::DexClass {
    let mut c = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    c.methods.retain(|m| m.name.as_ref() == "answer");
    c.fields.clear();
    match index {
        0 => {
            c.descriptor = "Lkotlin/text/StringsKt;".into();
            c.symbols = Arc::new(DexSymbols {
                types: vec![
                    "Ljava/lang/Character;".into(),
                    "Ljava/lang/Integer;".into(),
                    "Ljava/lang/String;".into(),
                    "Lkotlin/jvm/internal/Intrinsics;".into(),
                    "Lkotlin/text/CharsKt__CharJVMKt;".into(),
                ],
                strings: vec![
                    "<this>".into(),
                    "digit".into(),
                    "valueOf".into(),
                    "charAt".into(),
                    "length".into(),
                    "checkNotNullParameter".into(),
                    "compare".into(),
                    "checkRadix".into(),
                ],
                protos: vec![
                    ("I".into(), vec!["I".into(), "I".into()]),
                    ("Ljava/lang/Integer;".into(), vec!["I".into()]),
                    ("C".into(), vec!["I".into()]),
                    ("I".into(), vec![]),
                    (
                        "V".into(),
                        vec!["Ljava/lang/Object;".into(), "Ljava/lang/String;".into()],
                    ),
                    ("I".into(), vec!["I".into()]),
                ],
                methods: vec![
                    (3, 4, 5),
                    (4, 5, 7),
                    (2, 3, 4),
                    (2, 2, 3),
                    (3, 0, 6),
                    (0, 0, 1),
                    (1, 1, 2),
                ],
                fields: vec![],
                ..Default::default()
            });
            let m = &mut c.methods[0];
            m.name = "s0".into();
            m.declaring_type = c.descriptor.clone();
            m.return_type = "Ljava/lang/Integer;".into();
            m.parameters = vec!["I".into(), "Ljava/lang/String;".into()];
            m.access_flags = 9;
            let code = m.code.as_mut().unwrap();
            code.registers = 11;
            code.ins = 2;
            code.outs = 2;
            code.instructions = vec![
                26, 0, 8305, 0, 10, 4209, 1, 9, 4206, 2, 10, 10, 57, 3, 17704, 274, 8302, 3, 26,
                522, 787, 48, 8305, 4, 50, 778, 1044, 1, 32768, 827, 21, 4882, 12339, 3, 12584,
                1299, 43, 21042, 11, 1043, 45, 16946, 3, 10280, 1045, 32768, 12801, 1320, 4609,
                808, 4609, 8961, 1300, 50973, 64625, 22017, 821, 33, 8302, 3, 58, 1802, 8305, 5,
                151, 1802, 1851, 3, 3880, 24885, 9, 22067, 12, 1683, 2308, 24885, 3, 1576, 37298,
                2192, 1796, 33077, 4, 2322, 2321, 29105, 984, 259, 57384, 568, 7, 4209, 6, 1, 2316,
                2321, 6523, 4209, 6, 9, 2316, 2321,
            ];
        }
        1 => {
            c.descriptor = "Lkotlin/text/StringsKt;".into();
            c.symbols = Arc::new(DexSymbols {
                types: vec![
                    "Ljava/lang/Character;".into(),
                    "Ljava/lang/Long;".into(),
                    "Ljava/lang/String;".into(),
                    "Lkotlin/jvm/internal/Intrinsics;".into(),
                    "Lkotlin/text/CharsKt__CharJVMKt;".into(),
                ],
                strings: vec![
                    "<this>".into(),
                    "digit".into(),
                    "valueOf".into(),
                    "charAt".into(),
                    "length".into(),
                    "checkNotNullParameter".into(),
                    "compare".into(),
                    "checkRadix".into(),
                ],
                protos: vec![
                    ("I".into(), vec!["I".into(), "I".into()]),
                    ("Ljava/lang/Long;".into(), vec!["J".into()]),
                    ("C".into(), vec!["I".into()]),
                    ("I".into(), vec![]),
                    (
                        "V".into(),
                        vec!["Ljava/lang/Object;".into(), "Ljava/lang/String;".into()],
                    ),
                    ("I".into(), vec!["I".into()]),
                ],
                methods: vec![
                    (3, 4, 5),
                    (4, 5, 7),
                    (2, 3, 4),
                    (2, 2, 3),
                    (3, 0, 6),
                    (0, 0, 1),
                    (1, 1, 2),
                ],
                fields: vec![],
                ..Default::default()
            });
            let m = &mut c.methods[0];
            m.name = "t0".into();
            m.declaring_type = c.descriptor.clone();
            m.return_type = "Ljava/lang/Long;".into();
            m.parameters = vec!["I".into(), "Ljava/lang/String;".into()];
            m.access_flags = 9;
            let code = m.code.as_mut().unwrap();
            code.registers = 21;
            code.ins = 2;
            code.outs = 2;
            code.instructions = vec![
                2, 19, 264, 20, 538, 0, 8305, 0, 33, 4209, 1, 0, 4206, 2, 1, 522, 569, 3, 22824,
                786, 8302, 3, 49, 1034, 1299, 48, 8305, 4, 84, 1290, 1560, 1, 0, 0, 32768, 1339,
                25, 5394, 21043, 3, 17192, 2067, 43, 33842, 11, 787, 45, 13362, 3, 14888, 1561,
                32768, 21249, 2088, 4610, 5, 13569, 770, 18, 552, 13569, 2072, 7282, 29127, 50972,
                64625, 2582, 0, 35844, 9013, 44, 8302, 3, 49, 1034, 8305, 5, 4, 1034, 1083, 3,
                6696, 3633, 3082, 3643, 14, 3121, 2060, 3129, 19, 3201, 3230, 3078, 3633, 3082,
                3643, 3, 2600, 3713, 60093, 20097, 4251, 3590, 1073, 4106, 1083, 4, 18, 17, 60092,
                984, 259, 54568, 1336, 7, 8305, 6, 186, 12, 17, 41085, 8305, 6, 16, 12, 17,
            ];
        }
        2 => {
            c.descriptor = "Lkotlin/text/d0;".into();
            c.symbols = Arc::new(DexSymbols {
                types: vec![
                    "Ll92/e0;".into(),
                    "Ljava/lang/Character;".into(),
                    "Ljava/lang/Integer;".into(),
                    "Ljava/lang/String;".into(),
                    "Lkotlin/jvm/internal/Intrinsics;".into(),
                    "Lkotlin/text/CharsKt__CharJVMKt;".into(),
                    "Ll92/d0;".into(),
                ],
                strings: vec![
                    "<this>".into(),
                    "digit".into(),
                    "compareUnsigned".into(),
                    "divideUnsigned".into(),
                    "charAt".into(),
                    "length".into(),
                    "checkNotNullParameter".into(),
                    "compare".into(),
                    "checkRadix".into(),
                    "<init>".into(),
                    "b".into(),
                ],
                protos: vec![
                    ("I".into(), vec!["I".into(), "I".into()]),
                    ("C".into(), vec!["I".into()]),
                    ("I".into(), vec![]),
                    (
                        "V".into(),
                        vec!["Ljava/lang/Object;".into(), "Ljava/lang/String;".into()],
                    ),
                    ("I".into(), vec!["I".into()]),
                    ("V".into(), vec!["I".into()]),
                ],
                methods: vec![
                    (4, 3, 6),
                    (5, 4, 8),
                    (3, 2, 5),
                    (3, 1, 4),
                    (4, 0, 7),
                    (1, 0, 1),
                    (2, 0, 2),
                    (2, 0, 3),
                    (0, 5, 9),
                ],
                fields: vec![(0, 6, 10)],
                ..Default::default()
            });
            let m = &mut c.methods[0];
            m.name = "e".into();
            m.declaring_type = c.descriptor.clone();
            m.return_type = "Ll92/e0;".into();
            m.parameters = vec!["I".into(), "Ljava/lang/String;".into()];
            m.access_flags = 25;
            let code = m.code.as_mut().unwrap();
            code.registers = 9;
            code.ins = 2;
            code.outs = 2;
            code.instructions = vec![
                26, 0, 8305, 0, 8, 4209, 1, 7, 4206, 2, 8, 10, 57, 3, 17960, 274, 8302, 3, 24, 522,
                787, 48, 8305, 4, 50, 778, 827, 10, 4882, 12338, 55, 1043, 43, 16946, 4, 12584,
                4865, 610, 0, 532, 29127, 1820, 9217, 821, 47, 8302, 3, 56, 1290, 8305, 5, 117,
                1290, 1339, 3, 7464, 8305, 6, 65, 1546, 1597, 16, 9267, 22, 62482, 8305, 7, 116,
                1034, 8305, 6, 65, 1546, 1597, 3, 2344, 29106, 5552, 8305, 6, 21, 266, 315, 4,
                1810, 1809, 984, 259, 20737, 53800, 1826, 0, 8304, 8, 23, 1809,
            ];
        }
        3 => {
            c.descriptor = "Lkotlin/text/d0;".into();
            c.symbols = Arc::new(DexSymbols {
                types: vec![
                    "Ll92/h0;".into(),
                    "Ljava/lang/Character;".into(),
                    "Ljava/lang/Long;".into(),
                    "Ljava/lang/String;".into(),
                    "Lkotlin/jvm/internal/Intrinsics;".into(),
                    "Lkotlin/text/CharsKt__CharJVMKt;".into(),
                    "Ll92/e0;".into(),
                    "Ll92/d0;".into(),
                    "Ll92/g0;".into(),
                ],
                strings: vec![
                    "<this>".into(),
                    "digit".into(),
                    "compareUnsigned".into(),
                    "divideUnsigned".into(),
                    "charAt".into(),
                    "length".into(),
                    "checkNotNullParameter".into(),
                    "compare".into(),
                    "checkRadix".into(),
                    "<init>".into(),
                    "b".into(),
                ],
                protos: vec![
                    ("I".into(), vec!["I".into(), "I".into()]),
                    ("I".into(), vec!["J".into(), "J".into()]),
                    ("J".into(), vec!["J".into(), "J".into()]),
                    ("C".into(), vec!["I".into()]),
                    ("I".into(), vec![]),
                    (
                        "V".into(),
                        vec!["Ljava/lang/Object;".into(), "Ljava/lang/String;".into()],
                    ),
                    ("I".into(), vec!["I".into()]),
                    ("V".into(), vec!["J".into()]),
                ],
                methods: vec![
                    (4, 5, 6),
                    (5, 6, 8),
                    (3, 4, 5),
                    (3, 3, 4),
                    (4, 0, 7),
                    (1, 0, 1),
                    (2, 1, 2),
                    (2, 2, 3),
                    (0, 7, 9),
                ],
                fields: vec![(0, 8, 10), (6, 7, 10)],
                ..Default::default()
            });
            let m = &mut c.methods[0];
            m.name = "h".into();
            m.declaring_type = c.descriptor.clone();
            m.return_type = "Ll92/h0;".into();
            m.parameters = vec!["I".into(), "Ljava/lang/String;".into()];
            m.access_flags = 25;
            let code = m.code.as_mut().unwrap();
            code.registers = 16;
            code.ins = 2;
            code.outs = 4;
            code.instructions = vec![
                26, 0, 8305, 0, 15, 4209, 1, 14, 4206, 2, 15, 10, 57, 3, 22056, 274, 8302, 3, 31,
                522, 787, 48, 8305, 4, 50, 778, 827, 10, 4370, 4146, 71, 787, 43, 12850, 3, 16680,
                57985, 1122, 0, 1048, 50972, 7281, 29127, 1820, 1558, 0, 18436, 309, 59, 8302, 3,
                31, 2570, 8305, 5, 234, 2570, 2619, 3, 10536, 16497, 6, 39030, 2826, 2877, 19,
                2097, 1032, 2105, 32, 2070, 65535, 16497, 7, 12952, 2059, 16497, 6, 39030, 2826,
                2877, 3, 4648, 9917, 2914, 1, 43649, 3096, 65535, 65535, 0, 0, 51904, 27323, 16497,
                6, 30394, 1546, 1595, 4, 3602, 3601, 472, 257, 42500, 50728, 3618, 0, 12400, 8,
                1902, 3601,
            ];
        }
        _ => unreachable!(),
    };
    c
}
#[test]
fn canonical_numeric_loops_keep_boolean_slot_refinements() {
    for index in 0..4 {
        let c = fixture(index);
        let source = native_java::render_method(
            &c.descriptor
                .trim_start_matches('L')
                .trim_end_matches(';')
                .replace('/', "."),
            &c,
            &c.methods[0],
        )
        .unwrap_or_else(|e| panic!("variant {index}: {e}"));
        assert!(source.source.contains("while"));
        assert!(source.source.contains("return"));
    }
}
