//! Both normal monitor releases reach one earlier post-release continuation.
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
            strings: ["first", "second", "after"]
                .into_iter()
                .map(String::from)
                .collect(),
            types: vec!["Lsample/Effects;".into()],
            protos: vec![
                ("I".into(), vec!["I".into()]),
                ("I".into(), vec!["I".into(), "I".into()]),
            ],
            methods: vec![(0, 0, 0), (0, 0, 1), (0, 1, 2)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/Monitor;".into(),
            name: "run".into(),
            return_type: "I".into(),
            parameters: vec!["Ljava/lang/Object;".into(), "Z".into(), "I".into()],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 7,
                ins: 3,
                outs: 2,
                tries: 3,
                offset: 0,
                instructions: vec![
                    0x041d, // enter0
                    0x0538, 11, // if !first -> second12
                    0x1071, 0, 6, 0x010a, 0x041e, // first(value), result1, release7
                    0x0012, 0x0b28, // shared8: zero=0; goto post20
                    0x020d, 0x0728, // cleanup10 capture; goto release18
                    0x1071, 1, 6, 0x010a, 0x041e, 0xf728, // second12, release16, goto shared8
                    0x041e, 0x0227, // cleanup18 release/rethrow same exception
                    0x2071, 2, 0x0010, 0x000a, 0x000f, // post20 after(zero,result1); return
                ],
                try_regions: [(1, 8), (12, 17), (18, 19)]
                    .into_iter()
                    .map(|(start, end)| DexTryRegion {
                        start,
                        end,
                        catches: vec![(None, 10)].into(),
                    })
                    .collect(),
            }),
        }],
    }
}

fn source(class: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.Monitor", class, &class.methods[0])?.source)
}

fn far_fixture() -> DexClass {
    let mut class = fixture();
    let symbols = Arc::get_mut(&mut class.symbols).unwrap();
    symbols.strings.push("bypass".into());
    symbols.methods.push((0, 0, 3));
    let method = &mut class.methods[0];
    method.name = "far".into();
    method.parameters.push("Z".into());
    let code = method.code.as_mut().unwrap();
    let old = code.instructions.clone();
    let mut words = vec![0x0739, 22]; // bypass avoids the entire monitor
    words.extend_from_slice(&old[..20]);
    words.extend_from_slice(&[0x1071, 3, 6, 0x010a, 0x0012]);
    words.extend_from_slice(&[0; 18]); // reachable bypass-only instructions
    words.extend_from_slice(&[0x0029, 2]);
    let resume = words.len();
    assert_eq!(resume, 47);
    words[11] = (((resume - 11) as u16) << 8) | 0x28;
    // An unrelated normal path also reaches the original shared assignment;
    // that instruction remains live after monitor-tail cloning.
    words[46] = (10isize - 45) as i16 as u16;
    words.extend_from_slice(&old[20..]);
    code.instructions = words;
    code.registers = 8;
    code.ins = 4;
    for region in &mut code.try_regions {
        region.start += 2;
        region.end += 2;
        region.catches = vec![(None, 12)].into();
    }
    class
}

#[test]
fn releases_share_the_original_post_release_tail_and_merge_actual_values() {
    for class in [fixture(), far_fixture()] {
        let code = class.methods[0].code.as_ref().unwrap();
        let original = code.instructions.clone();
        let original_regions = code.try_regions.clone();
        let text = source(&class).unwrap();
        assert_eq!(text.matches("synchronized (p0)").count(), 1, "{text}");
        for call in ["Effects.first(", "Effects.second(", "Effects.after("] {
            assert_eq!(text.matches(call).count(), 1, "{text}");
        }
        if class.methods[0].name.as_ref() == "far" {
            assert_eq!(text.matches("Effects.bypass(").count(), 1, "{text}");
        }
        assert_eq!(
            class.methods[0].code.as_ref().unwrap().instructions,
            original
        );
        assert_eq!(
            class.methods[0].code.as_ref().unwrap().try_regions,
            original_regions
        );
    }
}

#[test]
fn released_path_reentry_tail_cycle_wrong_lock_and_wrong_rethrow_decline() {
    for (pc, word) in [
        (17, 0xf228), // released second path reenters protected first body3
        (9, 0xff28),  // post-release shared tail cycles to8
        (9, 0x0928),  // normal continuation enters exceptional release18
        (16, 0x061e), // second path releases integer value register, not original lock
        (19, 0x0027), // cleanup throws zero register instead of captured exception2
        (17, 0xfc28), // released bridge enters second() argument word13
        (9, 0x0c28),  // post-release jump enters after() instruction interior21
    ] {
        let mut class = fixture();
        class.methods[0].code.as_mut().unwrap().instructions[pc] = word;
        assert!(source(&class).is_err(), "pc={pc}, word={word:04x}");
    }
    let mut class = fixture();
    let code = class.methods[0].code.as_mut().unwrap();
    code.instructions.insert(17, 0x1112); // later release changes live result before shared8
    code.instructions[18] = 0xf628;
    code.instructions[9] = 0x0c28;
    code.instructions[11] = 0x0828;
    code.try_regions[2].start += 1;
    code.try_regions[2].end += 1;
    assert!(
        source(&class).is_err(),
        "unequal later bridge write accepted"
    );
}

