use rdx::{
    native_dex::{DexClass, DexCode, DexField, DexMethod, DexSymbols},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;
fn empty(owner: &str, parent: &str) -> DexClass {
    DexClass {
        descriptor: owner.into(),
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
fn args() -> Vec<Arc<str>> {
    vec!["I".into(), "Ljava/lang/Object;".into(), "J".into()]
}
fn method(owner: &str, parameters: Vec<Arc<str>>, words: Vec<u16>) -> DexMethod {
    DexMethod {
        declaring_type: owner.into(),
        name: "<init>".into(),
        return_type: "V".into(),
        parameters,
        thrown_types: vec![],
        access_flags: 0x10001,
        code: Some(DexCode {
            registers: 5,
            ins: 5,
            outs: 5,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: words,
        }),
    }
}
fn fixture() -> (DexClass, DexClass, DexClass, DexClass) {
    let mut base = empty("Lsample/Base;", "Ljava/lang/Object;");
    base.symbols = Arc::new(DexSymbols {
        types: vec![
            "Ljava/lang/Object;".into(),
            base.descriptor.clone(),
            "I".into(),
            "J".into(),
        ],
        strings: vec![
            "<init>".into(),
            "number".into(),
            "reference".into(),
            "wide".into(),
        ],
        protos: vec![("V".into(), vec![])],
        methods: vec![(0, 0, 0)],
        fields: vec![(1, 2, 1), (1, 0, 2), (1, 3, 3)],
        ..Default::default()
    });
    base.fields = [
        ("number", "I"),
        ("reference", "Ljava/lang/Object;"),
        ("wide", "J"),
    ]
    .into_iter()
    .map(|(name, ty)| DexField {
        declaring_type: base.descriptor.clone(),
        name: name.into(),
        field_type: ty.into(),
        access_flags: 17,
        is_static: false,
    })
    .collect();
    base.methods.push(method(
        &base.descriptor,
        args(),
        vec![0x1070, 0, 0, 0x0159, 0, 0x025b, 1, 0x035a, 2, 14],
    ));
    let inner = empty("Lsample/Inner;", "Lsample/Base;");
    let outer = empty("Lsample/Outer;", "Lsample/Inner;");
    let mut child = empty("Lsample/Child;", "Lsample/Outer;");
    child.access_flags = 1;
    child.symbols = Arc::new(DexSymbols {
        types: vec![base.descriptor.clone()],
        strings: vec!["<init>".into()],
        protos: vec![("V".into(), args())],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    child.methods.push(method(
        &child.descriptor,
        vec!["Ljava/lang/Object;".into(), "J".into(), "I".into()],
        vec![0x5370, 0, 0x2140, 14],
    ));
    (base, inner, outer, child)
}
#[test]
fn two_erased_overloads_keep_wide_argument_identity_and_exact_links() {
    let (base, inner, outer, child) = fixture();
    let h = Arc::new(TypeHierarchy::from_classes([&base, &inner, &outer, &child]).unwrap());
    assert!(h.equivalent_super_constructor(&child.descriptor, &base.descriptor, &args()));
    for c in [&base, &inner, &outer, &child] {
        c.symbols.hierarchy.set(h.clone()).unwrap();
    }
    let c = native_java::render_method("sample.Child", &child, &child.methods[0]).unwrap();
    assert!(
        c.source
            .contains("super(((int) p2), ((java.lang.Object) p0), ((long) p1));"),
        "{}",
        c.source
    );
    assert!(
        c.links
            .iter()
            .any(|l| l.label == "sample.Base.<init>(ILjava/lang/Object;J)V")
    );
    for (name, class, parent) in [
        ("sample.Outer", &outer, "sample.Inner"),
        ("sample.Inner", &inner, "sample.Base"),
    ] {
        let r = native_java::render(name, class).unwrap();
        assert!(r.source.contains("super(p0, p1, p2)"), "{}", r.source);
        assert!(
            r.links
                .iter()
                .any(|l| l.label == format!("{parent}.<init>(ILjava/lang/Object;J)V"))
        );
    }
    assert!(
        !h.equivalent_constructor(&outer.descriptor, &base.descriptor, &args()),
        "super observations do not authorize allocations"
    );
}
#[test]
fn receiver_alias_is_proved_independently_of_argument_words() {
    let (base, inner, outer, mut child) = fixture();
    let code = child.methods[0].code.as_mut().unwrap();
    code.registers = 6;
    code.instructions = vec![0x1007, 0x5470, 0, 0x3250, 14];
    let h = Arc::new(TypeHierarchy::from_classes([&base, &inner, &outer, &child]).unwrap());
    assert!(h.equivalent_super_constructor(&child.descriptor, &base.descriptor, &args()));
    child.symbols.hierarchy.set(h).unwrap();
    assert!(
        native_java::render_method("sample.Child", &child, &child.methods[0])
            .unwrap()
            .source
            .contains("super(")
    );
}
#[test]
fn retained_overload_final_field_access_duplicate_and_cycle_decline() {
    for variant in 0..6 {
        let (base, mut inner, mut outer, child) = fixture();
        match variant {
            0 => inner
                .methods
                .push(method(&inner.descriptor, args(), vec![14])),
            1 => inner.fields.push(DexField {
                declaring_type: inner.descriptor.clone(),
                name: "x".into(),
                field_type: "I".into(),
                access_flags: 17,
                is_static: false,
            }),
            2 => inner.access_flags = 0x402,
            3 => outer.superclass = Some(child.descriptor.clone()),
            4 => inner.access_flags = 0x411,
            5 => inner.access_flags = 0x601,
            _ => unreachable!(),
        }
        let h = TypeHierarchy::from_classes([&base, &inner, &outer, &child]).unwrap();
        assert!(
            !h.equivalent_super_constructor(&child.descriptor, &base.descriptor, &args()),
            "variant {variant}"
        );
    }
    let (base, inner, outer, child) = fixture();
    let h = TypeHierarchy::from_classes([&base, &inner, &inner, &outer, &child]).unwrap();
    assert!(!h.equivalent_super_constructor(&child.descriptor, &base.descriptor, &args()));
}
#[test]
fn target_body_checked_or_unknown_effects_decline() {
    for variant in 0..5 {
        let (mut base, inner, outer, child) = fixture();
        match variant {
            0 => base.methods[0].thrown_types = vec!["Ljava/lang/Exception;".into()],
            1 => base.methods[0].code = None,
            2 => base.methods[0].code.as_mut().unwrap().instructions = vec![0x0012, 0x0027],
            3 => base.methods[0].code.as_mut().unwrap().instructions = vec![0x1071, 0, 0, 14],
            4 => base.methods[0].code.as_mut().unwrap().instructions = vec![0x0170, 0, 0, 14],
            _ => unreachable!(),
        }
        let h = TypeHierarchy::from_classes([&base, &inner, &outer, &child]).unwrap();
        assert!(
            !h.equivalent_super_constructor(&child.descriptor, &base.descriptor, &args()),
            "variant {variant}"
        );
    }
}
#[test]
fn every_restored_parameter_type_requires_access_evidence() {
    let (mut base, inner, outer, mut child) = fixture();
    base.methods[0].parameters[1] = "Lhidden/Argument;".into();
    Arc::get_mut(&mut child.symbols).unwrap().protos[0].1[1] = "Lhidden/Argument;".into();
    let h = TypeHierarchy::from_classes([&base, &inner, &outer, &child]).unwrap();
    assert!(!h.equivalent_super_constructor(
        &child.descriptor,
        &base.descriptor,
        &base.methods[0].parameters
    ));
    let mut argument = empty("Lhidden/Argument;", "Ljava/lang/Object;");
    argument.access_flags = 0;
    let h = TypeHierarchy::from_classes([&base, &inner, &outer, &child, &argument]).unwrap();
    assert!(!h.equivalent_super_constructor(
        &child.descriptor,
        &base.descriptor,
        &base.methods[0].parameters
    ));
    argument.access_flags = 1;
    let h = TypeHierarchy::from_classes([&base, &inner, &outer, &child, &argument]).unwrap();
    assert!(h.equivalent_super_constructor(
        &child.descriptor,
        &base.descriptor,
        &base.methods[0].parameters
    ));
}

#[test]
fn erased_layer_depth_is_bounded() {
    for count in [8, 9] {
        let (base, _, _, mut child) = fixture();
        let layers: Vec<_> = (0..count)
            .map(|n| {
                empty(
                    &format!("Lsample/Layer{n};"),
                    &if n == 0 {
                        base.descriptor.to_string()
                    } else {
                        format!("Lsample/Layer{};", n - 1)
                    },
                )
            })
            .collect();
        child.superclass = Some(layers.last().unwrap().descriptor.clone());
        let mut classes = vec![&base, &child];
        classes.extend(layers.iter());
        let h = TypeHierarchy::from_classes(classes).unwrap();
        assert_eq!(
            h.equivalent_super_constructor(&child.descriptor, &base.descriptor, &args()),
            count == 8
        );
    }
}
#[test]
fn target_checked_closure_rejects_recursion_unknown_and_try_regions() {
    for variant in 0..4 {
        let (mut base, inner, outer, child) = fixture();
        let symbols = Arc::get_mut(&mut base.symbols).unwrap();
        match variant {
            0 => {
                symbols.methods[0] = (1, 0, 0);
                let mut recursive = method(&base.descriptor, vec![], vec![0x1070, 0, 0, 14]);
                let code = recursive.code.as_mut().unwrap();
                code.registers = 1;
                code.ins = 1;
                code.outs = 1;
                base.methods.push(recursive);
            }
            1 => {
                symbols.types[0] = "Lmissing/Target;".into();
            }
            2 => {
                base.methods[0].code.as_mut().unwrap().tries = 1;
            }
            3 => {
                base.methods[0].code.as_mut().unwrap().instructions = vec![0; 513];
            }
            _ => unreachable!(),
        }
        let h = TypeHierarchy::from_classes([&base, &inner, &outer, &child]).unwrap();
        assert!(
            !h.equivalent_super_constructor(&child.descriptor, &base.descriptor, &args()),
            "variant {variant}"
        );
    }
}
#[test]
fn inaccessible_retained_target_and_duplicate_signatures_decline() {
    for variant in 0..3 {
        let (mut base, inner, outer, child) = fixture();
        match variant {
            0 => base.methods[0].access_flags = 0x10002,
            1 => base
                .methods
                .push(method(&base.descriptor, args(), vec![14])),
            2 => base.access_flags = 0x411,
            _ => unreachable!(),
        }
        let h = TypeHierarchy::from_classes([&base, &inner, &outer, &child]).unwrap();
        assert!(
            !h.equivalent_super_constructor(&child.descriptor, &base.descriptor, &args()),
            "variant {variant}"
        );
    }
}
#[test]
fn fixed_arity_wrappers_preserve_original_caller_throws_header() {
    let (mut base, inner, outer, mut child) = fixture();
    base.methods[0].access_flags |= 0x80;
    child.methods[0].thrown_types = vec!["Ljava/io/IOException;".into()];
    let h = Arc::new(TypeHierarchy::from_classes([&base, &inner, &outer, &child]).unwrap());
    for c in [&base, &inner, &outer, &child] {
        c.symbols.hierarchy.set(h.clone()).unwrap();
    }
    assert!(
        native_java::render_method("sample.Child", &child, &child.methods[0])
            .unwrap()
            .source
            .contains("throws java.io.IOException")
    );
    for (name, c) in [("sample.Inner", &inner), ("sample.Outer", &outer)] {
        let r = native_java::render(name, c).unwrap();
        assert!(r.source.contains("long p2"));
        assert!(!r.source.contains("throws "));
        assert!(!r.source.contains("..."));
    }
}
#[test]
fn malformed_target_register_and_prototype_metadata_decline() {
    for variant in 0..5 {
        let (mut base, inner, outer, child) = fixture();
        match variant {
            0 => base.methods[0].code.as_mut().unwrap().registers = 8193,
            1 => base.methods[0].code.as_mut().unwrap().ins = 4,
            2 => base.methods[0].return_type = "I".into(),
            3 => base.methods[0].declaring_type = "Lwrong/Owner;".into(),
            4 => base.methods[0].code.as_mut().unwrap().outs = 8193,
            _ => unreachable!(),
        }
        let h = TypeHierarchy::from_classes([&base, &inner, &outer, &child]).unwrap();
        assert!(
            !h.equivalent_super_constructor(&child.descriptor, &base.descriptor, &args()),
            "variant {variant}"
        );
    }
}
#[test]
fn observations_are_order_independent_and_cannot_skip_retained_overloads() {
    let (mut base, inner, outer, child) = fixture();
    let mut grand = empty("Lsample/Grand;", "Ljava/lang/Object;");
    grand
        .methods
        .push(method(&grand.descriptor, args(), vec![14]));
    base.superclass = Some(grand.descriptor.clone());
    let (_, _, _, mut incompatible) = fixture();
    incompatible.descriptor = "Lsample/OtherChild;".into();
    incompatible.methods[0].declaring_type = incompatible.descriptor.clone();
    Arc::get_mut(&mut incompatible.symbols).unwrap().types[0] = grand.descriptor.clone();
    for reverse in [false, true] {
        let mut classes = vec![&base, &inner, &outer, &child, &grand, &incompatible];
        if reverse {
            classes.reverse();
        }
        let h = TypeHierarchy::from_classes(classes).unwrap();
        assert!(h.equivalent_super_constructor(&child.descriptor, &base.descriptor, &args()));
        assert!(!h.equivalent_super_constructor(
            &incompatible.descriptor,
            &grand.descriptor,
            &args()
        ));
        assert_eq!(h.recovered_constructors(&outer.descriptor).len(), 1);
        assert_eq!(h.recovered_constructors(&inner.descriptor).len(), 1);
    }
}
fn exported(name: &str, class: &DexClass) -> serde_json::Value {
    use rdx::native_ir::{DecodedMethod, PoolKind};
    let mut fields = serde_json::Map::new();
    let mut strings = serde_json::Map::new();
    let mut types = serde_json::Map::new();
    let mut calls = serde_json::Map::new();
    let constructors:Vec<_>=class.methods.iter().filter(|m|m.name.as_ref()=="<init>").map(|m|{
        let code=m.code.as_ref().unwrap();
        for i in DecodedMethod::decode(code).unwrap().instructions {
            if let Some(r)=i.reference {let index=r.index as usize;let key=index.to_string();match r.kind {
                PoolKind::Field=>{let (o,t,n)=class.symbols.fields[index];fields.insert(key,serde_json::json!({"owner":class.symbols.types[o as usize].as_ref(),"type":class.symbols.types[t as usize].as_ref(),"name":class.symbols.strings[n as usize].as_str()}));},
                PoolKind::String=>{strings.insert(key,serde_json::json!(class.symbols.strings[index].as_str()));},
                PoolKind::Type=>{types.insert(key,serde_json::json!(class.symbols.types[index].as_ref()));},
                PoolKind::Method=>{let(o,p,n)=class.symbols.methods[index];let(ret,args)=&class.symbols.protos[p as usize];calls.insert(key,serde_json::json!({"owner":class.symbols.types[o as usize].as_ref(),"name":class.symbols.strings[n as usize].as_str(),"args":args.iter().map(AsRef::as_ref).collect::<Vec<&str>>(),"return":ret.as_ref()}));},
                _=>panic!("unexpected pool kind")
            }}
        }
        serde_json::json!({"args":m.parameters.iter().map(AsRef::as_ref).collect::<Vec<&str>>(),"registers":code.registers,"ins":code.ins,"words":code.instructions,"source":native_java::render_method(name,class,m).unwrap().source,"throws":m.thrown_types.iter().map(AsRef::as_ref).collect::<Vec<&str>>()})
    }).collect();
    let shallow = DexClass {
        descriptor: class.descriptor.clone(),
        superclass: class.superclass.clone(),
        access_flags: class.access_flags & 0x611,
        interfaces: vec![],
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![],
        symbols: class.symbols.clone(),
    };
    let rendered = native_java::render(name, &shallow).unwrap().source;
    let restored: Vec<_> = rendered
        .split("// Reconstructed forwarding constructor removed by DEX optimization.\n")
        .skip(1)
        .map(|part| {
            let end = part.find("    }\n").unwrap() + 6;
            part[..end].to_owned()
        })
        .collect();
    serde_json::json!({"name":name,"owner":class.descriptor.as_ref(),"parent":class.superclass.as_deref(),"flags":class.access_flags,"declarations":class.fields.iter().map(|f|serde_json::json!({"name":f.name.as_ref(),"type":f.field_type.as_ref(),"flags":f.access_flags,"static":f.is_static})).collect::<Vec<_>>(),"constructors":constructors,"restored":restored,"fields":fields,"strings":strings,"types":types,"calls":calls})
}
fn run_oracle(data: serde_json::Value, label: &str) {
    let jdk = std::env::var("RDX_JAVA25_HOME").expect("RDX_JAVA25_HOME required");
    let dir = std::env::temp_dir().join(format!("rdx-layered-{label}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("exact.json"),
        serde_json::to_vec_pretty(&data).unwrap(),
    )
    .unwrap();
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/layered_constructor_jvm.py");
    let output = std::process::Command::new("python3")
        .arg(script)
        .arg(&dir)
        .env("RDX_JAVA25_HOME", jdk)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    println!("{}", String::from_utf8_lossy(&output.stdout));
    println!("evidence {}", dir.display());
}
#[test]
#[ignore = "requires RDX_JAVA25_HOME, JDK25 exact wide/reference argument oracle"]
fn layered_wide_reference_jvm_oracle() {
    let (base, inner, outer, child) = fixture();
    let h = Arc::new(TypeHierarchy::from_classes([&base, &inner, &outer, &child]).unwrap());
    for c in [&base, &inner, &outer, &child] {
        c.symbols.hierarchy.set(h.clone()).unwrap();
    }
    run_oracle(
        serde_json::json!({"kind":"synthetic","classes":[exported("sample.Base",&base),exported("sample.Inner",&inner),exported("sample.Outer",&outer),exported("sample.Child",&child)],"targets":["sample.Child"]}),
        "wide",
    );
}
#[test]
#[ignore = "requires RDX_TEST_APK pinned PhonePe and RDX_JAVA25_HOME"]
fn layered_pinned_phonepe_jvm_oracle() {
    use rdx::{engine::DecompilerEngine, native_engine::NativeDexEngine};
    use sha2::{Digest, Sha256};
    let path = std::env::var("RDX_TEST_APK").expect("RDX_TEST_APK required");
    assert_eq!(
        format!("{:x}", Sha256::digest(std::fs::read(&path).unwrap())),
        "b3ed4903dcad902a39ef555a81b330040c7e868faeeb1e658be8e6025d4ebffd"
    );
    let mut engine = NativeDexEngine::default();
    engine.open(std::path::Path::new(&path)).unwrap();
    let names = [
        "kotlin.jvm.internal.e",
        "kotlin.jvm.internal.f0",
        "kotlin.jvm.internal.x",
        "kotlin.jvm.internal.t",
        "kotlin.jvm.internal.v",
        "kotlin.jvm.internal.z",
        "kotlin.jvm.internal.a0",
        "kotlin.jvm.internal.u",
        "kotlin.jvm.internal.w",
        "a2.k",
        "i72.y",
    ];
    let classes: Vec<_> = names
        .iter()
        .map(|name| exported(name, engine.class(name).unwrap()))
        .collect();
    run_oracle(
        serde_json::json!({"kind":"phonepe","classes":classes,"targets":["kotlin.jvm.internal.a0","kotlin.jvm.internal.u","kotlin.jvm.internal.w","a2.k","i72.y"]}),
        "phonepe",
    );
}

#[test]
fn retained_public_forwarder_requires_unchanged_arguments_and_complete_body() {
    let (base, _, _, _) = fixture();
    let make = || {
        let mut child = empty("Lsample/Retained;", "Lsample/Base;");
        child.access_flags = 1;
        child.symbols = Arc::new(DexSymbols {
            types: vec![base.descriptor.clone()],
            strings: vec!["<init>".into()],
            protos: vec![("V".into(), args())],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        child.methods.push(method(
            &child.descriptor,
            args(),
            vec![0x5470, 0, 0x3210, 14],
        ));
        child
    };
    let child = make();
    let admits = |c: &DexClass| {
        TypeHierarchy::from_classes([&base, c])
            .unwrap()
            .equivalent_constructor(&c.descriptor, &base.descriptor, &args())
    };
    assert!(admits(&child));
    for flags in [2, 4, 9, 0x101, 0x401] {
        let mut c = make();
        c.methods[0].access_flags = flags;
        assert!(!admits(&c), "access {flags:x}");
    }
    for words in [
        vec![0x5470, 0, 0x3120, 14],
        vec![0x1012, 0x5470, 0, 0x3210, 14],
        vec![0x5470, 0, 0x3210, 0x1071, 0, 0, 14],
    ] {
        let mut c = make();
        c.methods[0].code.as_mut().unwrap().instructions = words;
        assert!(!admits(&c));
    }
    let mut c = make();
    c.methods.push(make().methods.remove(0));
    assert!(!admits(&c));
}
