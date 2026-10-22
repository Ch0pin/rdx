//! An unprotected boundary jump keeps a shared backward tail outside its catch.
use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_java,
};
use std::{process::Command, sync::Arc};

fn fixture(other_catch: bool) -> DexClass {
    let mut class = DexClass {
        descriptor: "Lsample/Boundary;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            types: vec!["Lsample/Effects;".into()],
            strings: ["body", "caught", "continuation", "externalCaught"]
                .into_iter()
                .map(String::from)
                .collect(),
            protos: vec![
                ("I".into(), vec!["I".into()]),
                ("I".into(), vec!["Ljava/lang/Throwable;".into()]),
                ("I".into(), vec!["I".into(), "I".into()]),
            ],
            methods: vec![(0, 0, 0), (0, 1, 1), (0, 2, 2), (0, 1, 3)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/Boundary;".into(),
            name: if other_catch {
                "independent".into()
            } else {
                "run".into()
            },
            return_type: "I".into(),
            parameters: vec!["I".into(), "Z".into()],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 5,
                ins: 2,
                outs: 2,
                tries: 1,
                offset: 0,
                instructions: vec![
                    0x3101, 0x0438, 6, // seedresult; bypass chooses shared3 vsbody7
                    0x0012, 0x1201, 0x0029, 13, // shared3 zero; copyresult; goto join18
                    0x1071, 0, 3, 0x010a, // original protected body7..11 has no branch
                    0xf828, // unprotected boundary11 -> shared3
                    0x020d, 0x1071, 1, 2, 0x000a,
                    0x000f, // handler12 exactfault -> callback/return
                    0x2071, 2, 0x0020, 0x000a,
                    0x000f, // join18 continuation(zero,copied)/return
                ],
                try_regions: vec![DexTryRegion {
                    start: 7,
                    end: 11,
                    catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 12)].into(),
                }],
            }),
        }],
    };
    if other_catch {
        let code = class.methods[0].code.as_mut().unwrap();
        code.instructions
            .extend_from_slice(&[0x020d, 0x1071, 3, 2, 0x000a, 0x000f]);
        let catches = vec![(Some("Ljava/lang/RuntimeException;".into()), 23)].into();
        code.try_regions.insert(
            0,
            DexTryRegion {
                start: 3,
                end: 7,
                catches,
            },
        );
        code.try_regions.push(DexTryRegion {
            start: 18,
            // Keep the invoke/result pair together. Only the invoke can throw;
            // covering its pure result preserves the same dispatch semantics.
            end: 22,
            catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 23)].into(),
        });
        code.tries = 3;
    }
    class
}

fn source(class: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.Boundary", class, &class.methods[0])?.source)
}

#[test]
fn original_unprotected_backward_boundary_preserves_one_external_continuation() {
    for other_catch in [false, true] {
        let class = fixture(other_catch);
        let code = class.methods[0].code.as_ref().unwrap();
        let words = code.instructions.clone();
        let regions = code.try_regions.clone();
        let text =
            source(&class).unwrap_or_else(|error| panic!("other_catch={other_catch}: {error:#}"));
        for call in ["Effects.body(", "Effects.continuation("] {
            assert_eq!(text.matches(call).count(), 1, "{text}");
        }
        assert!(
            text.find("catch (").unwrap() < text.find("Effects.continuation(").unwrap(),
            "{text}"
        );
        assert_eq!(class.methods[0].code.as_ref().unwrap().instructions, words);
        assert_eq!(class.methods[0].code.as_ref().unwrap().try_regions, regions);
    }
}

#[test]
fn cyclic_or_handler_entering_jumps_cannot_be_flattened_as_an_acyclic_tail() {
    let mut class = fixture(false);
    class.methods[0].code.as_mut().unwrap().instructions[11] = 0xfc28; // actual body7 cycle
    if let Ok(text) = source(&class) {
        assert!(
            text.contains("while"),
            "real repeated body flattened:\n{text}"
        );
    }
    for word in [0x0128, 0xfd28] {
        // handler12 or protected invoke argument8
        let mut class = fixture(false);
        class.methods[0].code.as_mut().unwrap().instructions[11] = word;
        assert!(
            source(&class).is_err(),
            "invalid boundary target {word:04x}"
        );
    }
}

