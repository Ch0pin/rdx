use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;
fn fixture() -> DexClass {
    let symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Hooks;".into()],
        strings: vec!["touch".into(), "cleanup".into()],
        protos: vec![("V".into(), vec![]), ("V".into(), vec!["I".into()])],
        methods: vec![(0, 0, 0), (0, 1, 1)],
        ..Default::default()
    });
    let root = DexClass {
        descriptor: "Lsample/Caller;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![DexMethod {
            declaring_type: "Lsample/Caller;".into(),
            name: "run".into(),
            parameters: vec!["I".into()],
            return_type: "V".into(),
            thrown_types: vec![],
            access_flags: 1,
            code: Some(DexCode {
                registers: 3,
                ins: 2,
                outs: 1,
                tries: 1,
                try_regions: vec![DexTryRegion {
                    start: 0,
                    end: 3,
                    catches: vec![(None, 9)].into(),
                }],
                instructions: vec![
                    0x0071, 0, 0, 0x0238, 5, 0x1071, 1, 2, 0x000e, 0x000d, 0x0238, 5, 0x1071, 1, 2,
                    0x0027,
                ],
                offset: 0,
            }),
        }],
        symbols: symbols.clone(),
    };
    let hooks = DexClass {
        descriptor: "Lsample/Hooks;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: ["touch", "cleanup"]
            .into_iter()
            .enumerate()
            .map(|(index, name)| DexMethod {
                declaring_type: "Lsample/Hooks;".into(),
                name: name.into(),
                parameters: if index == 0 { vec![] } else { vec!["I".into()] },
                return_type: "V".into(),
                access_flags: 0x109,
                code: None,
                thrown_types: if index == 0 {
                    vec!["Ljava/io/IOException;".into()]
                } else {
                    vec![]
                },
            })
            .collect(),
        symbols: symbols.clone(),
    };
    symbols
        .hierarchy
        .set(Arc::new(
            TypeHierarchy::from_classes([&root, &hooks]).unwrap(),
        ))
        .unwrap();
    root
}
fn joined_handlers_fixture() -> DexClass {
    let mut class = fixture();
    let code = class.methods[0].code.as_mut().unwrap();
    code.tries = 2;
    code.instructions = vec![
        0x0238, 8, 0x0071, 0, 0, 0x000e, 0x000d, 0x0628, 0x0071, 0, 0, 0x000e, 0x000d, 0x1071, 1,
        2, 0x0027,
    ];
    code.try_regions = vec![
        DexTryRegion {
            start: 2,
            end: 5,
            catches: vec![(None, 6)].into(),
        },
        DexTryRegion {
            start: 8,
            end: 11,
            catches: vec![(None, 12)].into(),
        },
    ];
    class
}

#[test]
fn shared_throw_tail_preserves_each_original_handler_value() {
    let class = joined_handlers_fixture();
    let source = native_java::render("sample.Caller", &class).unwrap().source;
    assert!(source.contains("throw _rdx_caught_throw_"), "{source}");
    assert_eq!(source.matches("Hooks.touch(").count(), 2, "{source}");
    // A parameter entering one side of the phi is not a captured exception.
    let mut bad = class;
    bad.methods[0].parameters = vec!["Ljava/lang/Throwable;".into()];
    bad.methods[0].code.as_mut().unwrap().instructions[12] = 0x2007;
    assert!(native_java::render_method("sample.Caller", &bad, &bad.methods[0]).is_err());
}

