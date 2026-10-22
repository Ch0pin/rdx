//! A nested search may continue its parent on exhaustion and finish through a
//! protected terminal success suffix on a match.
pub use rdx::{native_cfg, native_dominators, native_ir, native_ssa};
#[allow(dead_code)]
#[path = "../src/native_java/forward_iteration_entry.rs"]
mod forward_iteration_entry;
#[path = "../src/native_java/parent_continue.rs"]
mod plan;
use rdx::{
    native_dex::{self, DexClass, DexSymbols, DexTryRegion},
    native_java,
};
use std::{fs, process::Command, sync::Arc};

fn fixture() -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class
        .methods
        .retain(|method| method.name.as_ref() == "answer");
    class.symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Effects;".into()],
        strings: ["parent", "child", "touch", "caught"]
            .into_iter()
            .map(String::from)
            .collect(),
        protos: vec![
            ("V".into(), vec!["I".into()]),
            ("I".into(), vec!["I".into()]),
            ("V".into(), vec!["Ljava/lang/Throwable;".into()]),
        ],
        methods: vec![(0, 0, 0), (0, 0, 1), (0, 1, 2), (0, 2, 3)],
        ..Default::default()
    });
    let method = &mut class.methods[0];
    method.name = "scan".into();
    method.parameters = vec!["I".into(), "I".into(), "I".into()];
    method.access_flags = 9;
    let code = method.code.as_mut().unwrap();
    code.registers = 6;
    code.ins = 3;
    code.outs = 1;
    code.tries = 1;
    code.instructions = vec![
        0x0012, 0x0112, // counters
        0x1071, 0, 0, 0x3035, 29, // parent effect and exhaustion guard ->34
        0x00d8, 0x0100, 0x0112, // next parent and child counter reset
        0x4135, 0xfff8, // child exhaustion ->parent2
        0x1071, 1, 1, 0x01d8, 0x0101, // child effect and increment
        0x5133, 0xfff9, // mismatch ->child10, match ->success19
        0x1071, 2, 1, 0x020a, 0x0238, 3, 0x020f, // touch; zero ->26
        0xf212, 0x020f, // zero result ->-1
        0x020d, 0x1071, 3, 2, 0xe212, 0x020f, // caught ->-2
        0xd212, 0x020f, // parent exhaustion ->-3
    ];
    code.try_regions = vec![DexTryRegion {
        start: 19,
        end: 25,
        catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 28)].into(),
    }];
    class
}

fn source(class: &DexClass) -> String {
    native_java::render_method("sample.Hello", class, &class.methods[0])
        .unwrap()
        .source
}

#[test]
fn conditional_child_feedback_keeps_its_protected_terminal_success() {
    let class = fixture();
    let code = class.methods[0].code.as_ref().unwrap();
    let plans = plan::select_all(code).expect("terminal success ownership");
    assert_eq!(plans.len(), 1);
    assert_eq!(
        (plans[0].parent, plans[0].child, plans[0].bound),
        (2, 10, 17)
    );
    assert_eq!(plans[0].terminal_success, Some(19));
    assert!(plans[0].success_terminal_nodes.contains(&28));
    assert!(plan::validate_all(code, &plans));
    let rendered = source(&class);
    assert_eq!(rendered.matches("while (true)").count(), 2, "{rendered}");
    assert_eq!(
        rendered.matches("sample.Effects.touch(").count(),
        1,
        "{rendered}"
    );
    assert!(
        rendered.contains("catch (java.lang.RuntimeException"),
        "{rendered}"
    );
}

#[test]
fn handler_or_success_reentry_and_protected_loop_body_remain_rejected() {
    for target in [2usize, 10, 12] {
        let mut class = fixture();
        class.methods[0].code.as_mut().unwrap().instructions[32] =
            (((target as isize - 32) as i8 as u8 as u16) << 8) | 0x28;
        assert!(plan::select_all(class.methods[0].code.as_ref().unwrap()).is_none());
    }
    let mut class = fixture();
    class.methods[0].code.as_mut().unwrap().try_regions[0].start = 12;
    assert!(plan::select_all(class.methods[0].code.as_ref().unwrap()).is_none());
    let mut class = fixture();
    class.methods[0].code.as_mut().unwrap().instructions[6] = 7; // parent guard enters child body
    assert!(plan::select_all(class.methods[0].code.as_ref().unwrap()).is_none());
}

