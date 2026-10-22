//! Ordinary catches keep their original dispatch around a disjoint monitor.
use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_java,
};
use std::{process::Command, sync::Arc};

fn fixture() -> DexClass {
    DexClass {
        descriptor: "Lsample/Monitor;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            strings: [
                "leading",
                "caughtLeading",
                "inside",
                "trailing",
                "caughtTrailing",
                "finish",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            types: vec!["Lsample/Effects;".into()],
            protos: vec![
                ("V".into(), vec![]),
                ("V".into(), vec!["Ljava/lang/Throwable;".into()]),
                ("I".into(), vec![]),
                ("I".into(), vec!["I".into()]),
                ("I".into(), vec!["Ljava/lang/Throwable;".into()]),
            ],
            methods: vec![
                (0, 0, 0),
                (0, 1, 1),
                (0, 2, 2),
                (0, 3, 3),
                (0, 4, 4),
                (0, 3, 5),
            ],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/Monitor;".into(),
            name: "run".into(),
            return_type: "I".into(),
            parameters: vec!["Ljava/lang/Object;".into()],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 5,
                ins: 1,
                outs: 1,
                tries: 4,
                offset: 0,
                instructions: vec![
                    0x0071, 0, 0, 0x0628, // leading0; normal goto monitor9
                    0x000d, 0x1071, 1, 0, 0,      // catch4 capture; caughtLeading5; merge9
                    0x041d, // monitor-enter9
                    0x0071, 2, 0, 0x010a, 0x041e,
                    0x0428, // inside10/result13/release14/goto trailing19
                    0x000d, 0x041e, 0x0027, // cleanup16 capture/release17/throw18
                    0x1071, 3, 1, 0x010a, 0x0628, // trailing19/result22/goto finish29
                    0x000d, 0x1071, 4, 0, 0x010a, // trailing catch24/callback25/result28
                    0x1071, 5, 1, 0x010a, 0x010f, // finish29/result32/return33
                ],
                try_regions: vec![
                    DexTryRegion {
                        start: 0,
                        end: 3,
                        catches: vec![(None, 4)].into(),
                    },
                    DexTryRegion {
                        start: 10,
                        end: 15,
                        catches: vec![(None, 16)].into(),
                    },
                    DexTryRegion {
                        start: 17,
                        end: 18,
                        catches: vec![(None, 16)].into(),
                    },
                    DexTryRegion {
                        start: 19,
                        end: 23,
                        catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 24)].into(),
                    },
                ],
            }),
        }],
    }
}

fn source(class: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.Monitor", class, &class.methods[0])?.source)
}

#[test]
fn leading_catchall_and_trailing_typed_catch_remain_outside_the_monitor() {
    let class = fixture();
    let code = class.methods[0].code.as_ref().unwrap();
    let original = code.instructions.clone();
    let regions = code.try_regions.clone();
    let text = source(&class).unwrap();
    assert_eq!(text.matches("synchronized (p0)").count(), 1, "{text}");
    assert_eq!(text.matches("catch (").count(), 2, "{text}");
    assert!(
        text.find("catch (java.lang.Throwable").unwrap() < text.find("synchronized (").unwrap(),
        "{text}"
    );
    assert!(
        text.find("catch (java.lang.RuntimeException").unwrap()
            > text.find("Effects.trailing(").unwrap(),
        "{text}"
    );
    for name in [
        "leading",
        "caughtLeading",
        "inside",
        "trailing",
        "caughtTrailing",
        "finish",
    ] {
        assert_eq!(
            text.matches(&format!("Effects.{name}(")).count(),
            1,
            "{text}"
        );
    }
    assert_eq!(
        class.methods[0].code.as_ref().unwrap().instructions,
        original
    );
    assert_eq!(class.methods[0].code.as_ref().unwrap().try_regions, regions);
}

#[test]
fn ordinary_exception_entries_cannot_enter_monitor_body_release_or_cleanup() {
    for target in [10, 14, 16] {
        let mut class = fixture();
        class.methods[0].code.as_mut().unwrap().try_regions[0].catches =
            vec![(None, target)].into();
        assert!(
            source(&class).is_err(),
            "ordinary handler enters owned pc{target}"
        );
    }
}

