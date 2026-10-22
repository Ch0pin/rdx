//! Nonthrowing loop suffixes retain the original throwing-instruction handlers.
use rdx::{
    native_dex::{self, DexClass, DexSymbols, DexTryRegion},
    native_java,
};
use std::sync::Arc;

fn fixture(shared: bool) -> DexClass {
    let mut c = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    c.methods.retain(|m| m.name.as_ref() == "answer");
    c.symbols = Arc::new(DexSymbols {
        strings: vec!["more".into(), "matches".into()],
        types: vec!["Lsample/Effects;".into()],
        protos: vec![("Z".into(), vec![])],
        methods: vec![(0, 0, 0), (0, 0, 1)],
        ..Default::default()
    });
    let m = &mut c.methods[0];
    m.name = "run".into();
    m.access_flags = 9;
    m.return_type = "Z".into();
    m.parameters = if shared { vec!["Z".into()] } else { vec![] };
    let code = m.code.as_mut().unwrap();
    code.registers = 4;
    code.ins = u16::from(shared);
    code.outs = 0;
    code.tries = 1;
    if shared {
        code.instructions = vec![
            0x0012, 0x0339, 14, 0x0071, 0, 0, 0x010a, 0x0138, 12, 0x0071, 1, 0, 0x010a, 0x0138, 4,
            0x1212, 0x020f, 0x0000, 0xf128, 0x000f,
        ];
        code.try_regions = vec![DexTryRegion {
            start: 1,
            end: 13,
            catches: vec![(None, 19)].into(),
        }];
    } else {
        code.instructions = vec![
            0x0012, 0x0071, 0, 0, 0x010a, 0x0138, 9, 0x0071, 1, 0, 0x010a, 0x0139, 0xfff6, 0x000f,
            0x1212, 0x020f, 0x000f,
        ];
        code.try_regions = vec![DexTryRegion {
            start: 1,
            end: 11,
            catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 16)].into(),
        }];
    }
    c
}
fn render(c: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.Hello", c, &c.methods[0])?.source)
}
#[test]
fn protected_loop_accepts_nonthrowing_latch_and_shared_terminal_leaf() {
    for shared in [false, true] {
        let c = fixture(shared);
        let s = render(&c).unwrap_or_else(|e| panic!("shared={shared}: {e:#}"));
        assert!(s.contains("while"), "{s}");
        assert!(s.contains("catch"), "{s}");
        println!("shared={shared}\n{s}");
    }
}
#[test]
fn newly_covered_throwing_instruction_and_handler_entry_reject() {
    let mut c = fixture(false);
    c.methods[0].code.as_mut().unwrap().try_regions[0].end = 7;
    assert!(render(&c).is_err());
    let mut c = fixture(true);
    c.methods[0].code.as_mut().unwrap().instructions[17] = 0x010d;
    assert!(render(&c).is_err());
    let mut c = fixture(true);
    let code = c.methods[0].code.as_mut().unwrap();
    code.tries = 2;
    code.try_regions.push(DexTryRegion {
        start: 15,
        end: 17,
        catches: vec![(None, 19)].into(),
    });
    assert!(render(&c).is_err());
    let mut c = backward_fixture();
    c.methods[0].code.as_mut().unwrap().instructions[18] = 0xfff8;
    assert!(render(&c).is_err());
}

fn backward_fixture() -> DexClass {
    let mut c = fixture(false);
    c.methods[0].name = "backward".into();
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions = vec![
        0x0012, 0x0071, 0, 0, 0x010a, 0x0138, 4, 0x0101, 0x0b28, 0x0071, 1, 0, 0x010a, 0x0139, 3,
        0xf828, 0x0000, 0x0138, 0xfff6, 0x0071, 0, 0, 0x010a, 0x010f, 0x020d, 0xee28,
    ];
    code.try_regions = vec![DexTryRegion {
        start: 1,
        end: 17,
        catches: vec![(None, 24)].into(),
    }];
    c
}

