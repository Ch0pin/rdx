//! Dead handler inputs need no recursive rendering of later disjoint catches.
use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_java,
};
use std::{collections::BTreeMap, process::Command, sync::Arc};

const TRIES: usize = 16;
// Exercise the maximum admitted sequence of disjoint catches with distinct
// nested joins. Eight real branch levels also fit ordinary debug worker stacks;
// repeated downstream handler probing remains the regression under test.
const LEVELS: usize = 8;

#[derive(Default)]
struct Asm {
    words: Vec<u16>,
    labels: BTreeMap<String, usize>,
    fixups: Vec<(usize, String)>,
}
impl Asm {
    fn emit(&mut self, words: &[u16]) {
        self.words.extend_from_slice(words);
    }
    fn mark(&mut self, label: String) {
        self.labels.insert(label, self.words.len());
    }
    fn branch(&mut self, opcode: u16, label: String) {
        self.fixups.push((self.words.len(), label));
        self.emit(&[opcode, 0]);
    }
    fn finish(mut self) -> (Vec<u16>, BTreeMap<String, usize>) {
        for (pc, label) in self.fixups {
            self.words[pc + 1] = (self.labels[&label] as isize - pc as isize) as i16 as u16;
        }
        (self.words, self.labels)
    }
}

fn class_with(symbols: DexSymbols, method: DexMethod) -> DexClass {
    DexClass {
        descriptor: "Lsample/Probe;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(symbols),
        methods: vec![method],
    }
}

fn dead_fixture() -> DexClass {
    let mut a = Asm::default();
    a.emit(&[0x0012, 0x0116, 10]);
    for index in 0..TRIES {
        a.mark(format!("start{index}"));
        a.emit(&[
            0x0013,
            (index + 1) as u16,
            0x0116,
            (1000 + index) as u16,
            0x3071,
            0,
            0x0210,
        ]);
        a.mark(format!("end{index}"));
        a.branch(0x0029, format!("join{index}"));
        a.mark(format!("handler{index}"));
        a.emit(&[
            0x030d,
            0x0013,
            (-(index as i16 + 1)) as u16,
            0x0116,
            (-(1000 + index as i16)) as u16,
            0x1071,
            1,
            3,
        ]);
        a.mark(format!("join{index}"));
        a.emit(&[0x3071, 2, 0x0210]);
    }
    for level in 0..LEVELS {
        a.branch(0x0538, format!("close{level}"));
        a.emit(&[0x0413, level as u16, 0x1071, 3, 4]);
    }
    for level in (0..LEVELS).rev() {
        a.mark(format!("close{level}"));
        a.emit(&[0x0413, level as u16, 0x1071, 4, 4]);
    }
    a.emit(&[0x0110]);
    let (words, labels) = a.finish();
    let regions = (0..TRIES)
        .map(|index| DexTryRegion {
            start: labels[&format!("start{index}")] as u32,
            end: labels[&format!("end{index}")] as u32,
            catches: vec![(
                Some("Ljava/lang/RuntimeException;".into()),
                labels[&format!("handler{index}")] as u32,
            )]
            .into(),
        })
        .collect();
    class_with(
        DexSymbols {
            strings: ["body", "caught", "observe", "depth", "close"]
                .into_iter()
                .map(String::from)
                .collect(),
            types: vec!["Lsample/Effects;".into()],
            protos: vec![
                ("V".into(), vec!["I".into(), "J".into()]),
                ("V".into(), vec!["Ljava/lang/Throwable;".into()]),
                ("V".into(), vec!["I".into()]),
            ],
            methods: vec![(0, 0, 0), (0, 1, 1), (0, 0, 2), (0, 2, 3), (0, 2, 4)],
            ..Default::default()
        },
        DexMethod {
            declaring_type: "Lsample/Probe;".into(),
            name: "run".into(),
            return_type: "J".into(),
            parameters: vec!["Z".into()],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 6,
                ins: 1,
                outs: 3,
                tries: TRIES as u16,
                try_regions: regions,
                instructions: words,
                offset: 0,
            }),
        },
    )
}