#[test]
#[ignore = "requires javac/java; emitted method compiled unchanged"]
fn jvm_shared_monitor_tail_preserves_lock_timing_effects_and_fault_identity() {
    let text = source(&fixture()).unwrap();
    let far_text = source(&far_fixture()).unwrap();
    let java = HARNESS.replace("METHOD", &format!("{text}\n{far_text}"));
    let dir = std::env::temp_dir().join(format!("rdx-monitor-shared-tail-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    let file = dir.join("sample/Monitor.java");
    std::fs::write(&file, java).unwrap();
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
public class Monitor {
METHOD
static int expected(Object lock,boolean first,int value){
 int result;
 synchronized(lock){result=first?Effects.first(value):Effects.second(value);}
 return Effects.after(0,result);
}
static int expectedFar(Object lock,boolean first,int value,boolean bypass){
 int result;
 if(bypass)result=Effects.bypass(value);
 else synchronized(lock){result=first?Effects.first(value):Effects.second(value);}
 return Effects.after(0,result);
}
static void released(Object lock)throws Exception{
 if(Thread.holdsLock(lock))throw new AssertionError("lock retained by caller");
 boolean[] acquired={false};Thread thread=new Thread(()->{synchronized(lock){acquired[0]=true;}});
 thread.setDaemon(true);thread.start();thread.join(2000);
 if(!acquired[0])throw new AssertionError("lock retained against another thread");
}
public static void main(String[]args)throws Exception{
 int checks=0;
 for(boolean first:new boolean[]{false,true})for(int value=-3;value<=3;value++)for(int failure=0;failure<=4;failure++){
  Object lock=new Object();int[] results={-1,-1};Throwable[] faults=new Throwable[2];String[] traces=new String[2];
  for(int side=0;side<2;side++){
   Effects.reset(lock,failure);
   try{results[side]=side==0?run(lock,first,value):expected(lock,first,value);}catch(Throwable fault){faults[side]=fault;}
   traces[side]=Effects.trace;released(lock);
  }
  if(results[0]!=results[1]||faults[0]!=faults[1]||!traces[0].equals(traces[1]))
   throw new AssertionError(first+":"+value+":"+failure+":"+traces[0]+"/"+traces[1]);
  checks++;
 }
 for(boolean bypass:new boolean[]{false,true})for(boolean first:new boolean[]{false,true})for(int value=-3;value<=3;value++)for(int failure=0;failure<=4;failure++){
  Object lock=new Object();int[] results={-1,-1};Throwable[] faults=new Throwable[2];String[] traces=new String[2];
  for(int side=0;side<2;side++){
   Effects.reset(lock,failure);
   try{results[side]=side==0?far(lock,first,value,bypass):expectedFar(lock,first,value,bypass);}catch(Throwable fault){faults[side]=fault;}
   traces[side]=Effects.trace;released(lock);
  }
  if(results[0]!=results[1]||faults[0]!=faults[1]||!traces[0].equals(traces[1]))
   throw new AssertionError("far "+bypass+":"+first+":"+value+":"+failure+":"+traces[0]+"/"+traces[1]);
  checks++;
 }
 for(boolean first:new boolean[]{false,true}){
  Effects.reset(null,0);
  try{run(null,first,7);throw new AssertionError("null monitor accepted");}
  catch(NullPointerException expected){if(!Effects.trace.equals(""))throw new AssertionError("effect before acquisition");}
  Effects.reset(null,0);
  if(far(null,first,7,true)!=(7+300)*3||!Effects.trace.equals("B7,A0:307,"))throw new AssertionError("null bypass");
 }
 System.out.println("checks="+checks);
}
}
class Effects{
 static final RuntimeException bodyRuntime=new RuntimeException("body"),afterRuntime=new RuntimeException("after");
 static final Error bodyError=new AssertionError("body"),afterError=new AssertionError("after");
 static Object lock;static int failure;static String trace;
 static void reset(Object input,int mode){lock=input;failure=mode;trace="";}
 static int body(int value,boolean first){
  if(!Thread.holdsLock(lock))throw new AssertionError("body outside lock");
  trace+=(first?"F":"S")+value+",";
  if(failure==1)throw bodyRuntime;if(failure==2)throw bodyError;
  return value+(first?100:200);
 }
 static int first(int value){return body(value,true);}
 static int second(int value){return body(value,false);}
 static int bypass(int value){
  if(lock!=null&&Thread.holdsLock(lock))throw new AssertionError("bypass acquired monitor");
  trace+="B"+value+",";
  if(failure==1)throw afterRuntime;if(failure==2)throw afterError;
  return value+300;
 }
 static int after(int zero,int value){
  if(lock!=null&&Thread.holdsLock(lock))throw new AssertionError("post-release effect still inside lock");
  if(zero!=0)throw new AssertionError("shared assignment missing");
  trace+="A"+zero+":"+value+",";
  if(failure==3)throw afterRuntime;if(failure==4)throw afterError;
  return value*3+zero;
 }
}
"#;
