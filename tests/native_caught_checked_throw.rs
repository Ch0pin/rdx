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
    fs::write(
        base.join("sample/Caller.java"),
        native_java::render("sample.Caller", &class).unwrap().source,
    )
    .unwrap();
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
public static void main(String[] args){for(boolean p:new boolean[]{false,true})for(boolean c:new boolean[]{false,true})for(int x:new int[]{0,1,-1}){Hooks.primary=p;Hooks.cleanupFailure=c;String expected=run(false,x),actual=run(true,x);if(!actual.equals(expected))throw new AssertionError(expected+" != "+actual);}System.out.print("ok");}
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
