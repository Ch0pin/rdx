use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_java,
};
use std::sync::Arc;

fn fixture() -> DexClass {
    DexClass {
        descriptor: "Lsample/Carry;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            types: vec!["Lsample/Source;".into()],
            strings: vec!["touch".into()],
            protos: vec![("V".into(), vec!["I".into()])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/Carry;".into(),
            name: "run".into(),
            return_type: "Ljava/lang/Object;".into(),
            parameters: vec!["I".into(), "Ljava/lang/Object;".into()],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 6,
                ins: 2,
                outs: 1,
                tries: 0,
                try_regions: vec![],
                offset: 0,
                // r1 is an integer at the header/backedge. The header first
                // reads that integer, then establishes an independent Object
                // lifetime for the guard's exit edge in the same physical word.
                instructions: vec![
                    0x0012, 0x0112, 0x1201, 0x1071, 0, 2, 0x5107, 0x4035, 8, 0x02d8, 0x0102,
                    0x2101, 0x00d8, 0x0100, 0xf428, 0x0111,
                ],
            }),
        }],
    }
}

#[test]
fn header_integer_carry_and_reference_exit_keep_distinct_lifetimes() {
    let class = fixture();
    let analysis = rdx::native_method::MethodAnalysis::build(&class, &class.methods[0]).unwrap();
    assert_eq!(analysis.infer_types().unwrap().wide_pair_issues, 0);
    let source = native_java::render_method("sample.Carry", &class, &class.methods[0])
        .unwrap()
        .source;
    assert!(source.contains("int "), "{source}");
    assert!(source.contains("java.lang.Object "), "{source}");
    assert!(source.contains("while (true)"), "{source}");
}

#[test]
fn reference_exit_cannot_be_proved_by_relabeling_integer_or_undefined_values() {
    for shape in 0..3 {
        let mut class = fixture();
        let words = &mut class.methods[0].code.as_mut().unwrap().instructions;
        // Keep the same valid control flow; neither poisoned header establishes
        // a reference value on every normal guard exit.
        words[6] = match shape {
            0 => 0x4101, // a real integer parameter, not a null literal
            1 => 0x3107, // an undefined register, not a reference definition
            _ => 0x1112, // a nonzero literal cannot supply a reference exit
        };
        rdx::native_method::MethodAnalysis::build(&class, &class.methods[0]).unwrap();
        assert!(native_java::render_method("sample.Carry", &class, &class.methods[0]).is_err());
    }
}

