//! Guarded normal tails merge values copied from other unchanged entry registers.
use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_java,
};
use std::{process::Command, sync::Arc};

fn fixture(wide: bool) -> DexClass {
    let ty = if wide { "J" } else { "Ljava/lang/Object;" };
    let (words, registers, ins, start, end, handler) = if wide {
        (
            vec![
                0x0016, 0, // complete initial r0/r1 pair
                0x0071, 0, 0, // protected2..5 body()
                0x0071, 1, 0, 0x6004,
                0x0628, // normal touch5; r0/r1=param1(r6/r7); goto join15
                0x020d, 0x4004, 0x1071, 2, 2, // handler10; r0/r1=param0(r4/r5); caught(exact)
                0x2071, 3, 0x0010, 0x000b, 0x0010, // join15 finish(J)J; result/return
            ],
            8,
            4,
            2,
            5,
            10,
        )
    } else {
        (
            vec![
                0x0012, // initial r0=0 is a different physical entry lifetime
                0x0071, 0, 0, // protected1..4 body()
                0x0071, 1, 0, 0x4007, 0x0628, // normal touch4; r0=param1(r4); goto join14
                0x010d, 0x3007, 0x1071, 2, 1, // handler9; r0=param0(r3); caught(exact)
                0x1071, 3, 0, 0x000c, 0x0011, // join14 finish(Object)Object; result/return
            ],
            5,
            2,
            1,
            4,
            9,
        )
    };
    DexClass {
        descriptor: "Lsample/Alias;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            types: vec!["Lsample/Effects;".into()],
            strings: vec![
                "body".into(),
                "touch".into(),
                "caught".into(),
                if wide {
                    "finishWide".into()
                } else {
                    "finish".into()
                },
            ],
            protos: vec![
                ("V".into(), vec![]),
                ("V".into(), vec!["Ljava/lang/Throwable;".into()]),
                (ty.into(), vec![ty.into()]),
            ],
            methods: vec![(0, 0, 0), (0, 0, 1), (0, 1, 2), (0, 2, 3)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/Alias;".into(),
            name: if wide { "wide".into() } else { "run".into() },
            return_type: ty.into(),
            parameters: vec![ty.into(), ty.into()],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers,
                ins,
                outs: if wide { 2 } else { 1 },
                tries: 1,
                offset: 0,
                instructions: words,
                try_regions: vec![DexTryRegion {
                    start,
                    end,
                    catches: vec![(Some("Ljava/lang/RuntimeException;".into()), handler)].into(),
                }],
            }),
        }],
    }
}

fn source(class: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.Alias", class, &class.methods[0])?.source)
}

#[test]
fn guarded_result_can_copy_a_complete_typed_alias_from_another_entry_register() {
    for wide in [false, true] {
        let class = fixture(wide);
        let code = class.methods[0].code.as_ref().unwrap();
        let words = code.instructions.clone();
        let regions = code.try_regions.clone();
        let text = source(&class).unwrap_or_else(|error| panic!("wide={wide}: {error:#}"));
        for call in [
            "Effects.body(",
            "Effects.touch(",
            "Effects.caught(",
            if wide {
                "Effects.finishWide("
            } else {
                "Effects.finish("
            },
        ] {
            assert_eq!(text.matches(call).count(), 1, "{text}");
        }
        assert!(
            text.find("catch (").unwrap() < text.find("Effects.touch(").unwrap(),
            "{text}"
        );
        assert!(
            text.contains("if ("),
            "normal continuation must retain its success guard:\n{text}"
        );
        assert_eq!(class.methods[0].code.as_ref().unwrap().instructions, words);
        assert_eq!(class.methods[0].code.as_ref().unwrap().try_regions, regions);
    }
}

#[test]
fn undefined_differently_typed_and_partial_wide_aliases_keep_their_existing_guards() {
    let mut class = fixture(false);
    class.methods[0].code.as_mut().unwrap().instructions[10] = 0x2007; // unused localr2 is undefined
    assert!(source(&class).is_err());
    let mut class = fixture(false);
    class.methods[0].parameters[0] = "I".into();
    assert!(
        source(&class).is_err(),
        "an int entry cannot become an Object alias"
    );
    let mut class = fixture(true);
    class.methods[0].code.as_mut().unwrap().instructions[11] = 0x5004; // highhalf of p0 is not a wide head
    assert!(
        source(&class).is_err(),
        "a detached wide tail cannot authorize a two-word copy"
    );
}