fn live_fixture() -> DexClass {
    class_with(
        DexSymbols {
            strings: ["liveThrow", "liveCaught", "before"]
                .into_iter()
                .map(String::from)
                .collect(),
            types: vec!["Lsample/Effects;".into()],
            protos: vec![
                ("V".into(), vec!["I".into(), "J".into()]),
                (
                    "J".into(),
                    vec!["I".into(), "J".into(), "Ljava/lang/Throwable;".into()],
                ),
                ("V".into(), vec![]),
            ],
            methods: vec![(0, 0, 0), (0, 1, 1), (0, 2, 2)],
            ..Default::default()
        },
        DexMethod {
            declaring_type: "Lsample/Probe;".into(),
            name: "live".into(),
            return_type: "J".into(),
            parameters: vec!["I".into(), "J".into()],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 6,
                ins: 3,
                outs: 4,
                tries: 1,
                offset: 0,
                instructions: vec![
                    0x3001, 0x4104, 0x0071, 2, 0, 0x7012, 0x0116, 99, 0x3071, 0, 0x0210, 0x0628,
                    0x030d, 0x4071, 1, 0x3210, 0x010b, 0x0110,
                ],
                try_regions: vec![DexTryRegion {
                    start: 2,
                    end: 11,
                    catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 12)].into(),
                }],
            }),
        },
    )
}

fn source(class: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.Probe", class, &class.methods[0])?.source)
}

#[test]
fn dead_narrow_and_both_wide_words_do_not_add_sequential_probe_nesting() {
    let class = dead_fixture();
    let code = class.methods[0].code.as_ref().unwrap();
    let original = code.instructions.clone();
    let regions = code.try_regions.clone();
    let text = source(&class).unwrap();
    assert_eq!(text.matches("catch (").count(), TRIES, "{text}");
    for name in ["body", "caught", "observe"] {
        assert_eq!(
            text.matches(&format!("Effects.{name}(")).count(),
            TRIES,
            "{text}"
        );
    }
    for name in ["depth", "close"] {
        assert_eq!(
            text.matches(&format!("Effects.{name}(")).count(),
            LEVELS,
            "{text}"
        );
    }
    assert!(!text.contains("__rdx_exception_input_"), "{text}");
    assert!(
        text.len() < 100_000,
        "unexpected continuation expansion: {}bytes",
        text.len()
    );
    assert_eq!(
        class.methods[0].code.as_ref().unwrap().instructions,
        original
    );
    assert_eq!(class.methods[0].code.as_ref().unwrap().try_regions, regions);
}

#[test]
fn live_narrow_and_wide_handler_inputs_retain_the_existing_snapshot_route() {
    let text = source(&live_fixture()).unwrap();
    assert!(text.contains("Effects.liveThrow("), "{text}");
    assert!(text.contains("Effects.liveCaught("), "{text}");
    assert!(!text.contains("__rdx_exception_input_"), "{text}");
}

#[test]
fn undefined_live_entry_and_overwritten_wide_half_cannot_skip_input_validation() {
    let mut class = live_fixture();
    let code = class.methods[0].code.as_mut().unwrap();
    code.instructions[..2].fill(0);
    rdx::native_cfg::ControlFlowGraph::build(code).unwrap();
    assert!(
        source(&class).is_err(),
        "throw-before-assignment exposes undefined handler input"
    );
    let mut class = live_fixture();
    class.methods[0].code.as_mut().unwrap().instructions[6..8].copy_from_slice(&[0x7212, 0]);
    assert!(
        source(&class).is_err(),
        "overwritten high word cannot remain a live wide value"
    );
}

