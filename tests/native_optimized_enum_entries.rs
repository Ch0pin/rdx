pub use rdx::{native_dex, native_ir};
use rdx::{
    native_dex::{
        DexAnnotation, DexAnnotationDirectory, DexClass, DexCode, DexField, DexMethod, DexSymbols,
        DexValue,
    },
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::{process::Command, sync::Arc};
#[path = "../src/native_enum_array_retainer.rs"]
mod retainer;
fn fixture() -> DexClass {
    let mut class = DexClass {
        descriptor: "Lsample/Mode;".into(),
        superclass: Some("Ljava/lang/Enum;".into()),
        interfaces: vec![],
        access_flags: 0x4011,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![],
        symbols: Arc::new(DexSymbols {
            strings: vec![
                "ONE".into(),
                "TWO".into(),
                "a".into(),
                "b".into(),
                "all".into(),
                "<init>".into(),
                "clone".into(),
            ],
            types: vec![
                "Lsample/Mode;".into(),
                "Ljava/lang/Enum;".into(),
                "[Lsample/Mode;".into(),
            ],
            protos: vec![
                ("V".into(), vec!["Ljava/lang/String;".into(), "I".into()]),
                ("Ljava/lang/Object;".into(), vec![]),
            ],
            methods: vec![(1, 0, 5), (2, 1, 6)],
            fields: vec![(0, 0, 2), (0, 0, 3), (0, 2, 4)],
            ..Default::default()
        }),
    };
    for (name, ty, flags) in [
        ("a", "Lsample/Mode;", 0x4019),
        ("b", "Lsample/Mode;", 0x4019),
        ("all", "[Lsample/Mode;", 0x1a),
    ] {
        class.fields.push(DexField {
            declaring_type: class.descriptor.clone(),
            name: name.into(),
            field_type: ty.into(),
            access_flags: flags,
            is_static: true,
        });
    }
    class.methods.push(DexMethod {
        declaring_type: class.descriptor.clone(),
        name: "<clinit>".into(),
        return_type: "V".into(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 0x10008,
        code: Some(DexCode {
            registers: 4,
            ins: 0,
            outs: 3,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: vec![
                0x0022, 0, 0x011a, 0, 0x0212, 0x3070, 0, 0x0210, 0x0069, 0, 0x0122, 0, 0x021a, 1,
                0x1312, 0x3070, 0, 0x0321, 0x0169, 1, 0x2024, 2, 0x0010, 0x000c, 0x0069, 2, 0x000e,
            ],
        }),
    });
    class.methods.push(DexMethod {
        declaring_type: class.descriptor.clone(),
        name: "values".into(),
        return_type: "[Lsample/Mode;".into(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 9,
        code: Some(DexCode {
            registers: 1,
            ins: 0,
            outs: 1,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: vec![0x0062, 2, 0x106e, 1, 0, 0x000c, 0x001f, 2, 0x0011],
        }),
    });
    class
}

fn world() -> Vec<DexClass> {
    world_with_tail(false)
}
fn world_with_tail(tail: bool) -> Vec<DexClass> {
    let mut c = fixture();
    c.fields[0].name = "ONE".into();
    c.fields[1].name = "TWO".into();
    c.fields[2].name = "$VALUES".into();
    c.annotations_offset = 1;
    let s = Arc::get_mut(&mut c.symbols).unwrap();
    s.strings[2] = "ONE".into();
    s.strings[3] = "TWO".into();
    s.strings[4] = "$VALUES".into();
    s.strings
        .extend(["$ENTRIES", "items", "value"].map(String::from));
    s.types.extend(
        [
            "[Ljava/lang/Enum;",
            "Lsample/Entries;",
            "Lsample/EntryList;",
            "Ljava/lang/Object;",
            "Lsample/Tag;",
        ]
        .map(Into::into),
    );
    s.protos.extend([
        ("V".into(), vec!["[Ljava/lang/Enum;".into()]),
        ("V".into(), vec![]),
    ]);
    s.methods.extend([(4, 2, 5), (6, 3, 5)]);
    s.fields.extend([(0, 5, 7), (4, 3, 8)]);
    s.annotations.insert(
        1,
        Arc::new(DexAnnotationDirectory {
            class: None,
            fields: vec![
                Some(Arc::from(vec![Arc::new(DexAnnotation {
                    visibility: 1,
                    type_idx: 7,
                    elements: vec![(9, DexValue::String(0))],
                })])),
                None,
                None,
                None,
            ],
            methods: vec![None, None],
            parameters: vec![vec![], vec![]],
        }),
    );
    c.fields.push(DexField {
        declaring_type: c.descriptor.clone(),
        name: "$ENTRIES".into(),
        field_type: "Lsample/EntryList;".into(),
        access_flags: 0x101a,
        is_static: true,
    });
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions.pop();
    code.instructions
        .extend([0x0122, 4, 0x2070, 2, 0x0001, 0x0169, 3, 0x000e]);
    if tail {
        let symbols = Arc::get_mut(&mut c.symbols).unwrap();
        symbols.types.push("Lsample/Companion;".into());
        symbols.strings.push("Companion".into());
        symbols.methods.push((8, 3, 5));
        symbols.fields.push((0, 8, 10));
        c.fields.push(DexField {
            declaring_type: c.descriptor.clone(),
            name: "Companion".into(),
            field_type: "Lsample/Companion;".into(),
            access_flags: 0x19,
            is_static: true,
        });
        let words = &mut c.methods[0].code.as_mut().unwrap().instructions;
        words.pop();
        words.extend([0x0022, 8, 0x1070, 4, 0, 0x0069, 5, 0x000e]);
    }
    let symbols = c.symbols.clone();
    let entries = DexClass {
        descriptor: "Lsample/Entries;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec!["Lsample/EntryList;".into()],
        access_flags: 0x11,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![DexField {
            declaring_type: "Lsample/Entries;".into(),
            name: "items".into(),
            field_type: "[Ljava/lang/Enum;".into(),
            access_flags: 0x11,
            is_static: false,
        }],
        methods: vec![DexMethod {
            declaring_type: "Lsample/Entries;".into(),
            name: "<init>".into(),
            return_type: "V".into(),
            parameters: vec!["[Ljava/lang/Enum;".into()],
            thrown_types: vec![],
            access_flags: 0x10001,
            code: Some(DexCode {
                registers: 2,
                ins: 2,
                outs: 1,
                tries: 0,
                try_regions: vec![],
                instructions: vec![0x1070, 3, 0, 0x015b, 4, 0x000e],
                offset: 0,
            }),
        }],
        symbols: symbols.clone(),
    };
    let interface = DexClass {
        descriptor: "Lsample/EntryList;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 0x601,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![],
        symbols: symbols.clone(),
    };
    let mut world = vec![c, entries, interface];
    if tail {
        world.push(DexClass {
            descriptor: "Lsample/Companion;".into(),
            superclass: Some("Ljava/lang/Object;".into()),
            interfaces: vec![],
            access_flags: 0x11,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            symbols: symbols.clone(),
            methods: vec![DexMethod {
                declaring_type: "Lsample/Companion;".into(),
                name: "<init>".into(),
                return_type: "V".into(),
                parameters: vec![],
                thrown_types: vec![],
                access_flags: 0x10001,
                code: Some(DexCode {
                    registers: 1,
                    ins: 1,
                    outs: 1,
                    tries: 0,
                    try_regions: vec![],
                    offset: 0,
                    instructions: vec![0x1070, 3, 0, 0x000e],
                }),
            }],
        });
    }
    symbols
        .hierarchy
        .set(Arc::new(TypeHierarchy::from_classes(world.iter()).unwrap()))
        .unwrap();
    world
}

#[test]
fn erased_annotated_enum_keeps_explicit_backing_array_and_raw_entries_constructor() {
    let w = world();
    assert!(retainer::collect(&w.iter().collect::<Vec<_>>()).contains("Lsample/Entries;"));
    let out = native_java::render("sample.Mode", &w[0]).unwrap();
    assert!(out.source.contains("enum Mode"), "{}", out.source);
    assert!(out.source.contains("@Tag"), "{}", out.source);
    assert!(
        out.source.contains("$VALUES = new Mode[]"),
        "{}",
        out.source
    );
    assert!(
        out.source.contains("new Entries($VALUES)"),
        "{}",
        out.source
    );
    assert!(!out.source.contains("enumEntries(values())"));
    assert!(
        out.links
            .iter()
            .any(|l| l.label == "sample.Entries.<init>([Ljava/lang/Enum;)V")
    );
}

#[test]
fn private_enum_postfix_retains_companion_allocation_assignment_and_navigation() {
    let w = world_with_tail(true);
    let out = native_java::render("sample.Mode", &w[0]).unwrap();
    // The field shares the type's simple name, so readable source may retain
    // its qualification to make that declaration unambiguous.
    let source = out.source.replace("sample.Companion", "Companion");
    assert!(
        source.contains("public static final Companion Companion;"),
        "{}",
        out.source
    );
    assert!(source.contains("new Companion("), "{}", out.source);
    assert!(source.find("new Entries(").unwrap() < source.find("new Companion(").unwrap());
    assert!(
        out.links
            .iter()
            .any(|link| link.label == "sample.Companion.<init>()V")
    );
}

#[test]
fn enum_postfix_cannot_reuse_prefix_temporaries_or_mutate_core_storage() {
    for mutation in 0..3 {
        let mut w = world_with_tail(true);
        let c = &mut w[0];
        let words = &mut c.methods[0].code.as_mut().unwrap().instructions;
        let start = words.len() - 8;
        match mutation {
            0 => {
                words[start] = 0x0000;
                words[start + 1] = 0x0000;
            }
            1 => words[start + 6] = 2,
            2 => {
                words[start + 2] = 0x0028;
            }
            _ => unreachable!(),
        }
        assert!(
            native_java::render("sample.Mode", c).is_err(),
            "mutation {mutation}"
        );
    }
}

#[test]
fn retainer_accepts_object_delegation_through_proven_empty_erased_ancestors() {
    for mutation in 0..3 {
        let mut w = world();
        w[1].superclass = Some("Lsample/EmptyForwarder;".into());
        let symbols = Arc::new(DexSymbols {
            types: vec!["Ljava/lang/Object;".into()],
            strings: vec!["<init>".into()],
            protos: vec![("V".into(), vec![])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        let mut forwarder = DexClass {
            descriptor: "Lsample/EmptyForwarder;".into(),
            superclass: Some("Lsample/ErasedBase;".into()),
            interfaces: vec![],
            access_flags: 0x401,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            symbols: symbols.clone(),
            methods: vec![DexMethod {
                declaring_type: "Lsample/EmptyForwarder;".into(),
                name: "<init>".into(),
                return_type: "V".into(),
                parameters: vec![],
                thrown_types: vec![],
                access_flags: 0x10001,
                code: Some(DexCode {
                    registers: 1,
                    ins: 1,
                    outs: 1,
                    tries: 0,
                    try_regions: vec![],
                    offset: 0,
                    instructions: vec![0x1070, 0, 0, 0x000e],
                }),
            }],
        };
        if mutation == 1 {
            forwarder.fields.push(DexField {
                declaring_type: forwarder.descriptor.clone(),
                name: "state".into(),
                field_type: "I".into(),
                access_flags: 1,
                is_static: false,
            });
        }
        if mutation == 2 {
            forwarder.methods[0].code.as_mut().unwrap().instructions[0] = 0x1071;
        }
        let erased = DexClass {
            descriptor: "Lsample/ErasedBase;".into(),
            superclass: Some("Ljava/lang/Object;".into()),
            interfaces: vec![],
            access_flags: 0x401,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            methods: vec![],
            symbols,
        };
        w.extend([forwarder, erased]);
        assert_eq!(
            retainer::collect(&w.iter().collect::<Vec<_>>()).contains("Lsample/Entries;"),
            mutation == 0
        );
    }
}

#[test]
fn entries_publication_mutation_wrong_order_and_suppressed_annotations_decline() {
    let mut w = world();
    w[1].methods[0].code.as_mut().unwrap().instructions[3] = 0x025b;
    assert!(!retainer::collect(&w.iter().collect::<Vec<_>>()).contains("Lsample/Entries;"));
    for mutation in 0..4 {
        let mut w = world();
        let c = &mut w[0];
        match mutation {
            0 => c.methods.push(DexMethod {
                declaring_type: c.descriptor.clone(),
                name: "expose".into(),
                return_type: "Lsample/EntryList;".into(),
                parameters: vec![],
                thrown_types: vec![],
                access_flags: 9,
                code: Some(DexCode {
                    registers: 1,
                    ins: 0,
                    outs: 0,
                    tries: 0,
                    try_regions: vec![],
                    instructions: vec![0x0062, 3, 0x0011],
                    offset: 0,
                }),
            }),
            1 => c.methods[0].code.as_mut().unwrap().instructions[22] = 0x0001,
            2 => c.fields[3].access_flags = 0x1019,
            3 => c.fields[2].access_flags = 0x19,
            _ => unreachable!(),
        }
        assert!(
            native_java::render("sample.Mode", c).is_err(),
            "mutation{mutation}"
        );
    }
}

#[test]
#[ignore = "root runs javac/java serially with RDX_ENUM_JVM_DIR"]
fn jvm_explicit_original_array_reaches_entries_constructor_and_annotations_survive() {
    let w = world();
    let source = native_java::render("sample.Mode", &w[0]).unwrap().source;
    let dir =
        std::path::PathBuf::from(std::env::var_os("RDX_ENUM_JVM_DIR").expect("RDX_ENUM_JVM_DIR"));
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    std::fs::write(dir.join("sample/Mode.java"), source).unwrap();
    std::fs::write(dir.join("sample/Check.java"),r#"package sample;
@java.lang.annotation.Retention(java.lang.annotation.RetentionPolicy.RUNTIME) @interface Tag {String value();}
interface EntryList {}
final class Entries implements EntryList {public final Enum[] items;Entries(Enum[] array){items=array;}}
public class Check {public static void main(String[] args)throws Exception {
Mode[] first=Mode.values(),second=Mode.values();if(first==second||first.length!=2||first[0]!=Mode.ONE||first[1]!=Mode.TWO)throw new AssertionError();
java.lang.reflect.Field backing=Mode.class.getDeclaredField("$VALUES");backing.setAccessible(true);
java.lang.reflect.Field entries=Mode.class.getDeclaredField("$ENTRIES");entries.setAccessible(true);
if(((Entries)entries.get(null)).items!=backing.get(null))throw new AssertionError("array identity");
if(!Mode.class.getField("ONE").getAnnotation(Tag.class).value().equals("ONE"))throw new AssertionError("annotation");
if(!Mode.ONE.name().equals("ONE")||Mode.TWO.ordinal()!=1||Mode.valueOf("ONE")!=Mode.ONE)throw new AssertionError();System.out.print("ok");}}
"#).unwrap();
    for (program, args) in [
        ("javac", vec!["sample/Mode.java", "sample/Check.java"]),
        ("java", vec!["-cp", ".", "sample.Check"]),
    ] {
        let result = Command::new(program)
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        std::fs::write(dir.join(format!("{program}.log")), &result.stderr).unwrap();
        assert!(
            result.status.success(),
            "{program}:{}",
            String::from_utf8_lossy(&result.stderr)
        );
        if program == "java" {
            assert_eq!(result.stdout, b"ok");
        }
    }
}

#[test]
#[ignore = "root runs javac/java serially with RDX_ENUM_JVM_DIR"]
fn jvm_enum_postfix_preserves_constructor_order_identity_and_failure() {
    let world = world_with_tail(true);
    let class = &world[0];
    let source = native_java::render("sample.Mode", class).unwrap().source;
    // A literal interpreter for this straight-line fixture's complete raw
    // instruction stream independently extracts its observable hook order.
    let words = &class.methods[0].code.as_ref().unwrap().instructions;
    let mut pc = 0;
    let mut expected = String::new();
    while pc < words.len() {
        let width = match words[pc] as u8 {
            0x12 | 0x0c | 0x0e => 1,
            0x22 | 0x1a | 0x69 => 2,
            0x24 => 3,
            0x70 => {
                let (owner, _, name) = class.symbols.methods[usize::from(words[pc + 1])];
                assert_eq!(class.symbols.strings[name as usize], "<init>");
                match class.symbols.types[owner as usize].as_ref() {
                    "Ljava/lang/Enum;" => {}
                    "Lsample/Entries;" => expected.push_str("entries;"),
                    "Lsample/Companion;" => expected.push_str("companion;"),
                    other => panic!("unsupported literal constructor {other}"),
                }
                3
            }
            opcode => panic!("unsupported literal opcode {opcode:x} at {pc}"),
        };
        pc += width;
    }
    assert_eq!(expected, "entries;companion;");
    let dir =
        std::path::PathBuf::from(std::env::var_os("RDX_ENUM_JVM_DIR").expect("RDX_ENUM_JVM_DIR"))
            .join("postfix");
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    std::fs::write(dir.join("sample/Mode.java"), source).unwrap();
    std::fs::write(
        dir.join("raw-words.json"),
        serde_json::to_string(words).unwrap(),
    )
    .unwrap();
    std::fs::write(dir.join("sample/Check.java"), r#"package sample;
@java.lang.annotation.Retention(java.lang.annotation.RetentionPolicy.RUNTIME) @interface Tag {String value();}
interface EntryList {}
final class Hooks {static String trace="";static boolean fail;static final RuntimeException marker=new RuntimeException();static Companion created;}
final class Entries implements EntryList {final Enum[] items;Entries(Enum[] array){items=array;if(array.length!=2||array[0]!=Mode.ONE||array[1]!=Mode.TWO)throw new AssertionError("entries input");Hooks.trace+="entries;";}}
final class Companion {Companion(){
if(Mode.Companion!=null||Mode.values()[0]!=Mode.ONE||Mode.values()[1]!=Mode.TWO)throw new AssertionError("publication");
Hooks.trace+="companion;";Hooks.created=this;if(Hooks.fail)throw Hooks.marker;
}}
public class Check {public static void main(String[] args)throws Exception {
Hooks.fail=Boolean.parseBoolean(args[0]);Throwable caught=null;try{Object value=Mode.Companion;}catch(Throwable failure){caught=failure;}
if(!Hooks.trace.equals(args[1]))throw new AssertionError(Hooks.trace);
if(Hooks.fail){if(!(caught instanceof ExceptionInInitializerError)||caught.getCause()!=Hooks.marker)throw new AssertionError("exception identity");
try{Object value=Mode.Companion;throw new AssertionError("initialization retried");}catch(NoClassDefFoundError expected){}
if(!Hooks.trace.equals(args[1]))throw new AssertionError("effects repeated");
}else{if(caught!=null||Mode.Companion!=Hooks.created)throw new AssertionError("companion identity");
java.lang.reflect.Field entries=Mode.class.getDeclaredField("$ENTRIES");entries.setAccessible(true);
java.lang.reflect.Field array=Mode.class.getDeclaredField("$VALUES");array.setAccessible(true);
if(((Entries)entries.get(null)).items!=array.get(null))throw new AssertionError("array identity");
if(Mode.values()==Mode.values()||Mode.valueOf("TWO")!=Mode.TWO)throw new AssertionError("enum API");}
System.out.print("ok");}}
"#).unwrap();
    let compile = Command::new("javac")
        .args(["sample/Mode.java", "sample/Check.java"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    for fail in ["false", "true"] {
        let run = Command::new("java")
            .args(["-cp", ".", "sample.Check", fail, &expected])
            .current_dir(&dir)
            .output()
            .unwrap();
        std::fs::write(dir.join(format!("java-{fail}.log")), &run.stderr).unwrap();
        assert!(
            run.status.success(),
            "failure={fail}: {}",
            String::from_utf8_lossy(&run.stderr)
        );
        assert_eq!(run.stdout, b"ok");
    }
}

#[test]
#[ignore = "root runs pinned Airbnb APK serially"]
fn airbnb_optimized_kotlin_enum_initializers_render() {
    use rdx::{engine::DecompilerEngine, native_engine::NativeDexEngine};
    let path = std::env::var_os("RDX_TEST_APK").expect("RDX_TEST_APK");
    let mut engine = NativeDexEngine::default();
    engine.open(std::path::Path::new(&path)).unwrap();
    for name in [
        "com.airbnb.android.args.fov.models.ChipPosition",
        "com.airbnb.android.args.fov.models.ConfirmLegalNameScreenPresentationStyle",
        "com.airbnb.android.args.fov.models.LeadingButtonStyle",
        "com.airbnb.android.data.explore.searchparams.ValueType",
    ] {
        let c = engine.class(name).unwrap();
        let out = native_java::render(name, c).unwrap_or_else(|error| panic!("{name}: {error:#}"));
        assert!(out.source.contains("enum "), "{}", out.source);
        if name == "com.airbnb.android.data.explore.searchparams.ValueType" {
            assert!(out.source.contains("Companion"), "{}", out.source);
            assert!(
                out.links
                    .iter()
                    .any(|link| link.label == format!("{name}.Companion:Lrwx1;")),
                "{}",
                out.source
            );
            assert!(
                out.links
                    .iter()
                    .any(|link| link.label == "java.lang.Object.<init>()V"),
                "{}",
                out.source
            );
        }
        assert!(
            out.links
                .iter()
                .any(|link| link.label == "kotlin.enums.b.<init>([Ljava/lang/Enum;)V"),
            "{}",
            out.source
        );
    }
}