#[test]
fn checked_callee_header_keeps_original_handler_object_and_caller_header() {
    let class = fixture();
    let source = native_java::render("sample.Caller", &class).unwrap().source;
    assert!(source.contains("throw _rdx_caught_throw_"), "{source}");
    assert!(
        !source
            .lines()
            .find(|l| l.contains(" run("))
            .unwrap()
            .contains("throws"),
        "{source}"
    );
    assert!(
        source.contains("touch()") && source.contains("cleanup("),
        "{source}"
    );
}
#[test]
fn reassigned_parameter_and_primitive_handler_values_do_not_gain_caught_helper() {
    for replacement in [0x1007, 0x1012] {
        let mut class = fixture();
        class.methods[0].code.as_mut().unwrap().instructions[9] = replacement;
        assert!(native_java::render_method("sample.Caller", &class, &class.methods[0]).is_err());
    }
}
#[test]
fn legal_throwable_parameter_reassignment_retains_checked_refusal() {
    let mut class = fixture();
    let method = &mut class.methods[0];
    method.parameters = vec!["Ljava/lang/Throwable;".into(), "I".into()];
    let code = method.code.as_mut().unwrap();
    code.registers = 4;
    code.ins = 3;
    code.instructions[3] = 0x0338;
    code.instructions[7] = 3;
    code.instructions[10] = 0x0338;
    code.instructions[14] = 3;
    code.instructions.insert(15, 0x2007);
    assert!(native_java::render_method("sample.Caller", &class, &class.methods[0]).is_err());
}
#[test]
fn caught_reference_replaced_by_null_preserves_npe_without_helper() {
    let mut class = fixture();
    class.methods[0]
        .code
        .as_mut()
        .unwrap()
        .instructions
        .insert(15, 0x0012);
    let source = native_java::render("sample.Caller", &class).unwrap().source;
    assert!(source.contains("throw null;"), "{source}");
    assert!(!source.contains("_rdx_caught_throw_"), "{source}");
}
#[test]
fn normal_branch_entry_to_handler_is_rejected() {
    let mut class = fixture();
    class.methods[0].code.as_mut().unwrap().instructions[4] = 6;
    assert!(native_java::render_method("sample.Caller", &class, &class.methods[0]).is_err());
}
#[test]
#[ignore = "requires javac/java; root runs serially"]
fn jvm_same_caught_object_and_cleanup_replacement_match_literal_dex() {
    use std::{fs, process::Command};
    let base = std::env::temp_dir().join(format!("rdx-caught-checked-{}", std::process::id()));
    fs::create_dir_all(base.join("sample")).unwrap();
    let class = fixture();
    let mut source = native_java::render("sample.Caller", &class).unwrap().source;
    let mut joined = joined_handlers_fixture();
    joined.methods[0].name = "joined".into();
    let end = source.rfind('}').unwrap();
    source.insert_str(
        end,
        &native_java::render_method("sample.Caller", &joined, &joined.methods[0])
            .unwrap()
            .source,
    );
    fs::write(base.join("sample/Caller.java"), source).unwrap();
    fs::write(base.join("sample/Hooks.java"),r#"package sample;
final class Hooks {
static boolean primary,cleanupFailure;static String trace="";
static final java.io.IOException FIRST=new java.io.IOException("primary");
static final AssertionError SECOND=new AssertionError("cleanup");
@SuppressWarnings("unchecked") private static <E extends Throwable> void sneaky(Throwable x)throws E {throw(E)x;}
static void touch() throws java.io.IOException {trace+="T";if(primary)throw FIRST;}
static void cleanup(int ignored){trace+="C";if(cleanupFailure)throw SECOND;}
}"#).unwrap();
    fs::write(base.join("sample/Check.java"),r#"package sample;
public class Check {
static final int[] W={0x0071,0,0,0x0238,5,0x1071,1,2,0x000e,0x000d,0x0238,5,0x1071,1,2,0x0027};
static void raw(int x)throws Throwable {Object[] r={null,new Object(),x};Throwable pending=null;int pc=0;
for(int budget=0;budget<100;budget++){try{int w=W[pc];switch(w&255){
case 0x71:if(W[pc+1]==0)Hooks.touch();else if(W[pc+1]==1)Hooks.cleanup((Integer)r[W[pc+2]&15]);else throw new AssertionError();pc+=3;break;
case 0x38:pc+=((Integer)r[w>>>8])==0?(short)W[pc+1]:2;break;
case 0x0e:return;
case 0x0d:r[w>>>8]=pending;pending=null;pc++;break;
case 0x27:throw(Throwable)r[w>>>8];
default:throw new AssertionError("opcode");}}
catch(Throwable failure){if(pc>=0&&pc<3){pending=failure;pc=9;}else throw failure;}}
throw new AssertionError("budget");}
static String run(boolean compiled,int condition){Hooks.trace="";String result="normal";
try{if(compiled)new Caller().run(condition);else raw(condition);}catch(Throwable failure){if(failure==Hooks.FIRST)result="first";else if(failure==Hooks.SECOND)result="second";else throw new AssertionError("identity",failure);}return result+"|"+Hooks.trace;}
static void joinedChecks(){for(boolean p:new boolean[]{false,true})for(boolean c:new boolean[]{false,true})for(int x:new int[]{0,1,-1}){
Hooks.primary=p;Hooks.cleanupFailure=c;Hooks.trace="";Throwable got=null;
try{new Caller().joined(x);}catch(Throwable e){got=e;}
Throwable expected=p?(c?Hooks.SECOND:Hooks.FIRST):null;
if(got!=expected||!Hooks.trace.equals(p?"TC":"T"))throw new AssertionError("joined identity/trace");}}
public static void main(String[] args){joinedChecks();for(boolean p:new boolean[]{false,true})for(boolean c:new boolean[]{false,true})for(int x:new int[]{0,1,-1}){Hooks.primary=p;Hooks.cleanupFailure=c;String expected=run(false,x),actual=run(true,x);if(!actual.equals(expected))throw new AssertionError(expected+" != "+actual);}System.out.print("ok");}
}"#).unwrap();
    let out = Command::new("javac")
        .args([
            "-Xlint:all",
            "-Werror",
            "sample/Caller.java",
            "sample/Hooks.java",
            "sample/Check.java",
        ])
        .current_dir(&base)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = Command::new("java")
        .args(["-cp", ".", "sample.Check"])
        .current_dir(&base)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, b"ok");
}

fn cause_fixture(inherited: bool, private_lookup: bool) -> DexClass {
    let symbols = Arc::new(DexSymbols {
        types: vec![
            "Lsample/Hooks;".into(),
            "Ljava/lang/Throwable;".into(),
            "Ljava/lang/Class;".into(),
            "Lsample/Caller;".into(),
        ],
        strings: vec![
            "touch".into(),
            "getCause".into(),
            "forName".into(),
            "take".into(),
            "java.lang.Object".into(),
        ],
        protos: vec![
            ("V".into(), vec![]),
            ("Ljava/lang/Throwable;".into(), vec![]),
            (
                "Ljava/lang/Class;".into(),
                vec!["Ljava/lang/String;".into()],
            ),
            ("V".into(), vec!["Ljava/lang/String;".into()]),
        ],
        methods: vec![(0, 0, 0), (1, 1, 1), (2, 2, 2), (3, 3, 3)],
        ..Default::default()
    });
    let mut words = if inherited {
        vec![0x011a, 4, 0x206e, 3, 0x0012]
    } else if private_lookup {
        vec![0x011a, 4, 0x1071, 2, 1]
    } else {
        vec![]
    };
    let start = words.len() as u32;
    words.extend([
        0x0071, 0, 0, 0x000e, 0x000d, 0x106e, 1, 0, 0x010c, 0x0138, 3, 0x0127, 0x0027,
    ]);
    let class = |descriptor: &str, superclass: &str, methods| DexClass {
        descriptor: descriptor.into(),
        superclass: Some(superclass.into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods,
        symbols: symbols.clone(),
    };
    let root = class(
        "Lsample/Caller;",
        "Lsample/Parent;",
        vec![DexMethod {
            declaring_type: "Lsample/Caller;".into(),
            name: "run".into(),
            parameters: vec!["I".into()],
            return_type: "V".into(),
            thrown_types: vec![],
            access_flags: if private_lookup { 10 } else { 1 },
            code: Some(DexCode {
                registers: 4,
                ins: if private_lookup { 1 } else { 2 },
                outs: 2,
                tries: 1,
                offset: 0,
                instructions: words,
                try_regions: vec![DexTryRegion {
                    start,
                    end: start + 3,
                    catches: vec![(None, start + 4)].into(),
                }],
            }),
        }],
    );
    let hooks = class(
        "Lsample/Hooks;",
        "Ljava/lang/Object;",
        vec![DexMethod {
            declaring_type: "Lsample/Hooks;".into(),
            name: "touch".into(),
            parameters: vec![],
            return_type: "V".into(),
            thrown_types: vec!["Ljava/io/IOException;".into()],
            access_flags: 0x109,
            code: None,
        }],
    );
    let parent = class(
        "Lsample/Parent;",
        "Ljava/lang/Object;",
        ["Ljava/lang/String;", "Ljava/lang/Object;"]
            .into_iter()
            .map(|ty| DexMethod {
                declaring_type: "Lsample/Parent;".into(),
                name: "take".into(),
                parameters: vec![ty.into()],
                return_type: "V".into(),
                thrown_types: vec![],
                access_flags: 1,
                code: Some(DexCode {
                    registers: 2,
                    ins: 2,
                    outs: 0,
                    tries: 0,
                    try_regions: vec![],
                    instructions: vec![0x000e],
                    offset: 0,
                }),
            })
            .collect(),
    );
    symbols
        .hierarchy
        .set(Arc::new(
            TypeHierarchy::from_classes([&root, &parent, &hooks]).unwrap(),
        ))
        .unwrap();
    root
}

#[test]
fn caught_cause_and_inherited_overload_keep_the_original_method_header() {
    for (inherited, private_lookup) in [(false, false), (true, false), (false, true)] {
        let class = cause_fixture(inherited, private_lookup);
        let source = native_java::render("sample.Caller", &class).unwrap().source;
        assert!(
            source.contains("getCause()") && source.contains("throw _rdx_caught_throw_"),
            "{source}"
        );
        assert!(
            !source
                .lines()
                .find(|l| l.contains(" run("))
                .unwrap()
                .contains("throws"),
            "{source}"
        );
        if private_lookup {
            assert!(
                source.contains("_rdx_checked_") && source.contains("Class.forName(p0)"),
                "{source}"
            );
        }
        if inherited {
            assert!(source.contains("take(\"java.lang.Object\")"), "{source}");
        }
    }
}

#[test]
fn uncaught_cause_receiver_does_not_gain_a_caught_origin() {
    let mut class = cause_fixture(false, false);
    class.methods[0].parameters = vec!["Ljava/lang/Throwable;".into()];
    class.methods[0].code.as_mut().unwrap().instructions[7] = 3;
    assert!(native_java::render_method("sample.Caller", &class, &class.methods[0]).is_err());
}

#[test]
#[ignore = "requires javac/java; root runs serially"]
fn jvm_cause_rethrow_preserves_checked_error_and_null_cause_identity() {
    use std::{fs, process::Command};
    let base = std::env::temp_dir().join(format!("rdx-caught-cause-{}", std::process::id()));
    fs::create_dir_all(base.join("sample")).unwrap();
    fs::write(
        base.join("sample/Caller.java"),
        native_java::render("sample.Caller", &cause_fixture(true, false))
            .unwrap()
            .source,
    )
    .unwrap();
    fs::write(base.join("sample/Parent.java"), "package sample; public class Parent { public void take(String x) { if(!x.equals(\"java.lang.Object\"))throw new AssertionError(); } public void take(Object x){throw new AssertionError(\"wrong overload\");} }").unwrap();
    fs::write(base.join("sample/Hooks.java"), r#"package sample; final class Hooks {
static Throwable failure;
@SuppressWarnings("unchecked") static <E extends Throwable> void rethrow(Throwable x)throws E {throw(E)x;}
static void touch()throws java.io.IOException {if(failure!=null)rethrow(failure);}
}"#).unwrap();
    fs::write(base.join("sample/Check.java"), r#"package sample; public class Check { public static void main(String[] args) {
Throwable[] failures={new java.io.IOException(), new AssertionError(), new RuntimeException(new java.io.IOException()), new RuntimeException(new AssertionError())};
new Caller().run(0);
for(Throwable f:failures){Hooks.failure=f;Throwable expected=f.getCause()==null?f:f.getCause();try{new Caller().run(0);throw new AssertionError("no exception");}catch(Throwable actual){if(actual!=expected)throw new AssertionError("identity",actual);}}
System.out.print("ok");}}
"#).unwrap();
    let out = Command::new("javac")
        .args([
            "-Xlint:all",
            "-Werror",
            "sample/Caller.java",
            "sample/Parent.java",
            "sample/Hooks.java",
            "sample/Check.java",
        ])
        .current_dir(&base)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = Command::new("java")
        .args(["-cp", ".", "sample.Check"])
        .current_dir(&base)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, b"ok");
}
