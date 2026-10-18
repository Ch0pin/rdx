//! A large loaded project still proves exact caller constraints within the
//! bounded index, instead of losing all checked-header information at 65k.
use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;
fn method(owner: &str, name: String, instructions: Vec<u16>, catches: bool) -> DexMethod {
    DexMethod {
        declaring_type: owner.into(),
        name: name.into(),
        return_type: "V".into(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 9,
        code: Some(DexCode {
            registers: 1,
            ins: 0,
            outs: 0,
            tries: u16::from(catches),
            offset: 0,
            instructions,
            try_regions: if catches {
                vec![DexTryRegion {
                    start: 0,
                    end: 3,
                    catches: vec![(None, 4)].into(),
                }]
            } else {
                vec![]
            },
        }),
    }
}
fn class(owner: &str, symbols: Arc<DexSymbols>, methods: Vec<DexMethod>) -> DexClass {
    DexClass {
        descriptor: owner.into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 0x11,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols,
        methods,
    }
}
fn verify(count: usize, complete: bool) {
    let symbols = Arc::new(DexSymbols {
        strings: vec!["fail".into()],
        types: vec!["Lsample/Api;".into()],
        protos: vec![("V".into(), vec![])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let api = class(
        "Lsample/Api;",
        symbols.clone(),
        vec![method(
            "Lsample/Api;",
            "fail".into(),
            vec![0x0012, 0x0027],
            false,
        )],
    );
    let calls = class(
        "Lsample/Calls;",
        symbols.clone(),
        vec![
            method(
                "Lsample/Calls;",
                "caught".into(),
                vec![0x0071, 0, 0, 0x000e, 0x000d, 0x0027],
                true,
            ),
            method(
                "Lsample/Calls;",
                "uncaught".into(),
                vec![0x0071, 0, 0, 0x000e],
                false,
            ),
        ],
    );
    // The low byte 0x27 is a literal, retaining a conservative raw candidate
    // without introducing an actual throwing instruction or call graph work.
    let padding = class(
        "Lsample/Padding;",
        symbols.clone(),
        (0..count)
            .map(|n| {
                method(
                    "Lsample/Padding;",
                    format!("m{n}"),
                    vec![0x0013, 0x27, 0x000e],
                    false,
                )
            })
            .collect(),
    );
    let hierarchy = TypeHierarchy::from_classes([&api, &calls, &padding]).unwrap();
    assert!(!hierarchy.permits_inferred_checked_throw(
        &api,
        &api.methods[0],
        "Ljava/io/IOException;"
    ));
    assert_eq!(
        hierarchy.call_checked_effects("Lsample/Api;", "fail", &[], "V", true),
        Some(if complete {
            vec![]
        } else {
            vec!["Ljava/lang/Throwable;".into()]
        })
    );
    symbols.hierarchy.set(Arc::new(hierarchy)).unwrap();
    assert_eq!(
        native_java::render_method("sample.Calls", &calls, &calls.methods[0]).is_ok(),
        complete
    );
}
#[test]
fn more_than_65536_candidates_keep_exact_caller_proofs() {
    verify(65_537, true);
}
#[test]
#[ignore = "large metadata capacity boundary fixture"]
fn exceeding_candidate_capacity_retains_unknown_effects() {
    verify(262_145, false);
}