fn shared_handler_leaf_fixture() -> DexClass {
    let mut c = fixture(false);
    c.methods[0].name = "defaultLeaf".into();
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions = vec![
        0x0012, 0x0071, 0, 0, 0x010a, 0x0138, 13, 0x0071, 1, 0, 0x010a, 0x0138, 4, 0x1212, 0x0528,
        0, 0, 0xf028, 0x0201, 0x020f,
    ];
    code.try_regions = vec![DexTryRegion {
        start: 1,
        end: 13,
        catches: vec![(None, 18)].into(),
    }];
    c
}

fn array_header_scalar_exit_fixture() -> DexClass {
    let mut c = fixture(false);
    c.symbols = Arc::new(DexSymbols {
        strings: vec!["fallback".into(), "matches".into()],
        types: vec!["Lsample/Effects;".into()],
        protos: vec![("Z".into(), vec!["Ljava/lang/String;".into()])],
        methods: vec![(0, 0, 1)],
        ..Default::default()
    });
    let m = &mut c.methods[0];
    m.name = "arrayLeaf".into();
    m.return_type = "Ljava/lang/String;".into();
    m.parameters = vec!["[Ljava/lang/String;".into()];
    let code = m.code.as_mut().unwrap();
    code.registers = 6;
    code.ins = 1;
    code.outs = 1;
    code.instructions = vec![
        0x5107, 0x0012, 0x1221, 0x031a, 0, 0x2035, 15, 0x1407, 0x0146, 0x0004, 0x1071, 0, 1,
        0x050a, 0x0539, 7, 0x00d8, 0x0100, 0x4107, 0xf228, 0x3107, 0x0111,
    ];
    code.try_regions = vec![DexTryRegion {
        start: 5,
        end: 14,
        catches: vec![(None, 20)].into(),
    }];
    c
}

#[test]
fn protected_array_header_has_a_distinct_scalar_exit_lifetime() {
    let mut c = array_header_scalar_exit_fixture();
    let s = render(&c).unwrap();
    assert!(s.contains("while"), "{s}");
    assert!(!s.contains("((java.lang.String[])"), "{s}");
    c.methods[0].code.as_mut().unwrap().instructions[20] = 0x010d;
    assert!(render(&c).is_err());
}

#[test]
fn nonthrowing_default_leaf_can_share_the_original_handler_entry() {
    let mut c = shared_handler_leaf_fixture();
    let s = render(&c).unwrap();
    assert!(s.contains("while"), "{s}");
    assert!(s.contains("catch"), "{s}");
    c.methods[0].code.as_mut().unwrap().instructions[18] = 0x020d;
    assert!(render(&c).is_err());
}

#[test]
fn acyclic_backward_pure_exit_retains_protected_effect_boundary() {
    let c = backward_fixture();
    let s = render(&c).unwrap();
    assert!(s.contains("catch"), "{s}");
    println!("{s}");
}

