//! A protected search keeps its Iterator carry and its String/null exit separate.
pub use rdx::{native_cfg, native_ir};
use rdx::{
    native_dex::{self, DexClass, DexSymbols, DexTryRegion},
    native_java,
};
use std::{process::Command, sync::Arc};
#[path = "../src/native_java/pure_loop_exit_span.rs"]
mod pure_tail;

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
        strings: ["more", "value", "finish", "caught"]
            .into_iter()
            .map(String::from)
            .collect(),
        protos: vec![
            ("Z".into(), vec!["Ljava/util/Iterator;".into()]),
            ("Ljava/lang/String;".into(), vec![]),
            (
                "Ljava/lang/String;".into(),
                vec!["Ljava/lang/String;".into(), "Ljava/util/Iterator;".into()],
            ),
            (
                "Ljava/lang/String;".into(),
                vec!["Ljava/lang/Throwable;".into()],
            ),
        ],
        methods: vec![(0, 0, 0), (0, 1, 1), (0, 2, 2), (0, 3, 3)],
        ..Default::default()
    });
    let method = &mut class.methods[0];
    method.name = "search".into();
    method.access_flags = 9;
    method.return_type = "Ljava/lang/String;".into();
    method.parameters = vec!["Ljava/util/Iterator;".into()];
    let code = method.code.as_mut().unwrap();
    code.registers = 4;
    code.ins = 1;
    code.outs = 2;
    code.tries = 1;
    code.try_regions = vec![DexTryRegion {
        start: 1,
        end: 18,
        catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 23)].into(),
    }];
    code.instructions = vec![
        0x3007, // Iterator entry r0=p0
        0x1071, 0, 0, 0x010a, 0x0138, 11, // more(r0); if !more -> guard tail16
        0x0207, 0x0071, 1, 0, 0x000c, // save Iterator r2; String r0=value()
        0x0039, 6, 0x2007, 0xf228, // String success ->18; otherwise restore carry
        0x0207, 0x0012, // exhausted: save Iterator; String null
        0x2071, 2, 0x0020, 0x000c, 0x0011, // unprotected finish(r0,r2); return
        0x000d, 0x1071, 3, 0, 0x000c, 0x0011, // exact caught throwable -> caught(); return
    ];
    class
}

fn source(class: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.Hello", class, &class.methods[0])?.source)
}

#[test]
fn protected_search_preserves_distinct_carry_exit_and_detached_catch() {
    let class = fixture();
    assert!(pure_tail::admits(
        class.methods[0].code.as_ref().unwrap(),
        1,
        16,
        18
    ));
    let text = source(&class).unwrap();
    assert!(text.contains("while"), "{text}");
    assert!(text.contains("java.util.Iterator"), "{text}");
    assert!(text.contains("java.lang.String"), "{text}");
    assert!(text.contains("catch"), "{text}");
    assert!(text.contains("Effects.finish("), "{text}");
}

#[test]
fn integer_exit_and_handler_visible_mutations_keep_the_original_guards() {
    let mut class = fixture();
    class.methods[0].code.as_mut().unwrap().instructions[17] = 0x1012;
    assert!(source(&class).is_err());
    let mut class = fixture();
    // An initialized r1 becomes visible to the catch while the loop changes
    // it. Changing exceptional snapshots retain their independent guard.
    Arc::get_mut(&mut class.symbols).unwrap().protos[3].1 = vec!["I".into()];
    let code = class.methods[0].code.as_mut().unwrap();
    code.instructions.insert(0, 0x0112);
    code.instructions[27] = 1;
    code.try_regions[0].start += 1;
    code.try_regions[0].end += 1;
    code.try_regions[0].catches = vec![(Some("Ljava/lang/RuntimeException;".into()), 24)].into();
    let error = source(&class).unwrap_err();
    assert!(
        format!("{error:#}").contains("loop writes handler-visible register"),
        "{error:#}"
    );
}

#[test]
fn protected_pure_tail_declines_throwing_instruction_and_exceptional_ingress() {
    let mut class = fixture();
    let code = class.methods[0].code.as_mut().unwrap();
    code.instructions[16] = 0x0027;
    assert!(!pure_tail::admits(code, 1, 16, 18));
    let mut class = fixture();
    let code = class.methods[0].code.as_mut().unwrap();
    code.try_regions[0].catches = vec![(None, 16)].into();
    assert!(!pure_tail::admits(code, 1, 16, 18));
}

#[test]
#[ignore = "requires javac and java; emitted method compiled unchanged"]
fn jvm_protected_search_preserves_values_iterator_identity_and_fault_order() {
    let text = source(&fixture()).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-protected-exit-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    let file = dir.join("sample/Hello.java");
    let harness = HARNESS.replace("METHOD", &text);
    std::fs::write(&file, harness).unwrap();
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
        .args(["-cp", dir.to_str().unwrap(), "sample.Hello"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

const HARNESS: &str = r#"package sample;
import java.util.*;
public class Hello {
METHOD
static String expected(Iterator iterator) {
 String result;
 try {
  for (;;) {
   if (!Effects.more(iterator)) { result=null; break; }
   result=Effects.value();
   if(result!=null) break;
  }
 } catch(RuntimeException fault) { return Effects.caught(fault); }
 return Effects.finish(result,iterator);
}
public static void main(String[] args) {
 int cases=0;
 for(int limit=0;limit<=7;limit++) for(int match=-1;match<=8;match++)
 for(int failure=-1;failure<=18;failure++) {
  Iterator identity=Collections.emptyIterator();
  Object[] results=new Object[2]; String[] traces=new String[2]; Throwable[] caught=new Throwable[2];
  for(int side=0;side<2;side++) {
   Effects.reset(identity,limit,match,failure);
   try {results[side]=side==0?search(identity):expected(identity);}
   catch(RuntimeException fault) {results[side]=fault;}
   traces[side]=Effects.trace; caught[side]=Effects.captured;
  }
  if(results[0]!=results[1] || !traces[0].equals(traces[1]) || caught[0]!=caught[1])
   throw new AssertionError(limit+":"+match+":"+failure+":"+traces[0]+"/"+traces[1]);
  cases++;
 }
 System.out.println("cases="+cases);
}
}
class Effects {
 static final RuntimeException sentinel=new RuntimeException("identity");
 static final String success=new String("success"), caughtResult=new String("caught");
 static Iterator identity; static int limit,match,failure,visits,calls; static String trace; static Throwable captured;
 static void reset(Iterator input,int n,int successAt,int failAt) {
  identity=input;limit=n;match=successAt;failure=failAt;visits=0;calls=0;trace="";captured=null;
 }
 static void step(String operation) {trace+=operation+",";if(calls++==failure)throw sentinel;}
 static boolean more(Iterator input) {if(input!=identity)throw new AssertionError("iterator");step("M"+visits);return visits<limit;}
 static String value() {step("V"+visits);return visits++==match?success:null;}
 static String finish(String result,Iterator input) {if(input!=identity)throw new AssertionError("saved iterator");step("F");return result;}
 static String caught(Throwable fault) {captured=fault;step("C");return caughtResult;}
}
"#;
