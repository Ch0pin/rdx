use rdx::{
    native_dex::{DexClass, DexCode, DexField, DexMethod, DexSymbols},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;
fn empty(ty: &str, parent: &str) -> DexClass {
    DexClass {
        descriptor: ty.into(),
        superclass: Some(parent.into()),
        interfaces: vec![],
        access_flags: 0x401,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![],
        symbols: Arc::new(DexSymbols::default()),
    }
}
fn constructor(owner: &str, parent: &str, parameters: Vec<Arc<str>>, words: Vec<u16>) -> DexClass {
    let mut c = empty(owner, parent);
    c.access_flags = 1;
    c.symbols = Arc::new(DexSymbols {
        types: vec!["Ljava/util/AbstractCollection;".into()],
        strings: vec!["<init>".into()],
        protos: vec![("V".into(), vec![])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    c.methods.push(DexMethod {
        declaring_type: owner.into(),
        name: "<init>".into(),
        return_type: "V".into(),
        parameters: parameters.clone(),
        thrown_types: vec![],
        access_flags: 0x10001,
        code: Some(DexCode {
            registers: 1 + parameters.len() as u16,
            ins: 1 + parameters.len() as u16,
            outs: 1,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: words,
        }),
    });
    c
}
fn chain() -> (DexClass, DexClass, DexClass) {
    (
        empty("Lsample/First;", "Ljava/util/AbstractCollection;"),
        empty("Lsample/Second;", "Lsample/First;"),
        constructor(
            "Lsample/Child;",
            "Lsample/Second;",
            vec![],
            vec![0x1070, 0, 0, 14],
        ),
    )
}
#[test]
fn protected_sdk_endpoint_proves_two_erased_layers_and_exact_link() {
    let (a, b, c) = chain();
    let h = Arc::new(TypeHierarchy::from_classes([&a, &b, &c]).unwrap());
    assert!(h.equivalent_noarg_super_constructor(&c.descriptor, "Ljava/util/AbstractCollection;"));
    c.symbols.hierarchy.set(h).unwrap();
    let r = native_java::render_method("sample.Child", &c, &c.methods[0]).unwrap();
    assert!(r.source.contains("super();"), "{}", r.source);
    assert!(
        r.links
            .iter()
            .any(|l| l.label == "java.util.AbstractCollection.<init>()V")
    );
}
#[test]
fn unknown_or_declared_throw_endpoint_declines() {
    for endpoint in [
        "Lunknown/Base;",
        "Ljava/io/PrintStream;",
        "Ljava/io/ObjectInputStream;",
    ] {
        let (mut a, b, c) = chain();
        a.superclass = Some(endpoint.into());
        let h = TypeHierarchy::from_classes([&a, &b, &c]).unwrap();
        assert!(!h.has_accessible_noarg_super(&c.descriptor), "{endpoint}");
        assert!(!h.equivalent_noarg_super_constructor(&c.descriptor, endpoint));
    }
}

#[test]
#[ignore = "Requires pinned PhonePe RDX_TEST_APK and JDK25 RDX_JAVA25_HOME"]
fn exact_phonepe_erased_sdk_layers_raw_dex_jvm() {
    use rdx::{engine::DecompilerEngine, native_engine::NativeDexEngine, native_ir::DecodedMethod};
    use sha2::{Digest, Sha256};
    use std::{fs, process::Command};
    let path = std::env::var_os("RDX_TEST_APK").expect("RDX_TEST_APK");
    assert_eq!(
        format!("{:x}", Sha256::digest(fs::read(&path).unwrap())),
        "b3ed4903dcad902a39ef555a81b330040c7e868faeeb1e658be8e6025d4ebffd"
    );
    let mut engine = NativeDexEngine::default();
    engine.open(std::path::Path::new(&path)).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-implicit-sdk-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let mut rows = Vec::new();
    for name in [
        "hh.c", "hh.g", "hh.h", "hh.i", "hh.j", "hh.k", "hh.m", "of.c", "of.d", "of.f",
    ] {
        let class = engine.class(name).unwrap();
        let constructors: Vec<_> = class
            .methods
            .iter()
            .filter(|m| m.name.as_ref() == "<init>")
            .collect();
        assert_eq!(constructors.len(), 1);
        let method = constructors[0];
        let code = method.code.as_ref().unwrap();
        let ir = DecodedMethod::decode(code).unwrap();
        let fields: Vec<_> = ir.instructions.iter().filter(|i| matches!(i.opcode, 0x59 | 0x5b)).map(|i| {
            let index = i.reference.unwrap().index;
            let &(owner, ty, field) = &class.symbols.fields[index as usize];
            serde_json::json!({"index":index,"owner":class.symbols.types[owner as usize].as_ref(),"type":class.symbols.types[ty as usize].as_ref(),"name":class.symbols.strings[field as usize]})
        }).collect();
        let rendered = native_java::render_method(name, class, method).unwrap();
        assert!(
            rendered
                .links
                .iter()
                .any(|l| l.label == "java.util.AbstractCollection.<init>()V")
        );
        rows.push(serde_json::json!({"name":name,"parent":class.superclass.as_deref(),"args":method.parameters.iter().map(AsRef::as_ref).collect::<Vec<&str>>(),"registers":code.registers,"ins":code.ins,"words":code.instructions,"fields":fields,"declarations":class.fields.iter().filter(|f|!f.is_static).map(|f|serde_json::json!({"name":f.name.as_ref(),"type":f.field_type.as_ref(),"flags":f.access_flags})).collect::<Vec<_>>(),"source":rendered.source}).to_string());
    }
    fs::write(dir.join("exact.jsonl"), rows.join("\n") + "\n").unwrap();
    let output = Command::new("python3")
        .args([
            "-c",
            include_str!("fixtures/implicit_sdk_constructor_jvm.py"),
        ])
        .arg(&dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("PASS 15360 exact raw DEX/JVM constructor comparisons")
    );
    fs::remove_dir_all(dir).unwrap();
}
#[test]
fn final_field_retained_constructor_and_duplicate_layers_decline() {
    for variant in 0..5 {
        let (mut a, mut b, c) = chain();
        match variant {
            0 => a.fields.push(DexField {
                declaring_type: a.descriptor.clone(),
                name: "x".into(),
                field_type: "I".into(),
                access_flags: 17,
                is_static: false,
            }),
            1 => {
                let mut d = constructor(
                    &a.descriptor,
                    "Ljava/util/AbstractCollection;",
                    vec!["I".into()],
                    vec![0x1070, 0, 0, 14],
                );
                d.access_flags = a.access_flags;
                a = d;
            }
            2 => b.superclass = Some(c.descriptor.clone()),
            3 => a.access_flags = 0x402,
            4 => a.access_flags = 0x601,
            _ => unreachable!(),
        };
        let h = TypeHierarchy::from_classes([&a, &b, &c]).unwrap();
        assert!(
            !h.equivalent_noarg_super_constructor(&c.descriptor, "Ljava/util/AbstractCollection;"),
            "variant {variant}"
        );
    }
    let (a, b, c) = chain();
    let h = TypeHierarchy::from_classes([&a, &a, &b, &c]).unwrap();
    assert!(!h.equivalent_noarg_super_constructor(&c.descriptor, "Ljava/util/AbstractCollection;"));
}
#[test]
fn package_parent_access_is_checked_per_child() {
    let (mut a, mut b, c) = chain();
    a.access_flags = 0x400;
    b.descriptor = "Lother/Second;".into();
    let h = TypeHierarchy::from_classes([&a, &b, &c]).unwrap();
    assert!(!h.equivalent_noarg_super_constructor(&c.descriptor, "Ljava/util/AbstractCollection;"));
}
#[test]
fn sdk_loaded_parent_shadows_metadata() {
    let (a, b, c) = chain();
    let mut shadow = empty("Ljava/util/AbstractCollection;", "Ljava/lang/Object;");
    shadow.methods.push(DexMethod {
        declaring_type: shadow.descriptor.clone(),
        name: "<init>".into(),
        return_type: "V".into(),
        parameters: vec![],
        thrown_types: vec!["Ljava/lang/Exception;".into()],
        access_flags: 4,
        code: Some(DexCode {
            registers: 1,
            ins: 1,
            outs: 0,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: vec![14],
        }),
    });
    let h = TypeHierarchy::from_classes([&a, &b, &c, &shadow]).unwrap();
    assert!(!h.has_accessible_noarg_super(&c.descriptor));
}
#[test]
fn sdk_member_outer_shadow_declines() {
    let a = empty(
        "Lsample/First;",
        "Landroid/graphics/drawable/Drawable$ConstantState;",
    );
    let b = empty("Lsample/Second;", "Lsample/First;");
    let h = TypeHierarchy::from_classes([&a, &b]).unwrap();
    assert!(h.has_accessible_noarg_super(&b.descriptor));
    let outer = empty("Landroid/graphics/drawable/Drawable;", "Ljava/lang/Object;");
    let h = TypeHierarchy::from_classes([&a, &b, &outer]).unwrap();
    assert!(!h.has_accessible_noarg_super(&b.descriptor));
}

#[test]
fn sdk_object_and_intermediate_ancestor_shadows_decline() {
    for (endpoint, shadow_name) in [
        ("Ljava/util/AbstractCollection;", "Ljava/lang/Object;"),
        ("Ljava/util/AbstractList;", "Ljava/util/AbstractCollection;"),
    ] {
        let (mut a, b, c) = chain();
        a.superclass = Some(endpoint.into());
        let shadow = empty(shadow_name, "Lunknown/Ancestor;");
        let h = TypeHierarchy::from_classes([&a, &b, &c, &shadow]).unwrap();
        assert!(!h.equivalent_noarg_super_constructor(&c.descriptor, endpoint));
    }
}

#[test]
fn erased_chain_query_depth_is_bounded() {
    for count in [64, 260] {
        let classes: Vec<_> = (0..count)
            .map(|n| {
                empty(
                    &format!("Lsample/Layer{n};"),
                    &if n == 0 {
                        "Ljava/util/AbstractCollection;".to_owned()
                    } else {
                        format!("Lsample/Layer{};", n - 1)
                    },
                )
            })
            .collect();
        let child = constructor(
            "Lsample/Child;",
            &format!("Lsample/Layer{};", count - 1),
            vec![],
            vec![0x1070, 0, 0, 14],
        );
        let h = TypeHierarchy::from_classes(classes.iter().chain([&child])).unwrap();
        assert_eq!(
            h.equivalent_noarg_super_constructor(
                &child.descriptor,
                "Ljava/util/AbstractCollection;"
            ),
            count == 64
        );
    }
}
