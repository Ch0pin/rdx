//! Exact Android public static member parents participate in constructor recovery.
use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;
fn class(descriptor: &str, parent: &str) -> DexClass {
    DexClass {
        descriptor: descriptor.into(),
        superclass: Some(parent.into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![],
        symbols: Arc::new(DexSymbols::default()),
    }
}
fn fixture(parent: &str, parameters: &[&str]) -> (DexClass, DexClass) {
    let leaf = class("Lsample/SdkChild;", parent);
    let mut caller = class("Lsample/Caller;", "Ljava/lang/Object;");
    caller.symbols = Arc::new(DexSymbols {
        types: vec![leaf.descriptor.clone(), parent.into()],
        strings: vec!["<init>".into()],
        protos: vec![("V".into(), parameters.iter().map(|p| (*p).into()).collect())],
        methods: vec![(1, 0, 0)],
        ..Default::default()
    });
    let count = parameters.len() + 1;
    caller.methods.push(DexMethod {
        declaring_type: caller.descriptor.clone(),
        name: "make".into(),
        return_type: leaf.descriptor.clone(),
        parameters: parameters.iter().map(|p| (*p).into()).collect(),
        thrown_types: vec![],
        access_flags: 9,
        code: Some(DexCode {
            registers: count as u16,
            ins: parameters.len() as u16,
            outs: count as u16,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: vec![
                0x0022,
                0,
                ((count as u16) << 12) | 0x70,
                0,
                if count == 3 { 0x0210 } else { 0x0010 },
                0x0011,
            ],
        }),
    });
    (caller, leaf)
}
#[test]
fn real_sdk_static_member_parent_recovers_exact_observed_overload() {
    for (parent, parameters) in [
        (
            "Landroid/view/View$BaseSavedState;",
            vec!["Landroid/os/Parcel;"],
        ),
        (
            "Landroid/view/ViewGroup$MarginLayoutParams;",
            vec!["I", "I"],
        ),
    ] {
        let (caller, leaf) = fixture(parent, &parameters);
        let h = Arc::new(TypeHierarchy::from_classes([&caller, &leaf]).unwrap());
        assert!(h.equivalent_constructor(
            &leaf.descriptor,
            parent,
            &parameters.iter().map(|p| (*p).into()).collect::<Vec<_>>()
        ));
        caller.symbols.hierarchy.set(h.clone()).unwrap();
        leaf.symbols.hierarchy.set(h).unwrap();
        let rendered =
            native_java::render_method("sample.Caller", &caller, &caller.methods[0]).unwrap();
        assert!(
            rendered.source.contains("new sample.SdkChild("),
            "{}",
            rendered.source
        );
        let declared = native_java::render("sample.SdkChild", &leaf).unwrap();
        assert!(declared.source.contains("super(p0"), "{}", declared.source);
        let signature = format!(
            "{}.<init>({})V",
            parent
                .trim_start_matches('L')
                .trim_end_matches(';')
                .replace('/', "."),
            parameters.join("")
        );
        assert!(
            rendered.links.iter().any(|link| link.label == signature),
            "{signature}"
        );
    }
}
#[test]
fn nested_sdk_parent_requires_exact_known_overload_and_unshadowed_declaration() {
    let parent = "Landroid/view/View$BaseSavedState;";
    let (caller, leaf) = fixture(parent, &["I"]);
    let h = TypeHierarchy::from_classes([&caller, &leaf]).unwrap();
    assert!(!h.equivalent_constructor(&leaf.descriptor, parent, &["I".into()]));
    let (caller, leaf) = fixture(parent, &["Landroid/os/Parcel;"]);
    for descriptor in [parent, "Landroid/view/View;"] {
        let shadow = class(descriptor, "Ljava/lang/Object;");
        let h = TypeHierarchy::from_classes([&caller, &leaf, &shadow]).unwrap();
        assert!(!h.equivalent_constructor(
            &leaf.descriptor,
            parent,
            &["Landroid/os/Parcel;".into()]
        ));
    }
}

#[test]
#[ignore = "requires javac on PATH and RDX_ANDROID35_JAR"]
fn sdk_member_parent_generated_source_compiles_against_real_api35() {
    let jar =
        std::env::var("RDX_ANDROID35_JAR").expect("RDX_ANDROID35_JAR must name API35 android.jar");
    for (index, (parent, parameters)) in [
        (
            "Landroid/view/View$BaseSavedState;",
            vec!["Landroid/os/Parcel;"],
        ),
        (
            "Landroid/view/ViewGroup$MarginLayoutParams;",
            vec!["I", "I"],
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let (caller, leaf) = fixture(parent, &parameters);
        let hierarchy = Arc::new(TypeHierarchy::from_classes([&caller, &leaf]).unwrap());
        caller.symbols.hierarchy.set(hierarchy.clone()).unwrap();
        leaf.symbols.hierarchy.set(hierarchy).unwrap();
        let leaf_source = native_java::render("sample.SdkChild", &leaf)
            .unwrap()
            .source;
        let caller_source = native_java::render("sample.Caller", &caller)
            .unwrap()
            .source;
        let directory = std::env::temp_dir().join(format!(
            "rdx-real-sdk-member-{}-{index}",
            std::process::id()
        ));
        std::fs::create_dir_all(directory.join("sample")).unwrap();
        std::fs::write(directory.join("sample/SdkChild.java"), &leaf_source).unwrap();
        std::fs::write(directory.join("sample/Caller.java"), &caller_source).unwrap();
        let output = std::process::Command::new("javac")
            .arg("-classpath")
            .arg(&jar)
            .args(["sample/SdkChild.java", "sample/Caller.java"])
            .current_dir(&directory)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{leaf_source}\n{caller_source}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