#[test]
fn ordinary_protection_cannot_claim_monitor_acquisition_or_actual_body() {
    let mut class = fixture();
    class.methods[0].code.as_mut().unwrap().try_regions[0].end = 10;
    assert!(
        source(&class).is_err(),
        "ordinary catch protects monitor-enter"
    );
    let mut class = fixture();
    let code = class.methods[0].code.as_mut().unwrap();
    code.try_regions[1].end = 13;
    code.try_regions.insert(
        2,
        DexTryRegion {
            start: 13,
            end: 14,
            catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 4)].into(),
        },
    );
    code.tries += 1;
    assert!(
        source(&class).is_err(),
        "ordinary catch claims nonthrowing but owned body pc13"
    );
}

#[test]
#[ignore = "requires javac/java; emitted method compiled unchanged"]
fn jvm_disjoint_catches_preserve_dispatch_lock_timing_and_later_faults() {
    let text = source(&fixture()).unwrap();
    let dir = std::env::temp_dir().join(format!(
        "rdx-monitor-disjoint-catches-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    let file = dir.join("sample/Monitor.java");
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
        "{}\n{text}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let result = Command::new(executable("java"))
        .args(["-cp", dir.to_str().unwrap(), "sample.Monitor"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{text}",
        String::from_utf8_lossy(&result.stderr)
    );
}

const HARNESS: &str = r#"package sample;
public class Monitor{
METHOD
static int expected(Object lock){
 try{Effects.leading();}catch(Throwable caught){Effects.caughtLeading(caught);}
 int result;synchronized(lock){result=Effects.inside();}
 try{result=Effects.trailing(result);}catch(RuntimeException caught){result=Effects.caughtTrailing(caught);}
 return Effects.finish(result);
}
static void released(Object lock)throws Exception{
 if(lock==null)return;
 if(Thread.holdsLock(lock))throw new AssertionError("caller retained lock");
 boolean[] acquired={false};Thread thread=new Thread(()->{synchronized(lock){acquired[0]=true;}});
 thread.setDaemon(true);thread.start();thread.join(2000);
 if(!acquired[0])throw new AssertionError("another thread blocked");
}
public static void main(String[]args)throws Exception{
 int checks=0;
 for(boolean absent:new boolean[]{false,true})for(int mode=0;mode<=14;mode++){
  Object lock=absent?null:new Object();int[] result={-1,-1};Throwable[] fault=new Throwable[2];String[] trace=new String[2];
  for(int side=0;side<2;side++){
   Effects.reset(lock,mode);
   try{result[side]=side==0?run(lock):expected(lock);}catch(Throwable caught){fault[side]=caught;}
   trace[side]=Effects.trace;released(lock);
  }
  boolean sameFault=fault[0]==fault[1]||(absent&&fault[0]instanceof NullPointerException&&fault[1]instanceof NullPointerException);
  if(result[0]!=result[1]||!sameFault||!trace[0].equals(trace[1]))throw new AssertionError(absent+":"+mode+":"+trace[0]+"/"+trace[1],fault[0]);
  checks++;
 }
 System.out.println("checks="+checks);
}
}
class Effects{
 static final RuntimeException original=new RuntimeException("original"),callback=new RuntimeException("callback");
 static final Error error=new AssertionError("error");static final java.io.IOException checked=new java.io.IOException("checked");
 static Object lock;static int mode;static String trace;
 static void reset(Object input,int selected){lock=input;mode=selected;trace="";}
 static void outside(String step){if(lock!=null&&Thread.holdsLock(lock))throw new AssertionError(step+" inside monitor");trace+=step;}
 @SuppressWarnings("unchecked") static <T extends Throwable>void raise(Throwable fault)throws T{throw(T)fault;}
 static void leading(){outside("L");if(mode==1||mode==4)throw original;if(mode==2)throw error;if(mode==3)Effects.<RuntimeException>raise(checked);}
 static void caughtLeading(Throwable fault){outside("H");if(fault!=original&&fault!=error&&fault!=checked)throw new AssertionError("leading identity");if(mode==4)throw callback;}
 static int inside(){if(!Thread.holdsLock(lock))throw new AssertionError("body outside monitor");trace+="I";if(mode==5)throw original;if(mode==6)throw error;if(mode==7)Effects.<RuntimeException>raise(checked);return 17;}
 static int trailing(int value){outside("T");if(mode==8||mode==11)throw original;if(mode==9)throw error;if(mode==10)Effects.<RuntimeException>raise(checked);return value+100;}
 static int caughtTrailing(Throwable fault){outside("R");if(fault!=original)throw new AssertionError("typed catch identity");if(mode==11)throw callback;return -19;}
 static int finish(int value){outside("F");if(mode==12)throw original;if(mode==13)throw error;if(mode==14)Effects.<RuntimeException>raise(checked);return value*2+3;}
}
"#;
