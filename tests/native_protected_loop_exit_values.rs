//! Protected conditional-latch searches retain each backward exit's complete frame.
pub use rdx::{native_cfg, native_dex, native_ir};
use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_java,
};
use std::{process::Command, sync::Arc};
#[path = "../src/native_java/protected_pure_exits.rs"]
mod protected_pure_exits;

fn fixture() -> DexClass {
    DexClass {
        descriptor: "Lsample/Search;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            types: vec!["Lsample/Effects;".into()],
            strings: ["more", "kind", "accept", "exhausted", "caught", "finish"]
                .into_iter()
                .map(String::from)
                .collect(),
            protos: vec![
                ("Z".into(), vec!["I".into(), "I".into()]),
                ("I".into(), vec!["I".into()]),
                ("Z".into(), vec!["I".into()]),
                ("V".into(), vec![]),
                ("V".into(), vec!["Ljava/lang/Throwable;".into()]),
                (
                    "Ljava/lang/Object;".into(),
                    vec!["Z".into(), "Ljava/lang/Object;".into(), "J".into()],
                ),
            ],
            methods: vec![
                (0, 0, 0),
                (0, 1, 1),
                (0, 2, 2),
                (0, 3, 3),
                (0, 4, 4),
                (0, 5, 5),
            ],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/Search;".into(),
            name: "run".into(),
            return_type: "Ljava/lang/Object;".into(),
            parameters: vec![
                "I".into(),
                "Ljava/lang/Object;".into(),
                "Ljava/lang/Object;".into(),
                "J".into(),
                "J".into(),
            ],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 15,
                ins: 7,
                outs: 4,
                tries: 1,
                offset: 0,
                instructions: vec![
                    0x0928, // 0 -> original try9
                    0x0012, 0x9107, 0xb204, 0x2728, // 1 failure: false, p1, p3; goto43
                    0x1012, 0xa107, 0xd204, 0x2328, // 5 success: true, p2, p4; goto43
                    0x0412, // 9 i=0
                    0x2071, 0, 0x0084, 0x050a, // 10 more(i,p0); result13
                    0x0538, 17, // 14 exhausted ->31
                    0x1071, 1, 4, 0x050a, // 16 kind(i); result19
                    0x04d8, 0x0104, // 20 i++
                    0x053c, 0xfff4, // 22 conditional latch kind>0 ->10
                    0x1071, 2, 5, 0x050a, // 24 accept(kind); result27
                    0x0538, 0xffe5, 0xe728, // 28 false ->1; 30 true ->5
                    0x0071, 3, 0, 0xdf28, // 31 exhausted(); 34 ->1
                    0x060d, 0x1071, 4, 6, // 35 capture; 36 caught(exact) outside protection
                    0x0012, 0x9107, 0xb204, 0x0128, // 39 catch defaults; 42 ->43
                    0x4071, 5, 0x3210, 0x010c, 0x0111, // 43 finish(Z,Object,J); return47
                ],
                try_regions: vec![DexTryRegion {
                    start: 9,
                    end: 35,
                    catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 35)].into(),
                }],
            }),
        }],
    }
}

fn copy_code(code: &DexCode) -> DexCode {
    DexCode {
        registers: code.registers,
        ins: code.ins,
        outs: code.outs,
        tries: code.tries,
        offset: code.offset,
        instructions: code.instructions.clone(),
        try_regions: code.try_regions.clone(),
    }
}

fn source(class: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.Search", class, &class.methods[0])?.source)
}

#[test]
fn protected_search_merges_boolean_identity_and_complete_wide_exit_values() {
    let class = fixture();
    let original = fixture();
    let text = source(&class).unwrap_or_else(|error| panic!("{error:#}"));
    for call in [
        "Effects.more(",
        "Effects.kind(",
        "Effects.accept(",
        "Effects.exhausted(",
        "Effects.caught(",
        "Effects.finish(",
    ] {
        assert_eq!(text.matches(call).count(), 1, "{call}\n{text}");
    }
    assert_eq!(text.matches("while (true)").count(), 1, "{text}");
    assert!(
        text.find("catch (").unwrap() < text.find("Effects.finish(").unwrap(),
        "{text}"
    );
    let code = class.methods[0].code.as_ref().unwrap();
    let before = original.methods[0].code.as_ref().unwrap();
    assert_eq!(code.instructions, before.instructions);
    assert_eq!(code.try_regions, before.try_regions);
}

#[test]
fn pure_exit_certificate_pins_ownership_words_and_frame_and_rejects_effects_or_reentry() {
    let class = fixture();
    let code = class.methods[0].code.as_ref().unwrap();
    let region = &code.try_regions[0];
    let plan = protected_pure_exits::prove(code, region).expect("exact pure exit certificate");
    assert_eq!((plan.start, plan.end, plan.join), (9, 35, 43));
    assert!(plan.normal.contains(&22));
    assert!(!plan.normal.contains(&35));
    assert_eq!(plan.tails.keys().copied().collect::<Vec<_>>(), vec![1, 5]);
    assert!(plan.layout.contains(&(22, 2, Some(10))));
    assert!(plan.matches(code));

    let mut throwing = copy_code(code);
    throwing.instructions[1..4].copy_from_slice(&[0x0071, 3, 0]);
    assert!(protected_pure_exits::prove(&throwing, &throwing.try_regions[0]).is_none());
    for word in [0xfd28, 0x0628] {
        // tail cycle or reentry to loop header10
        let mut changed = copy_code(code);
        changed.instructions[4] = word;
        assert!(protected_pure_exits::prove(&changed, &changed.try_regions[0]).is_none());
    }
    let mut protected_tail = copy_code(code);
    protected_tail.tries = 2;
    protected_tail.try_regions.push(DexTryRegion {
        start: 1,
        end: 5,
        catches: region.catches.clone(),
    });
    assert!(protected_pure_exits::prove(&protected_tail, &protected_tail.try_regions[0]).is_none());
    let mut handler_reentry = copy_code(code);
    handler_reentry.instructions[42] = 0xdf28;
    assert!(
        protected_pure_exits::prove(&handler_reentry, &handler_reentry.try_regions[0]).is_none()
    );

    for field in 0..6 {
        let mut changed = copy_code(code);
        match field {
            0 => changed.registers += 1,
            1 => changed.ins -= 1,
            2 => changed.outs += 1,
            3 => changed.tries += 1,
            4 => changed.instructions[1] = 0x1012,
            _ => changed.try_regions[0].end = 34,
        }
        assert!(!plan.matches(&changed), "stale plan field {field}");
    }
}

