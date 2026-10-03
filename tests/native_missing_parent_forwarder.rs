use rdx::{
    native_dex::{DexClass, DexCode, DexField, DexMethod, DexSymbols},
    native_hierarchy::TypeHierarchy,
    native_java,
    native_method::MethodAnalysis,
};
use std::{fs, process::Command, sync::Arc};
fn class(owner: &str, parent: &str, flags: u32) -> DexClass {
    DexClass {
        descriptor: owner.into(),
        superclass: Some(parent.into()),
        interfaces: vec![],
        access_flags: flags,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![],
        symbols: Arc::new(DexSymbols::default()),
    }
}
fn method(
    owner: &str,
    name: &str,
    args: &[&str],
    registers: u16,
    ins: u16,
    words: &[u16],
) -> DexMethod {
    DexMethod {
        declaring_type: owner.into(),
        name: name.into(),
        return_type: "V".into(),
        parameters: args.iter().map(|a| Arc::from(*a)).collect(),
        thrown_types: vec![],
        access_flags: if name == "<clinit>" { 0x10008 } else { 0x10001 },
        code: Some(DexCode {
            registers,
            ins,
            outs: registers,
            tries: 0,
            try_regions: vec![],
            instructions: words.to_vec(),
            offset: 0,
        }),
    }
}
fn fixture() -> Vec<DexClass> {
    let mut grand = class("Lsample/Grand;", "Ljava/lang/Object;", 1);
    grand.symbols = Arc::new(DexSymbols {
        types: vec!["Ljava/lang/Object;".into(), "Lsample/Hooks;".into()],
        strings: vec!["<init>".into(), "base".into()],
        protos: vec![("V".into(), vec![]), ("V".into(), vec!["I".into()])],
        methods: vec![(0, 0, 0), (1, 1, 1)],
        ..Default::default()
    });
    grand.methods.push(method(
        "Lsample/Grand;",
        "<init>",
        &["I"],
        2,
        2,
        &[0x1070, 0, 0, 0x1071, 1, 1, 0x000e],
    ));
    let mut parent = class("Lsample/Parent;", "Lsample/Grand;", 0x401);
    parent.fields.push(DexField {
        declaring_type: parent.descriptor.clone(),
        name: "state".into(),
        field_type: "I".into(),
        access_flags: 1,
        is_static: false,
    });
    parent.symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Hooks;".into()],
        strings: vec!["init".into()],
        protos: vec![("V".into(), vec![])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    parent.methods.push(method(
        "Lsample/Parent;",
        "<clinit>",
        &[],
        0,
        0,
        &[0x0071, 0, 0, 0x000e],
    ));
    let mut child = class("Lsample/Child;", "Lsample/Parent;", 1);
    child.symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Grand;".into(), "Lsample/Hooks;".into()],
        strings: vec!["pre".into(), "<init>".into(), "after".into()],
        protos: vec![
            ("I".into(), vec!["I".into()]),
            ("V".into(), vec!["I".into()]),
        ],
        methods: vec![(1, 0, 0), (0, 1, 1), (1, 1, 2)],
        ..Default::default()
    });
    child.methods.push(method(
        "Lsample/Child;",
        "<init>",
        &["I"],
        3,
        2,
        &[
            0x1071, 0, 2, 0x000a, 0x2070, 1, 0x0001, 0x1071, 2, 2, 0x000e,
        ],
    ));
    vec![grand, parent, child]
}
fn install(c: &[DexClass]) -> Arc<TypeHierarchy> {
    let h = Arc::new(TypeHierarchy::from_classes(c.iter()).unwrap());
    for x in c {
        x.symbols.hierarchy.set(h.clone()).unwrap();
    }
    h
}
fn check_front(c: &DexClass) {
    MethodAnalysis::build(c, &c.methods[0]).unwrap();
}
#[test]
fn restores_only_observed_missing_abstract_parent_overload_and_raw_delegation_link() {
    let c = fixture();
    let h = install(&c);
    let ctors = h.recovered_constructors("Lsample/Parent;");
    assert_eq!(ctors.len(), 1);
    assert_eq!(ctors[0].parameters, vec![Arc::from("I")]);
    assert_eq!(ctors[0].invoked_owner.as_ref(), "Lsample/Grand;");
    let parent = native_java::render("sample.Parent", &c[1]).unwrap();
    assert!(
        parent.source.contains("Parent(int p0)"),
        "{}",
        parent.source
    );
    assert!(parent.source.contains("super(p0);"));
    assert!(!parent.source.contains("state ="));
    let body = native_java::render_method("sample.Child", &c[2], &c[2].methods[0]).unwrap();
    assert!(
        body.source.contains("super(((int) v0));"),
        "{}",
        body.source
    );
    let link = body
        .links
        .iter()
        .find(|l| l.label == "sample.Grand.<init>(I)V")
        .unwrap();
    assert_eq!(
        body.source
            .chars()
            .skip(link.start)
            .take(link.end - link.start)
            .collect::<String>(),
        "super"
    );
}
#[test]
fn excludes_final_fields_existing_matching_ctor_enum_parent_and_inaccessible_targets() {
    for mode in 0..5 {
        let mut c = fixture();
        match mode {
            0 => c[1].fields[0].access_flags |= 16,
            1 => {
                let owner = c[1].descriptor.clone();
                c[1].methods
                    .push(method(&owner, "<init>", &["I"], 2, 2, &[0x000e]));
            }
            2 => c[1].access_flags = 0x4401,
            3 => c[0].methods[0].access_flags = 0x10002,
            4 => {
                c[0].access_flags = 0;
                c[1].superclass = Some("Loutside/Grand;".into());
                c[0].descriptor = "Loutside/Grand;".into();
                c[0].methods[0].declaring_type = c[0].descriptor.clone();
                Arc::get_mut(&mut c[2].symbols).unwrap().types[0] = "Loutside/Grand;".into();
            }
            _ => unreachable!(),
        }
        check_front(&c[2]);
        let h = install(&c);
        assert!(
            h.recovered_constructors("Lsample/Parent;").is_empty(),
            "mode {mode}"
        );
        assert!(
            native_java::render_method("sample.Child", &c[2], &c[2].methods[0]).is_err(),
            "mode {mode}"
        );
    }
}
#[test]
fn receiver_proof_rejects_other_parameter_undefined_integer_cast_and_duplicate_delegation() {
    for mode in 0..5 {
        let mut c = fixture();
        let code = c[2].methods[0].code.as_mut().unwrap();
        match mode {
            0 => code.instructions[6] = 0x0022,
            1 => {
                code.instructions[3] = 0;
                code.instructions[6] = 0x0000;
            }
            2 => code.instructions[6] = 0x0020,
            3 => {
                code.instructions.splice(4..4, [0x011f, 0]);
            }
            4 => {
                code.instructions.splice(7..7, [0x2070, 1, 0x0021]);
            }
            _ => unreachable!(),
        }
        check_front(&c[2]);
        let h = install(&c);
        assert!(
            h.recovered_constructors("Lsample/Parent;").is_empty(),
            "mode {mode}"
        );
    }
}
#[test]
fn retains_effectful_existing_intermediate_constructor_without_retargeting() {
    let mut c = fixture();
    let mut intermediate = class("Lsample/Middle;", "Lsample/Grand;", 0x401);
    intermediate.symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Grand;".into(), "Lsample/Hooks;".into()],
        strings: vec!["<init>".into(), "base".into()],
        protos: vec![("V".into(), vec!["I".into()])],
        methods: vec![(0, 0, 0), (1, 0, 1)],
        ..Default::default()
    });
    intermediate.methods.push(method(
        "Lsample/Middle;",
        "<init>",
        &["I"],
        2,
        2,
        &[0x2070, 0, 0x0010, 0x1071, 1, 1, 0x000e],
    ));
    c[1].superclass = Some(intermediate.descriptor.clone());
    c.push(intermediate);
    check_front(&c[2]);
    let h = install(&c);
    assert!(h.recovered_constructors("Lsample/Parent;").is_empty());
    assert!(native_java::render_method("sample.Child", &c[2], &c[2].methods[0]).is_err());
}
#[test]
#[ignore = "requires Java 25 flexible constructor bodies"]
fn jvm_emitted_parent_and_child_preserve_classinit_argument_ctor_throw_and_post_effect_order() {
    let c = fixture();
    install(&c);
    let dir = std::env::temp_dir().join(format!("rdx-missing-parent-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    for (name, class) in [("Grand", &c[0]), ("Parent", &c[1]), ("Child", &c[2])] {
        let code = native_java::render(&format!("sample.{name}"), class).unwrap();
        fs::write(dir.join(format!("sample/{name}.java")), code.source).unwrap();
    }
    let hooks = r#"package sample;
class Hooks { static String trace=""; static int mode; static final RuntimeException marker=new RuntimeException(); static void init(){trace+="init;";} static int pre(int v){trace+="pre"+v+";";if(mode==1)throw marker;return v+1;} static void base(int v){trace+="base"+v+";";if(mode==2)throw marker;} static void after(int v){trace+="after"+v+";";if(mode==3)throw marker;} }
public class Run { public static void main(String[] a){new Child(2);if(!Hooks.trace.equals("init;pre2;base3;after2;"))throw new AssertionError(Hooks.trace);for(int mode=0;mode<4;mode++){Hooks.trace="";Hooks.mode=mode;boolean thrown=false;try{Child c=new Child(4);if(c.state!=0)throw new AssertionError();}catch(RuntimeException actual){if(actual!=Hooks.marker)throw new AssertionError();thrown=true;}String expected=mode==1?"pre4;":mode==2?"pre4;base5;":"pre4;base5;after4;";if(!Hooks.trace.equals(expected)||thrown!=(mode!=0))throw new AssertionError(mode+":"+Hooks.trace);} } }
"#;
    fs::write(dir.join("sample/Run.java"), hooks).unwrap();
    let home = std::env::var("RDX_JAVA25_HOME")
        .expect("set RDX_JAVA25_HOME for Java 25 constructor validation");
    for (prog, args) in [
        (
            "javac",
            vec![
                "sample/Grand.java",
                "sample/Parent.java",
                "sample/Child.java",
                "sample/Run.java",
            ],
        ),
        ("java", vec!["sample.Run"]),
    ] {
        let r = Command::new(format!("{home}/bin/{prog}"))
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            r.status.success(),
            "{prog}: {}",
            String::from_utf8_lossy(&r.stderr)
        );
    }
}
#[test]
fn rejects_unreachable_delegation_bypass_return_and_prior_initialized_this_use() {
    for mode in 0..3 {
        let mut c = fixture();
        match mode {
            0 => {
                c[2].methods[0].code.as_mut().unwrap().instructions =
                    vec![0x0828, 0x1071, 0, 2, 0x000a, 0x2070, 1, 0x0001, 0x000e]
            }
            1 => {
                c[2].methods[0]
                    .code
                    .as_mut()
                    .unwrap()
                    .instructions
                    .splice(0..0, [0x0238, 12]);
            }
            2 => {
                let sy = Arc::get_mut(&mut c[2].symbols).unwrap();
                sy.types.push("Ljava/lang/Object;".into());
                sy.strings.push("hashCode".into());
                sy.protos.push(("I".into(), vec![]));
                sy.methods.push((2, 2, 3));
                c[2].methods[0]
                    .code
                    .as_mut()
                    .unwrap()
                    .instructions
                    .splice(0..0, [0x106e, 3, 1]);
            }
            _ => unreachable!(),
        }
        check_front(&c[2]);
        let h = install(&c);
        assert!(
            h.recovered_constructors("Lsample/Parent;").is_empty(),
            "mode {mode}"
        );
    }
}
#[test]
fn conflicting_observed_owners_for_same_parent_overload_are_not_last_writer_permission() {
    let mut c = fixture();
    let mut middle = class("Lsample/Middle;", "Lsample/Grand;", 0x401);
    middle.symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Grand;".into()],
        strings: vec!["<init>".into()],
        protos: vec![("V".into(), vec!["I".into()])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    middle.methods.push(method(
        "Lsample/Middle;",
        "<init>",
        &["I"],
        2,
        2,
        &[0x2070, 0, 0x0010, 0x000e],
    ));
    c[1].superclass = Some(middle.descriptor.clone());
    let mut other = fixture().pop().unwrap();
    other.descriptor = "Lsample/Other;".into();
    other.methods[0].declaring_type = other.descriptor.clone();
    Arc::get_mut(&mut other.symbols).unwrap().types[0] = middle.descriptor.clone();
    c.push(middle);
    c.push(other);
    for child in [&c[2], &c[4]] {
        check_front(child);
    }
    let h = install(&c);
    assert!(h.recovered_constructors("Lsample/Parent;").is_empty());
}
#[test]
fn chaining_only_recovery_does_not_enable_abstract_or_unobserved_allocations() {
    let c = fixture();
    let h = install(&c);
    assert!(!h.equivalent_constructor("Lsample/Parent;", "Lsample/Grand;", &[Arc::from("I")]));
    assert!(!h.equivalent_noarg_constructor("Lsample/Parent;", "Lsample/Grand;"));
}
#[test]
fn looped_single_delegation_instruction_is_not_an_exactly_once_initializer() {
    let mut c = fixture();
    c[2].methods[0].code.as_mut().unwrap().instructions =
        vec![0x2070, 1, 0x0021, 0x0239, 0xfffd, 0x000e];
    check_front(&c[2]);
    let h = install(&c);
    assert!(h.recovered_constructors("Lsample/Parent;").is_empty());
}
fn overload_fixture(reference: bool) -> Vec<DexClass> {
    let target = if reference { "Ljava/lang/Object;" } else { "J" };
    let narrow = if reference { "Ljava/lang/String;" } else { "I" };
    let mut grand = class("Lsample/Grand;", "Ljava/lang/Object;", 1);
    grand.symbols = Arc::new(DexSymbols {
        types: vec!["Ljava/lang/Object;".into(), "Lsample/Hooks;".into()],
        strings: vec![
            "<init>".into(),
            if reference {
                "baseObj".into()
            } else {
                "baseLong".into()
            },
        ],
        protos: vec![("V".into(), vec![]), ("V".into(), vec![Arc::from(target)])],
        methods: vec![(0, 0, 0), (1, 1, 1)],
        ..Default::default()
    });
    grand.methods.push(method(
        "Lsample/Grand;",
        "<init>",
        &[target],
        if reference { 2 } else { 3 },
        if reference { 2 } else { 3 },
        if reference {
            &[0x1070, 0, 0, 0x1071, 1, 1, 0x000e]
        } else {
            &[0x1070, 0, 0, 0x2071, 1, 0x0021, 0x000e]
        },
    ));
    let mut parent = class("Lsample/Parent;", "Lsample/Grand;", 0x401);
    parent.symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Grand;".into(), "Lsample/Hooks;".into()],
        strings: vec!["<init>".into(), "retained".into()],
        protos: vec![("V".into(), vec![Arc::from(target)]), ("V".into(), vec![])],
        methods: vec![(0, 0, 0), (1, 1, 1)],
        ..Default::default()
    });
    parent.methods.push(method(
        "Lsample/Parent;",
        "<init>",
        &[narrow],
        if reference { 2 } else { 4 },
        2,
        if reference {
            &[0x2070, 0, 0x0010, 0x0071, 1, 0, 0x000e]
        } else {
            &[0x3081, 0x3070, 0, 0x0102, 0x0071, 1, 0, 0x000e]
        },
    ));
    let mut child = class("Lsample/Child;", "Lsample/Parent;", 1);
    child.symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Grand;".into()],
        strings: vec!["<init>".into()],
        protos: vec![("V".into(), vec![Arc::from(target)])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    child.methods.push(method(
        "Lsample/Child;",
        "<init>",
        &[narrow],
        if reference { 2 } else { 4 },
        2,
        if reference {
            &[0x2070, 0, 0x0010, 0x000e]
        } else {
            &[0x3081, 0x3070, 0, 0x0102, 0x000e]
        },
    ));
    child.methods.push(method(
        "Lsample/Child;",
        "<init>",
        &[target],
        if reference { 2 } else { 3 },
        if reference { 2 } else { 3 },
        if reference {
            &[0x2070, 0, 0x0010, 0x000e]
        } else {
            &[0x3070, 0, 0x0210, 0x000e]
        },
    ));
    vec![grand, parent, child]
}
#[test]
fn final_emission_pins_original_reference_null_and_primitive_descriptors_against_retained_parent_overloads()
 {
    for reference in [false, true] {
        let c = overload_fixture(reference);
        let h = install(&c);
        let target = if reference { "Ljava/lang/Object;" } else { "J" };
        assert!(
            h.recovered_constructors("Lsample/Parent;")
                .iter()
                .any(|r| r.parameters == vec![Arc::from(target)])
        );
        let full = native_java::render("sample.Child", &c[2]).unwrap();
        assert!(
            full.source.contains(if reference {
                "super(((Object)"
            } else {
                "super(((long)"
            }),
            "{}",
            full.source
        );
        assert!(
            full.links
                .iter()
                .any(|l| l.label == format!("sample.Grand.<init>({target})V"))
        );
    }
}
#[test]
#[ignore = "requires Java 25"]
fn jvm_full_pipeline_retains_parent_overload_effects_and_selects_exact_restored_descriptor() {
    for reference in [false, true] {
        let c = overload_fixture(reference);
        install(&c);
        let dir = std::env::temp_dir().join(format!(
            "rdx-parent-overloads-{}-{reference}",
            std::process::id()
        ));
        fs::create_dir_all(dir.join("sample")).unwrap();
        for (name, class) in [("Grand", &c[0]), ("Parent", &c[1]), ("Child", &c[2])] {
            fs::write(
                dir.join(format!("sample/{name}.java")),
                native_java::render(&format!("sample.{name}"), class)
                    .unwrap()
                    .source,
            )
            .unwrap();
        }
        let calls = if reference {
            r#"new Child("x");if(!Hooks.trace.equals("baseObj;"))throw new AssertionError(Hooks.trace);Hooks.trace="";new Child((String)null);if(!Hooks.trace.equals("baseObj;"))throw new AssertionError(Hooks.trace);Hooks.trace="";new Child((Object)null);if(!Hooks.trace.equals("baseObj;"))throw new AssertionError(Hooks.trace);Hooks.trace="";new Retained("x");if(!Hooks.trace.equals("baseObj;retained;"))throw new AssertionError(Hooks.trace);"#
        } else {
            r#"new Child(5);if(!Hooks.trace.equals("base5;"))throw new AssertionError(Hooks.trace);Hooks.trace="";new Child(5L);if(!Hooks.trace.equals("base5;"))throw new AssertionError(Hooks.trace);Hooks.trace="";new Retained(5);if(!Hooks.trace.equals("base5;retained;"))throw new AssertionError(Hooks.trace);"#
        };
        let narrow = if reference { "String" } else { "int" };
        let run = format!(
            r#"package sample;class Hooks{{static String trace="";static void baseObj(Object value){{trace+="baseObj;";}}static void baseLong(long value){{trace+="base"+value+";";}}static void retained(){{trace+="retained;";}}}}class Retained extends Parent{{Retained({narrow} value){{super(value);}}}}public class Run{{public static void main(String[] a){{{calls}}}}}"#
        );
        fs::write(dir.join("sample/Run.java"), run).unwrap();
        let home = std::env::var("RDX_JAVA25_HOME")
            .expect("set RDX_JAVA25_HOME for Java 25 constructor validation");
        for (prog, args) in [
            (
                "javac",
                vec![
                    "sample/Grand.java",
                    "sample/Parent.java",
                    "sample/Child.java",
                    "sample/Run.java",
                ],
            ),
            ("java", vec!["sample.Run"]),
        ] {
            let r = Command::new(format!("{home}/bin/{prog}"))
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(
                r.status.success(),
                "{reference} {prog}: {}",
                String::from_utf8_lossy(&r.stderr)
            );
        }
    }
}
fn early_field_fixture() -> Vec<DexClass> {
    let mut c = fixture();
    let child = &mut c[2];
    child.fields.push(DexField {
        declaring_type: child.descriptor.clone(),
        name: "captured".into(),
        field_type: "I".into(),
        access_flags: 1,
        is_static: false,
    });
    let sy = Arc::get_mut(&mut child.symbols).unwrap();
    sy.types.extend(["Lsample/Child;".into(), "I".into()]);
    sy.strings.push("captured".into());
    sy.fields.push((2, 3, 3));
    child.methods[0]
        .code
        .as_mut()
        .unwrap()
        .instructions
        .splice(0..0, [0x1259, 0]);
    let sy = Arc::get_mut(&mut c[0].symbols).unwrap();
    sy.strings.push("observe".into());
    sy.protos
        .push(("V".into(), vec!["Ljava/lang/Object;".into()]));
    sy.methods.push((1, 2, 2));
    c[0].methods[0]
        .code
        .as_mut()
        .unwrap()
        .instructions
        .splice(3..3, [0x1071, 2, 0]);
    c
}
#[test]
fn early_owned_field_write_is_preserved_but_inherited_read_and_this_escape_are_rejected() {
    let c = early_field_fixture();
    check_front(&c[2]);
    let h = install(&c);
    assert_eq!(h.recovered_constructors("Lsample/Parent;").len(), 1);
    let body = native_java::render_method("sample.Child", &c[2], &c[2].methods[0]).unwrap();
    assert!(
        body.source.find("captured =").unwrap() < body.source.find("super(").unwrap(),
        "{}",
        body.source
    );
    for mode in 0..3 {
        let mut c = early_field_fixture();
        match mode {
            0 => Arc::get_mut(&mut c[2].symbols).unwrap().types[2] = "Lsample/Parent;".into(),
            1 => c[2].methods[0].code.as_mut().unwrap().instructions[0] = 0x1052,
            2 => {
                c[2].fields[0].field_type = "Ljava/lang/Object;".into();
                Arc::get_mut(&mut c[2].symbols).unwrap().types[3] = "Ljava/lang/Object;".into();
                c[2].methods[0].code.as_mut().unwrap().instructions[0] = 0x115b;
            }
            _ => unreachable!(),
        }
        check_front(&c[2]);
        let h = install(&c);
        assert!(
            h.recovered_constructors("Lsample/Parent;").is_empty(),
            "mode {mode}"
        );
    }
}
#[test]
#[ignore = "requires Java 25"]
fn jvm_superclass_observes_early_child_field_at_original_position() {
    let c = early_field_fixture();
    install(&c);
    let dir = std::env::temp_dir().join(format!("rdx-parent-early-field-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    for (name, class) in [("Grand", &c[0]), ("Parent", &c[1]), ("Child", &c[2])] {
        fs::write(
            dir.join(format!("sample/{name}.java")),
            native_java::render(&format!("sample.{name}"), class)
                .unwrap()
                .source,
        )
        .unwrap();
    }
    let java = r#"package sample;class Hooks{static String trace="";static int expected;static void init(){trace+="init;";}static int pre(int value){trace+="pre"+value+";";return value+1;}static void observe(Object value){int actual=((Child)value).captured;if(actual!=expected)throw new AssertionError(actual);trace+="see"+actual+";";}static void base(int value){trace+="base"+value+";";}static void after(int value){trace+="after"+value+";";}}public class Run{public static void main(String[] args){Hooks.expected=4;new Child(4);if(!Hooks.trace.equals("init;pre4;see4;base5;after4;"))throw new AssertionError(Hooks.trace);}}"#;
    fs::write(dir.join("sample/Run.java"), java).unwrap();
    let home = std::env::var("RDX_JAVA25_HOME")
        .expect("set RDX_JAVA25_HOME for Java 25 constructor validation");
    for (prog, args) in [
        (
            "javac",
            vec![
                "sample/Grand.java",
                "sample/Parent.java",
                "sample/Child.java",
                "sample/Run.java",
            ],
        ),
        ("java", vec!["sample.Run"]),
    ] {
        let r = Command::new(format!("{home}/bin/{prog}"))
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            r.status.success(),
            "{prog}: {}",
            String::from_utf8_lossy(&r.stderr)
        );
    }
}