#[test]
fn another_original_try_owner_on_the_boundary_blocks_the_unprotected_cut() {
    let mut class = fixture(false);
    let code = class.methods[0].code.as_mut().unwrap();
    code.try_regions.push(DexTryRegion {
        start: 11,
        end: 12,
        catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 12)].into(),
    });
    code.tries += 1;
    rdx::native_cfg::ControlFlowGraph::build(code).unwrap();
    assert!(
        source(&class).is_err(),
        "a boundary owned by another original try was treated as unprotected"
    );
}

#[test]
#[ignore = "requires javac/java; emitted methods compiled unchanged"]
fn jvm_boundary_trampolines_preserve_original_and_independent_catch_dispatch() {
    let primary = source(&fixture(false)).unwrap();
    let independent = source(&fixture(true)).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-protected-boundary-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    let file = dir.join("sample/Boundary.java");
    std::fs::write(
        &file,
        HARNESS.replace("METHODS", &format!("{primary}\n{independent}")),
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
        "{}\n{primary}\n{independent}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let result = Command::new(executable("java"))
        .args(["-cp", dir.to_str().unwrap(), "sample.Boundary"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

const HARNESS: &str = r#"package sample;
public class Boundary{
METHODS
static int expected(int input,boolean bypass,boolean independent){
 int value=input;
 if(!bypass){try{value=Effects.body(input);}catch(RuntimeException fault){return Effects.caught(fault);}}
 if(independent){try{return Effects.continuation(0,value);}catch(RuntimeException fault){return Effects.externalCaught(fault);}}
 return Effects.continuation(0,value);
}
public static void main(String[]args){
 int checks=0;
 for(boolean independent:new boolean[]{false,true})for(boolean bypass:new boolean[]{false,true})for(int input=-2;input<=3;input++)for(int mode=0;mode<=12;mode++){
  int[] result={-1,-1};Throwable[] fault=new Throwable[2];String[] trace=new String[2];
  for(int side=0;side<2;side++){
   Effects.reset(mode);
   try{result[side]=side==0?(independent?independent(input,bypass):run(input,bypass)):expected(input,bypass,independent);}
   catch(Throwable caught){fault[side]=caught;}
   trace[side]=Effects.trace;
  }
  if(result[0]!=result[1]||fault[0]!=fault[1]||!trace[0].equals(trace[1]))throw new AssertionError(independent+":"+bypass+":"+input+":"+mode+":"+trace[0]+"/"+trace[1],fault[0]);
  checks++;
 }
 System.out.println("checks="+checks);
}
}
class Effects{
 static final RuntimeException original=new RuntimeException("original"),external=new RuntimeException("external"),callback=new RuntimeException("callback");
 static final Error error=new AssertionError("identity");static final java.io.IOException checked=new java.io.IOException("identity");
 static int mode;static String trace;
 static void reset(int selected){mode=selected;trace="";}
 @SuppressWarnings("unchecked")static<T extends Throwable>void raise(Throwable fault)throws T{throw(T)fault;}
 static int body(int input){trace+="B"+input+",";if(mode==1||mode==4||mode==8||mode==9)throw original;if(mode==2)throw error;if(mode==3)Effects.<RuntimeException>raise(checked);return input+100;}
 static int caught(Throwable fault){trace+="H,";if(fault!=original)throw new AssertionError("body identity",fault);if(mode==4)throw callback;if(mode==8)throw error;if(mode==9)Effects.<RuntimeException>raise(checked);return -29;}
 static int continuation(int zero,int value){trace+="C"+zero+":"+value+",";if(zero!=0)throw new AssertionError("lost shared zero");if(mode==5||mode>=10)throw external;if(mode==6)throw error;if(mode==7)Effects.<RuntimeException>raise(checked);return value*2+17;}
 static int externalCaught(Throwable fault){trace+="X,";if(fault!=external)throw new AssertionError("external identity",fault);if(mode==10)throw callback;if(mode==11)throw error;if(mode==12)Effects.<RuntimeException>raise(checked);return -71;}
}
"#;