#[test]
fn exit_values_require_defined_compatible_heads_and_complete_wide_pairs() {
    for (pc, word) in [(2, 0x7107), (2, 0x1112), (3, 0x0312), (3, 0xc204)] {
        let mut class = fixture();
        class.methods[0].code.as_mut().unwrap().instructions[pc] = word;
        assert!(
            source(&class).is_err(),
            "invalid exit at {pc} word={word:#x}"
        );
    }
    let mut class = fixture();
    // A catch may not loop back into the original protected search.
    class.methods[0].code.as_mut().unwrap().instructions[42] = 0xdf28;
    assert!(source(&class).is_err());
}

#[test]
#[ignore = "requires javac/java; emitted method compiled unchanged"]
fn jvm_protected_search_preserves_exit_frames_dispatch_and_fault_identity() {
    let text = source(&fixture()).unwrap();
    let dir = std::env::temp_dir().join(format!(
        "rdx-protected-loop-exit-values-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    let file = dir.join("sample/Search.java");
    std::fs::write(&file, HARNESS.replace("METHOD", &text)).unwrap();
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
        .args(["-cp", dir.to_str().unwrap(), "sample.Search"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

const HARNESS: &str = r#"package sample;
public class Search{
METHOD
static Object reference(int limit,Object fallback,Object success,long fallbackBits,long successBits){
 boolean selected=false;Object value=fallback;long bits=fallbackBits;
 try{
  int i=0;
  while(true){
   if(!Effects.more(i,limit)){Effects.exhausted();break;}
   int kind=Effects.kind(i);i++;
   if(kind>0)continue;
   if(Effects.accept(kind)){selected=true;value=success;bits=successBits;}
   break;
  }
 }catch(RuntimeException fault){Effects.caught(fault);selected=false;value=fallback;bits=fallbackBits;}
 return Effects.finish(selected,value,bits);
}
public static void main(String[]args){
 Object a=new String("same"),b=new String("same");
 Object[][] objects={{null,a},{a,null},{a,b}};
 long[][] pairs={{Long.MIN_VALUE,Long.MAX_VALUE},{0,0x123456789abcdef0L},{-1,0}};
 int checks=0;
 for(int limit:new int[]{-1,0,1,2,4})for(int scenario=0;scenario<5;scenario++)
 for(Object[] values:objects)for(long[] bits:pairs)for(int faultCase=0;faultCase<55;faultCase++){
  Object[] result=new Object[2];Throwable[] faults=new Throwable[2];String[] traces=new String[2];
  for(int side=0;side<2;side++){
   Effects.reset(scenario,faultCase);
   try{result[side]=side==0?run(limit,values[0],values[1],bits[0],bits[1]):reference(limit,values[0],values[1],bits[0],bits[1]);}
   catch(Throwable fault){faults[side]=fault;}
   traces[side]=Effects.trace;
  }
  if(result[0]!=result[1]||faults[0]!=faults[1]||!traces[0].equals(traces[1]))
   throw new AssertionError("limit="+limit+" scenario="+scenario+" fault="+faultCase+" "+traces[0]+" / "+traces[1],faults[0]);
  checks++;
 }
 System.out.println("checks="+checks);
}
}
class Effects{
 static int scenario,stage,occurrence,kind;static int[] calls;static String trace;
 static final Throwable[][] faults=new Throwable[6][3];
 static{for(int i=0;i<6;i++){faults[i][0]=new RuntimeException("runtime"+i);faults[i][1]=new AssertionError("error"+i);faults[i][2]=new java.io.IOException("checked"+i);}}
 static void reset(int selected,int faultCase){scenario=selected;stage=faultCase==0?-1:(faultCase-1)/9;occurrence=(faultCase-1)%9/3;kind=(faultCase-1)%3;calls=new int[6];trace="";}
 @SuppressWarnings("unchecked")static<T extends Throwable>void raise(Throwable fault)throws T{throw(T)fault;}
 static void event(int at,String text){trace+=text+";";int n=calls[at]++;if(stage==4&&at==0&&n==0)Effects.<RuntimeException>raise(faults[0][0]);if(stage==at&&occurrence==n)Effects.<RuntimeException>raise(faults[at][kind]);}
 static boolean more(int i,int limit){event(0,"more:"+i);return i<limit;}
 static int kind(int i){event(1,"kind:"+i);switch(scenario){case 0:return 1;case 1:return 0;case 2:return -1;case 3:return i<2?1:0;default:return i<2?1:-1;}}
 static boolean accept(int value){event(2,"accept:"+value);return value==0;}
 static void exhausted(){event(3,"exhausted");}
 static void caught(Throwable fault){event(4,"caught:"+System.identityHashCode(fault));}
 static Object finish(boolean selected,Object value,long bits){event(5,"finish:"+selected+":"+System.identityHashCode(value)+":"+bits);return value;}
}
"#;