#[test]
#[ignore = "requires javac and java"]
fn jvm_zero_iterations_and_restored_integer_backedges_return_exact_reference() {
    use std::process::Command;
    let class = fixture();
    let source = native_java::render_method("sample.Carry", &class, &class.methods[0])
        .unwrap()
        .source;
    let dir = std::env::temp_dir().join(format!("rdx-exit-lifetime-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    let file = dir.join("sample/Carry.java");
    std::fs::write(&file, format!(r#"package sample; public class Carry {{
{source}
public static void main(String[] args) {{
 Object value = new Object();
 for(int n=-3;n<=15;n++) {{
  String expected=""; for(int i=0;i<=Math.max(n,0);i++) expected+=i+",";
  for(Object input:new Object[]{{value,null}}) {{
   Source.trace="";
   if(run(n,input)!=input || !Source.trace.equals(expected)) throw new AssertionError(n+":"+Source.trace);
  }}
 }}
 for(int at:new int[]{{0,3,8}}) {{
  Source.trace=""; Source.throwAt=at;
  try {{ run(12,value); throw new AssertionError(); }}
  catch(RuntimeException e) {{ if(e!=Source.sentinel) throw new AssertionError(e); }}
  String expected=""; for(int i=0;i<=at;i++) expected+=i+",";
  if(!Source.trace.equals(expected)) throw new AssertionError(Source.trace);
 }}
}}
}}
class Source {{
 static String trace=""; static int throwAt=-1;
 static final RuntimeException sentinel=new RuntimeException();
 static void touch(int value) {{ trace+=value+","; if(value==throwAt) throw sentinel; }}
}}
"#)).unwrap();
    let compile = Command::new("javac").arg(file).output().unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new("java")
        .args(["-cp", dir.to_str().unwrap(), "sample.Carry"])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

fn retry_fixture() -> DexClass {
    let mut class = fixture();
    class.symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Source;".into()],
        strings: vec!["retry".into(), "success".into(), "inspect".into()],
        protos: vec![
            ("Z".into(), vec!["Ljava/lang/Object;".into()]),
            ("Ljava/lang/Object;".into(), vec![]),
            ("V".into(), vec!["Ljava/lang/Object;".into()]),
        ],
        methods: vec![(0, 0, 0), (0, 1, 1), (0, 2, 2)],
        ..Default::default()
    });
    let method = &mut class.methods[0];
    method.name = "run".into();
    method.return_type = "I".into();
    method.parameters.clear();
    let code = method.code.as_mut().unwrap();
    code.registers = 3;
    code.ins = 0;
    code.outs = 1;
    code.instructions = vec![
        0x0012, 0x0112, 0x1071, 0, 1, 0x020a, 0x0238, 7, 0x0071, 1, 0, 0x010c, 0x0428, 0x00d8,
        0x0100, 0xf328, 0x000f,
    ];
    class
}

#[test]
fn retry_invariant_null_is_not_materialized_due_to_dead_success_scratch_write() {
    let class = retry_fixture();
    rdx::native_method::MethodAnalysis::build(&class, &class.methods[0]).unwrap();
    let source = native_java::render_method("sample.Carry", &class, &class.methods[0])
        .unwrap()
        .source;
    assert!(
        source.contains("sample.Source.retry(((java.lang.Object) null))"),
        "{source}"
    );
    assert!(source.contains("sample.Source.success()"), "{source}");
}

#[test]
fn retry_changed_backedge_or_live_success_word_retains_conservative_fallback() {
    for shape in 0..2 {
        let mut class = retry_fixture();
        let code = class.methods[0].code.as_mut().unwrap();
        if shape == 0 {
            code.instructions[13] = 0x1112;
            code.instructions[14] = 0;
        } else {
            code.instructions.truncate(16);
            code.instructions.extend([0x1071, 2, 1, 0x000f]);
        }
        rdx::native_method::MethodAnalysis::build(&class, &class.methods[0]).unwrap();
        assert!(native_java::render_method("sample.Carry", &class, &class.methods[0]).is_err());
    }
}

#[test]
#[ignore = "requires javac and java"]
fn jvm_retry_null_and_success_scratch_keep_counts_order_and_exception_identity() {
    use std::process::Command;
    let class = retry_fixture();
    let source = native_java::render_method("sample.Carry", &class, &class.methods[0])
        .unwrap()
        .source;
    let dir = std::env::temp_dir().join(format!("rdx-retry-lifetime-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    let file = dir.join("sample/Carry.java");
    std::fs::write(&file, format!(r#"package sample; public class Carry {{
{source}
public static void main(String[] args) {{
 for(int failures=0;failures<=8;failures++) {{
  Source.reset(failures,-1);
  if(run()!=failures || !Source.trace.equals("R".repeat(failures+1)+"S")) throw new AssertionError(Source.trace);
 }}
 for(int at:new int[]{{0,3,8}}) {{
  Source.reset(9,at);
  try {{ run(); throw new AssertionError(); }}
  catch(RuntimeException e) {{ if(e!=Source.sentinel) throw new AssertionError(e); }}
  if(!Source.trace.equals("R".repeat(at+1))) throw new AssertionError(Source.trace);
 }}
 Source.reset(3,-2);
 try {{ run(); throw new AssertionError(); }}
 catch(RuntimeException e) {{ if(e!=Source.sentinel) throw new AssertionError(e); }}
 if(!Source.trace.equals("RRRRS")) throw new AssertionError(Source.trace);
}}
}}
class Source {{
 static int failures,calls,throwAt; static String trace;
 static final RuntimeException sentinel = new RuntimeException();
 static void reset(int n,int at) {{ failures=n; calls=0; throwAt=at; trace=""; }}
 static boolean retry(Object expected) {{
  if(expected!=null) throw new AssertionError(); trace+="R";
  if(calls==throwAt) throw sentinel; return calls++>=failures;
 }}
 static Object success() {{ trace+="S"; if(throwAt==-2) throw sentinel; return new Object(); }}
}}
"#)).unwrap();
    let compile = Command::new("javac").arg(file).output().unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new("java")
        .args(["-cp", dir.to_str().unwrap(), "sample.Carry"])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