#[test]
#[ignore = "requires javac/java; emitted methods compiled unchanged"]
fn jvm_dead_probe_elision_and_live_snapshots_preserve_values_effects_and_faults() {
    let dead = source(&dead_fixture()).unwrap();
    let live = source(&live_fixture()).unwrap();
    let java = HARNESS
        .replace("METHODS", &format!("{dead}\n{live}"))
        .replace("TRY_COUNT", &TRIES.to_string())
        .replace("LEVEL_COUNT", &LEVELS.to_string());
    let dir = std::env::temp_dir().join(format!("rdx-dead-handler-probe-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    let file = dir.join("sample/Probe.java");
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
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let result = Command::new(executable("java"))
        .args(["-cp", dir.to_str().unwrap(), "sample.Probe"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

const HARNESS: &str = r#"package sample;
public class Probe{
METHODS
static long expected(boolean take){
 int narrow=0;long wide=10;
 for(int index=0;index<TRY_COUNT;index++){
  try{narrow=index+1;wide=1000+index;Effects.body(narrow,wide);}
  catch(RuntimeException fault){narrow=-(index+1);wide=-(1000+index);Effects.caught(fault);}
  Effects.observe(narrow,wide);
 }
 if(take){for(int level=0;level<LEVEL_COUNT;level++)Effects.depth(level);for(int level=LEVEL_COUNT-1;level>0;level--)Effects.close(level);}
 Effects.close(0);return wide;
}
public static void main(String[]args){
 int checks=0;
 for(boolean take:new boolean[]{false,true})for(int mode=0;mode<=7;mode++)for(int failure=-1;failure<LEVEL_COUNT;failure++){
  long[] result={-1,-1};Throwable[] fault=new Throwable[2];String[] trace=new String[2];
  for(int side=0;side<2;side++){
   Effects.reset(mode,failure);
   try{result[side]=side==0?run(take):expected(take);}catch(Throwable caught){fault[side]=caught;}
   trace[side]=Effects.trace;
  }
  if(result[0]!=result[1]||fault[0]!=fault[1]||!trace[0].equals(trace[1]))throw new AssertionError(take+":"+mode+":"+failure+":"+trace[0]+"/"+trace[1],fault[0]);
  checks++;
 }
 for(int narrow:new int[]{-7,0,999})for(long wide:new long[]{Long.MIN_VALUE,0,Long.MAX_VALUE})for(int failure=0;failure<=2;failure++){
  Effects.reset(failure,0);long value=live(narrow,wide);
  long expected=failure==0?99:failure==1?7099:narrow*1000L+wide;
  String trace=failure==0?"PL7:99,":failure==1?"PL7:99,H7:99,":"PH"+narrow+":"+wide+",";
  if(value!=expected||!Effects.trace.equals(trace))throw new AssertionError("live snapshots "+Effects.trace+":"+value);
  checks++;
 }
 System.out.println("checks="+checks);
}
}
class Effects{
 static final RuntimeException original=new RuntimeException("original"),callback=new RuntimeException("callback"),external=new RuntimeException("external");
 static final Error error=new AssertionError("identity");static final java.io.IOException checked=new java.io.IOException("identity");
 static int mode,failure,stage;static String trace;
 static void reset(int selected,int at){mode=selected;failure=at;stage=0;trace="";}
 @SuppressWarnings("unchecked")static<T extends Throwable>void raise(Throwable fault)throws T{throw(T)fault;}
 static void body(int value,long wide){if(value!=stage+1||wide!=1000+stage)throw new AssertionError("body arguments");trace+="B"+stage+",";if(stage==failure){if(mode==1||mode==4)throw original;if(mode==2)throw error;if(mode==3)Effects.<RuntimeException>raise(checked);}}
 static void caught(Throwable fault){if(fault!=original)throw new AssertionError("caught identity");trace+="H"+stage+",";if(mode==4)throw callback;}
 static void observe(int value,long wide){boolean handled=mode==1&&stage==failure;int sign=handled?-1:1;if(value!=sign*(stage+1)||wide!=sign*(1000+stage))throw new AssertionError("overwritten handler values");trace+="O"+value+":"+wide+",";if(mode==5&&stage==failure)throw external;stage++;}
 static void depth(int level){trace+="D"+level+",";if(mode==6&&level==failure)throw external;}
 static void close(int level){trace+="C"+level+",";if(mode==7&&level==failure)throw external;}
 static void liveThrow(int value,long wide){trace+="L"+value+":"+wide+",";if(mode==1)throw original;}
 static long liveCaught(int value,long wide,Throwable fault){if(fault!=original)throw new AssertionError("live identity");trace+="H"+value+":"+wide+",";return value*1000+wide;}
 static void before(){trace+="P";if(mode==2)throw original;}
}
"#;