#[test]
fn terminal_plan_pins_handler_identity_and_original_words() {
    let mut class = fixture();
    let code = class.methods[0].code.as_mut().unwrap();
    let selected = plan::select_all(code).unwrap().remove(0);
    code.try_regions[0].catches = vec![(Some("Ljava/lang/Error;".into()), 28)].into();
    assert!(!plan::validate(code, &selected));
    code.try_regions[0].catches = selected.try_regions[0].catches.clone();
    code.instructions[21] = 0; // touch now reads the original parent counter
    assert!(!plan::validate(code, &selected));
}

#[test]
fn three_independent_terminal_searches_share_the_bounded_selector() {
    let mut class = fixture();
    let code = class.methods[0].code.as_mut().unwrap();
    let original = code.instructions.clone();
    let region = code.try_regions[0].clone();
    code.instructions.clear();
    code.try_regions.clear();
    for index in 0..3 {
        let base = code.instructions.len() as u32;
        let mut words = original.clone();
        if index < 2 {
            words[34] = 0; // exhaustion falls through into the next independent search
            words[35] = 0;
        }
        code.instructions.extend(words);
        code.try_regions.push(DexTryRegion {
            start: base + region.start,
            end: base + region.end,
            catches: vec![(region.catches[0].0.clone(), base + region.catches[0].1)].into(),
        });
    }
    code.tries = 3;
    let plans = plan::select_all(code).unwrap();
    assert_eq!(plans.len(), 3);
    assert!(plan::validate_all(code, &plans));
    let rendered = source(&class);
    assert_eq!(rendered.matches("while (true)").count(), 6, "{rendered}");
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn jvm_terminal_search_preserves_trace_values_and_fault_identity() {
    let rendered = source(&fixture());
    let dir = std::env::temp_dir().join(format!(
        "rdx-parent-terminal-success-{}",
        std::process::id()
    ));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/Hello.java"), format!(r#"package sample;
public class Hello {{
 {rendered}
 static int reference(int outerLimit,int innerLimit,int stop) {{
  int outer=0;
  for(;;) {{
   Effects.parent(outer);if(outer>=outerLimit)return -3;outer++;
   int inner=0;
   for(;;) {{
    if(inner>=innerLimit)break;
    Effects.child(inner);inner++;
    if(inner!=stop)continue;
    try {{int value=Effects.touch(inner);return value==0?-1:value;}}
    catch(RuntimeException fault) {{Effects.caught(fault);return -2;}}
   }}
  }}
 }}
 static String outcome(boolean generated,int outer,int inner,int stop,int mode,int failAt) {{
  Effects.mode=mode;Effects.failAt=failAt;Effects.calls=0;Effects.trace="";Effects.captured=null;
  String result;
  try {{result="value:"+(generated?scan(outer,inner,stop):reference(outer,inner,stop));}}
  catch(RuntimeException fault) {{if(fault!=Effects.fault)throw fault;result="runtime";}}
  catch(Error fault) {{if(fault!=Effects.unmatched)throw fault;result="error";}}
  return result+":"+Effects.trace+":"+(Effects.captured==Effects.fault);
 }}
 public static void main(String[] args) {{
  for(int outer=0;outer<4;outer++)for(int inner=0;inner<4;inner++)for(int stop=-1;stop<5;stop++)
   for(int mode=0;mode<4;mode++)for(int failAt=-1;failAt<20;failAt++) {{
    String expected=outcome(false,outer,inner,stop,mode,failAt);
    String actual=outcome(true,outer,inner,stop,mode,failAt);
    if(!expected.equals(actual))throw new AssertionError("expected="+expected+" actual="+actual);
   }}
 }}
}}
class Effects {{
 static int mode,failAt,calls;static String trace;static Throwable captured;
 static final RuntimeException fault=new RuntimeException("sentinel");
 static final Error unmatched=new Error("sentinel");
 static void record(String label,int value){{trace+=label+value+";";if(calls++==failAt)throw fault;}}
 static void parent(int value){{record("p",value);}}
 static void child(int value){{record("c",value);}}
 static int touch(int value){{record("t",value);if(mode==2)throw fault;if(mode==3)throw unmatched;return mode==1?0:value+100;}}
 static void caught(Throwable fault){{captured=fault;record("caught",0);}}
}}
"#)).unwrap();
    for (program, args) in [
        ("javac", vec!["sample/Hello.java"]),
        ("java", vec!["-cp", ".", "sample.Hello"]),
    ] {
        let output = Command::new(program)
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{program}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