#[test]
#[ignore = "requires javac/java; emitted Object/long methods compiled unchanged"]
fn jvm_guarded_entry_aliases_preserve_identity_wide_bits_and_independent_faults() {
    let narrow = source(&fixture(false)).unwrap();
    let wide = source(&fixture(true)).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-guarded-entry-alias-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    let file = dir.join("sample/Alias.java");
    std::fs::write(
        &file,
        HARNESS.replace("METHODS", &format!("{narrow}\n{wide}")),
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
        .args(["-cp", dir.to_str().unwrap(), "sample.Alias"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

const HARNESS: &str = r#"package sample;
public class Alias{
METHODS
static Object expected(Object fallback,Object normal){
 boolean success=false;Object result=null;
 try{Effects.body();success=true;}catch(RuntimeException fault){result=fallback;Effects.caught(fault);}
 if(success){Effects.touch();result=normal;}
 return Effects.finish(result);
}
static long expected(long fallback,long normal){
 boolean success=false;long result=0;
 try{Effects.body();success=true;}catch(RuntimeException fault){result=fallback;Effects.caught(fault);}
 if(success){Effects.touch();result=normal;}
 return Effects.finishWide(result);
}
public static void main(String[]args){
 int checks=0;Object[] values={null,new String("equal contents"),new String("equal contents")};
 for(Object fallback:values)for(Object normal:values)for(int mode=0;mode<=12;mode++){
  Object[] result=new Object[2];Throwable[] fault=new Throwable[2];String[] trace=new String[2];
  for(int side=0;side<2;side++){
   Effects.reset(mode);
   try{result[side]=side==0?run(fallback,normal):expected(fallback,normal);}catch(Throwable caught){fault[side]=caught;}
   trace[side]=Effects.trace;
  }
  if(result[0]!=result[1]||fault[0]!=fault[1]||!trace[0].equals(trace[1]))throw new AssertionError("Object "+mode+":"+trace[0]+"/"+trace[1],fault[0]);
  checks++;
 }
 long[] bits={Long.MIN_VALUE,0,Long.MAX_VALUE,0x123456789abcdef0L};
 for(long fallback:bits)for(long normal:bits)for(int mode=0;mode<=12;mode++){
  long[] result={-1,-1};Throwable[] fault=new Throwable[2];String[] trace=new String[2];
  for(int side=0;side<2;side++){
   Effects.reset(mode);
   try{result[side]=side==0?wide(fallback,normal):expected(fallback,normal);}catch(Throwable caught){fault[side]=caught;}
   trace[side]=Effects.trace;
  }
  if(result[0]!=result[1]||fault[0]!=fault[1]||!trace[0].equals(trace[1]))throw new AssertionError("long "+mode+":"+trace[0]+"/"+trace[1],fault[0]);
  checks++;
 }
 System.out.println("checks="+checks);
}
}
class Effects{
 static final RuntimeException original=new RuntimeException("original"),continuation=new RuntimeException("continuation"),callback=new RuntimeException("callback");
 static final Error error=new AssertionError("identity");static final java.io.IOException checked=new java.io.IOException("identity");
 static int mode;static String trace;
 static void reset(int selected){mode=selected;trace="";}
 @SuppressWarnings("unchecked")static<T extends Throwable>void raise(Throwable fault)throws T{throw(T)fault;}
 static void body(){trace+="B";if(mode==1||mode==7||mode==8||mode==9)throw original;if(mode==2)throw error;if(mode==3)Effects.<RuntimeException>raise(checked);}
 static void touch(){trace+="T";if(mode==4)throw continuation;if(mode==5)throw error;if(mode==6)Effects.<RuntimeException>raise(checked);}
 static void caught(Throwable fault){trace+="H";if(fault!=original)throw new AssertionError("protected identity");if(mode==7)throw callback;if(mode==8)throw error;if(mode==9)Effects.<RuntimeException>raise(checked);}
 static void finishEffect(){trace+="F";if(mode==10)throw continuation;if(mode==11)throw error;if(mode==12)Effects.<RuntimeException>raise(checked);}
 static Object finish(Object value){finishEffect();return value;}
 static long finishWide(long value){finishEffect();return value;}
}
"#;
