//! Independent effect-placement checks for shared tails and exception boundaries.
use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_java,
};
use std::sync::Arc;
fn fixture(words: Vec<u16>) -> DexClass {
    DexClass {
        descriptor: "Lsample/Test;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            strings: vec!["effect".into()],
            types: vec!["Lsample/Effects;".into()],
            protos: vec![("V".into(), vec!["I".into()])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/Test;".into(),
            name: "run".into(),
            return_type: "V".into(),
            parameters: vec!["I".into()],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 2,
                ins: 1,
                outs: 1,
                tries: 0,
                try_regions: vec![],
                instructions: words,
                offset: 0,
            }),
        }],
    }
}
#[test]
fn shared_effect_is_emitted_once_in_each_mutually_exclusive_arm() {
    let c = fixture(vec![
        0x0138, 7, 0x1012, 0x1071, 0, 0, 0x0328, 0x2012, 0xfb28, 0x000e,
    ]);
    let code = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
    assert!(!code.source.contains("while"), "{}", code.source);
    let (first, second) = code
        .source
        .split_once("} else {")
        .expect("mutually exclusive arms");
    assert_eq!(
        first.matches("sample.Effects.effect(").count(),
        1,
        "{}",
        code.source
    );
    assert_eq!(
        second.matches("sample.Effects.effect(").count(),
        1,
        "{}",
        code.source
    );
    assert!(code.source.contains("effect(1)"), "{}", code.source);
    assert!(code.source.contains("effect(2)"), "{}", code.source);
    assert_eq!(code.source.matches("return;").count(), 1);
    assert_eq!(
        code.links
            .iter()
            .filter(|l| l.label == "sample.Effects.effect(I)V")
            .count(),
        2
    );
}
#[test]
fn cyclic_tail_is_not_unrolled_as_an_acyclic_shared_effect() {
    let c = fixture(vec![
        0x0138, 7, 0x1012, 0x1071, 0, 0, 0x0128, 0x2012, 0xfb28,
    ]);
    let error = native_java::render_method("sample.Test", &c, &c.methods[0])
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("loop"), "{error}");
}
#[test]
fn unprotected_effect_cannot_be_moved_inside_try_to_force_a_join() {
    // nop (protected); effect(p0); goto return; handler: move-exception; goto return.
    let mut c = fixture(vec![0, 0x1071, 0, 1, 0x0328, 0x000d, 0x0128, 0x000e]);
    let code = c.methods[0].code.as_mut().unwrap();
    code.tries = 1;
    code.try_regions.push(DexTryRegion {
        start: 0,
        end: 1,
        catches: vec![(Some("Ljava/lang/Exception;".into()), 5)].into(),
    });
    let error = native_java::render_method("sample.Test", &c, &c.methods[0])
        .err()
        .unwrap()
        .to_string();
    assert!(
        error.contains("effectful normal continuation before exception join"),
        "{error}"
    );
}