#[test]
#[ignore = "requires javac and java"]
fn jvm_checks_shared_returns_latch_outcomes_and_exception_timing() {
    let shared = render(&fixture(true)).unwrap();
    let latch = render(&fixture(false)).unwrap();
    let backward = render(&backward_fixture()).unwrap();
    let default_leaf = render(&shared_handler_leaf_fixture()).unwrap();
    let array_leaf = render(&array_header_scalar_exit_fixture()).unwrap();
    let java = format!(
        r#"package sample;
public class Check {{
 {shared}
 {latch}
 {backward}
 {default_leaf}
 {array_leaf}
 static void check(boolean shared, boolean bypass, int limit, int match, int throwMore, int throwMatch, boolean expected, String trace) {{
  Effects.n=0; Effects.limit=limit; Effects.match=match; Effects.throwMore=throwMore; Effects.throwMatch=throwMatch; Effects.trace="";
  boolean actual=shared?run(bypass):run();
  if(actual!=expected || !Effects.trace.equals(trace)) throw new AssertionError(actual+"/"+Effects.trace+" expected "+expected+"/"+trace);
 }}
 static void arrayCheck(String[] values, String expected, boolean escaped, String trace) {{
  Effects.trace=""; boolean caught=false;
  try {{ if(!arrayLeaf(values).equals(expected)) throw new AssertionError("array result"); }} catch(NullPointerException e) {{ caught=true; }}
  if(caught!=escaped || !Effects.trace.equals(trace)) throw new AssertionError("array trace "+Effects.trace);
 }}
 static void defaultCheck(int limit, int match, int throwMore, int throwMatch, boolean expected, String trace) {{
  Effects.n=0; Effects.limit=limit; Effects.match=match; Effects.throwMore=throwMore; Effects.throwMatch=throwMatch; Effects.trace="";
  if(defaultLeaf()!=expected || !Effects.trace.equals(trace)) throw new AssertionError("default leaf "+Effects.trace);
 }}
 static void backwardCheck(int limit, int match, int throwMore, int throwMatch, boolean expected, boolean escaped, String trace) {{
  Effects.n=0; Effects.limit=limit; Effects.match=match; Effects.throwMore=throwMore; Effects.throwMatch=throwMatch; Effects.trace="";
  boolean caught=false;
  try {{ if(backward()!=expected) throw new AssertionError("backward result"); }} catch(RuntimeException e) {{ caught=true; }}
  if(caught!=escaped || !Effects.trace.equals(trace)) throw new AssertionError("backward catch/trace "+caught+"/"+Effects.trace);
 }}
 public static void main(String[] args) {{
  check(true,true,3,2,0,0,true,"");
  check(true,false,0,2,0,0,false,"m1;");
  check(true,false,3,2,0,0,true,"m1;t1;m2;t2;");
  check(true,false,3,99,0,0,false,"m1;t1;m2;t2;m3;t3;m4;");
  check(true,false,3,99,2,0,false,"m1;t1;m2;");
  check(true,false,3,99,0,2,false,"m1;t1;m2;t2;");
  check(false,false,0,1,0,0,true,"m1;");
  check(false,false,3,99,0,0,false,"m1;t1;");
  check(false,false,1,1,0,0,true,"m1;t1;m2;");
  check(false,false,3,1,2,0,false,"m1;t1;m2;");
  check(false,false,3,1,0,1,false,"m1;t1;");
  backwardCheck(3,1,0,0,true,false,"m1;m2;");
  backwardCheck(0,1,0,0,false,false,"m1;t1;m2;");
  backwardCheck(3,1,1,0,true,false,"m1;m2;");
  backwardCheck(3,1,2,0,false,true,"m1;m2;");
  backwardCheck(0,1,0,1,false,false,"m1;t1;m2;");
  defaultCheck(0,2,0,0,false,"m1;");
  defaultCheck(3,2,0,0,true,"m1;t1;m2;t2;");
  defaultCheck(3,99,0,0,false,"m1;t1;m2;t2;m3;t3;m4;");
  defaultCheck(3,99,2,0,false,"m1;t1;m2;");
  defaultCheck(3,99,0,2,false,"m1;t1;m2;t2;");
  arrayCheck(new String[]{{}},"fallback",false,"");
  arrayCheck(new String[]{{"hit"}},"hit",false,"a:hit;");
  arrayCheck(new String[]{{"miss","hit","later"}},"hit",false,"a:miss;a:hit;");
  arrayCheck(new String[]{{"miss","later"}},"fallback",false,"a:miss;a:later;");
  arrayCheck(new String[]{{"miss",null,"hit"}},"fallback",false,"a:miss;a:null;");
  arrayCheck(new String[]{{"error","hit"}},"fallback",false,"a:error;");
  arrayCheck(null,"fallback",true,"");
 }}
}}
class Effects {{
 static int n,limit,match,throwMore,throwMatch; static String trace;
 static boolean more() {{ ++n; trace+="m"+n+";"; if(n==throwMore) throw new RuntimeException(); return n<=limit; }}
 static boolean matches() {{ trace+="t"+n+";"; if(n==throwMatch) throw new RuntimeException(); return n==match; }}
 static boolean matches(String value) {{ trace+="a:"+value+";"; if(value==null) throw new NullPointerException(); if(value.equals("error")) throw new AssertionError(); return value.equals("hit"); }}
}}
"#
    );
    let dir = std::env::temp_dir().join(format!("rdx-protected-suffix-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    std::fs::write(dir.join("sample/Check.java"), &java).unwrap();
    for (cmd, arg) in [("javac", "sample/Check.java"), ("java", "sample.Check")] {
        let output = std::process::Command::new(cmd)
            .arg(arg)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{java}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
