//! A shared reachable default is not a join when success bypasses it.
use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_java,
};
use std::{process::Command, sync::Arc};

fn fixture(adjacent: bool) -> DexClass {
    let (words, ranges) = if adjacent {
        (
            vec![
                0x1071, 0, 3, 0x010c, 0x0428, // fetch0/result1; normal goto nexttry8
                0x020d, 0x0112, 0, // ignored first catch5 assigns null; enters nexttry8
                0x1071, 2, 1, 0x010c, 0x0428, // independent fallback8/result1; goto common16
                0x020d, 0x0112, 0, // ignored fallbackcatch13 assignsnull
                0x1071, 1, 1, 0x000a, 0x000f, // consume16 outside both catches
            ],
            vec![(0, 4, 5), (8, 12, 13)],
        )
    } else {
        (
            vec![
                0x1071, 0, 3, 0x000c, // fetch0/result0
                0x0038, 6, 0x0107, 0x0428, // null ->default10; successcopy +goto truejoin11
                0x020d, 0, 0x0112, // ignored catch8 ->default10
                0x1071, 1, 1, 0x000a, 0x000f, // consume11 outside originalcatch
            ],
            vec![(0, 4, 8)],
        )
    };
    DexClass {
        descriptor: "Lsample/ClosedJoin;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            types: vec!["Lsample/Effects;".into()],
            strings: ["fetch", "consume", "fallback"]
                .into_iter()
                .map(String::from)
                .collect(),
            protos: vec![
                (
                    "Ljava/lang/Object;".into(),
                    vec!["Ljava/lang/Object;".into()],
                ),
                ("I".into(), vec!["Ljava/lang/Object;".into()]),
            ],
            methods: vec![(0, 0, 0), (0, 1, 1), (0, 0, 2)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/ClosedJoin;".into(),
            name: if adjacent {
                "adjacent".into()
            } else {
                "run".into()
            },
            return_type: "I".into(),
            parameters: vec!["Ljava/lang/Object;".into()],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 4,
                ins: 1,
                outs: 1,
                tries: ranges.len() as u16,
                offset: 0,
                instructions: words,
                try_regions: ranges
                    .into_iter()
                    .map(|(start, end, handler)| DexTryRegion {
                        start,
                        end,
                        catches: vec![(Some("Ljava/lang/RuntimeException;".into()), handler)]
                            .into(),
                    })
                    .collect(),
            }),
        }],
    }
}

fn source(class: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.ClosedJoin", class, &class.methods[0])?.source)
}

#[test]
fn success_bypasses_the_default_and_reaches_one_closed_external_join() {
    for adjacent in [false, true] {
        let class = fixture(adjacent);
        let code = class.methods[0].code.as_ref().unwrap();
        let words = code.instructions.clone();
        let regions = code.try_regions.clone();
        let text = source(&class).unwrap_or_else(|error| panic!("adjacent={adjacent}: {error:#}"));
        assert_eq!(text.matches("Effects.fetch(").count(), 1, "{text}");
        assert_eq!(text.matches("Effects.consume(").count(), 1, "{text}");
        assert_eq!(
            text.matches("catch (").count(),
            if adjacent { 2 } else { 1 },
            "{text}"
        );
        if adjacent {
            assert_eq!(text.matches("Effects.fallback(").count(), 1, "{text}");
        }
        assert_eq!(class.methods[0].code.as_ref().unwrap().instructions, words);
        assert_eq!(class.methods[0].code.as_ref().unwrap().try_regions, regions);
    }
}

#[test]
fn effectful_handler_reentry_keeps_the_original_protected_region_guard() {
    let mut class = fixture(false);
    let code = class.methods[0].code.as_mut().unwrap();
    code.instructions.splice(9..9, [0x1071, 0, 3]); // outsidehandler fetch effect before reentry
    code.instructions[5] = 9; // null now targetsdefault13
    code.instructions[7] = 0x0728; // success bypassesdefault tojoin14
    code.instructions[12] = 0xf428; // handler jumps back into protectedfetch0
    rdx::native_cfg::ControlFlowGraph::build(code).unwrap();
    assert!(
        source(&class).is_err(),
        "effectful handler reentry was mistaken for a closed join"
    );
}

#[test]
#[ignore = "requires javac/java; emitted methods compiled unchanged"]
fn jvm_closed_catch_join_preserves_nullable_identity_paths_and_dispatch() {
    let primary = source(&fixture(false)).unwrap();
    let adjacent = source(&fixture(true)).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-closed-catch-join-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    let file = dir.join("sample/ClosedJoin.java");
    std::fs::write(
        &file,
        HARNESS.replace("METHODS", &format!("{primary}\n{adjacent}")),
    )
    .unwrap();
    let home = std::env::var("RDX_JAVA25_HOME").ok();
    let executable = |name: &str| {
        home.as_ref()
            .map_or_else(|| name.to_string(), |home| format!("{home}/bin/{name}"))
    };
    let compile = Command::new(executable("javac"))
        .arg(&file)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let result = Command::new(executable("java"))
        .args(["-cp", dir.to_str().unwrap(), "sample.ClosedJoin"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

const HARNESS: &str = r#"package sample;
public class ClosedJoin{
METHODS
static int expected(Object input,boolean adjacent){
 Object value=null;
 try{Object found=Effects.fetch(input);if(found!=null)value=found;}catch(RuntimeException ignored){}
 if(adjacent){try{value=Effects.fallback(value);}catch(RuntimeException ignored){value=null;}}
 return Effects.consume(value);
}
public static void main(String[]args){
 int checks=0;Object[] inputs={null,new String("equal contents"),new String("equal contents")};
 for(boolean next:new boolean[]{false,true})for(Object input:inputs)for(int mode=0;mode<=12;mode++){
  int[] result={-1,-1};Throwable[] fault=new Throwable[2];String[] trace=new String[2];
  for(int side=0;side<2;side++){
   Effects.reset(input,mode,next);
   try{result[side]=side==0?(next?adjacent(input):run(input)):expected(input,next);}catch(Throwable caught){fault[side]=caught;}
   trace[side]=Effects.trace;
  }
  if(result[0]!=result[1]||fault[0]!=fault[1]||!trace[0].equals(trace[1]))throw new AssertionError(next+":"+mode+":"+trace[0]+"/"+trace[1],fault[0]);
  checks++;
 }
 System.out.println("checks="+checks);
}
}
class Effects{
 static final RuntimeException body=new RuntimeException("body"),external=new RuntimeException("external");
 static final Error error=new AssertionError("identity");static final java.io.IOException checked=new java.io.IOException("identity");
 static int mode;static Object expected;static String trace;
 static void reset(Object input,int selected,boolean adjacent){mode=selected;expected=mode==1||adjacent&&mode==10?null:input;trace="";}
 @SuppressWarnings("unchecked")static<T extends Throwable>void raise(Throwable fault)throws T{throw(T)fault;}
 static Object fetch(Object input){trace+="F";if(mode==1)throw body;if(mode==2)throw error;if(mode==3)Effects.<RuntimeException>raise(checked);return input;}
 static Object fallback(Object input){trace+="N";if(mode==10)throw body;if(mode==11)throw error;if(mode==12)Effects.<RuntimeException>raise(checked);return input;}
 static int consume(Object value){trace+="C";if(value!=expected)throw new AssertionError("nullable reference identity");if(mode==4||mode>=7&&mode<=9)throw external;if(mode==5)throw error;if(mode==6)Effects.<RuntimeException>raise(checked);return value==null?0:17;}
}
"#;
