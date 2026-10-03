use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_java,
    native_method::MethodAnalysis,
};
use std::sync::Arc;

fn method(name: &str, ret: &str, args: &[&str], registers: u16, words: Vec<u16>) -> DexMethod {
    DexMethod {
        declaring_type: "Lsample/Widen;".into(),
        name: name.into(),
        return_type: ret.into(),
        parameters: args.iter().map(|s| (*s).into()).collect(),
        thrown_types: vec![],
        access_flags: 9,
        code: Some(DexCode {
            registers,
            ins: args.len() as u16,
            outs: 1,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: words,
        }),
    }
}
fn class() -> DexClass {
    DexClass {
        descriptor: "Lsample/Widen;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            types: vec!["Lsample/Overloads;".into(), "S".into()],
            strings: vec!["use".into(), "stored".into()],
            protos: vec![
                ("I".into(), vec!["S".into()]),
                ("I".into(), vec!["B".into()]),
            ],
            methods: vec![(0, 0, 0), (0, 1, 0)],
            fields: vec![(0, 1, 1)],
            ..Default::default()
        }),
        methods: vec![
            method("call", "I", &["B"], 1, vec![0x1071, 0, 0, 0x000a, 0x000f]),
            method("ret", "S", &["B"], 1, vec![0x000f]),
            method(
                "array",
                "V",
                &["[S", "I", "B"],
                3,
                vec![0x0251, 0x0100, 0x000e],
            ),
            method("field", "V", &["B"], 1, vec![0x006d, 0, 0x000e]),
            method(
                "carry",
                "S",
                &["I", "S", "B"],
                5,
                vec![
                    0x0012, 0x3101, 0x2035, 6, 0x4101, 0x00d8, 0x0100, 0xfb28, 0x010f,
                ],
            ),
        ],
    }
}

#[test]
fn calls_returns_stores_and_loop_copies_widen_only_byte_to_short() {
    let class = class();
    for method in &class.methods {
        MethodAnalysis::build(&class, method).unwrap();
        let code = native_java::render_method("sample.Widen", &class, method).unwrap();
        assert!(code.source.contains("short"), "{}", code.source);
        if method.name.as_ref() == "call" {
            assert!(code.source.contains("((short)"), "{}", code.source);
            assert!(
                code.links
                    .iter()
                    .any(|link| link.label == "sample.Overloads.use(S)I")
            );
        }
    }
}

#[test]
fn narrowing_char_and_boolean_domains_keep_their_existing_guards() {
    for (from, to) in [("S", "B"), ("C", "S"), ("B", "Z")] {
        let mut class = class();
        class.methods = vec![method("bad", to, &[from], 1, vec![0x000f])];
        MethodAnalysis::build(&class, &class.methods[0]).unwrap();
        assert!(
            native_java::render_method("sample.Widen", &class, &class.methods[0]).is_err(),
            "{from} to {to}"
        );
    }
    for (to, from) in [("B", "S"), ("S", "C")] {
        let mut class = class();
        class.methods = vec![method(
            "badLoop",
            to,
            &["I", to, from],
            5,
            vec![
                0x0012, 0x3101, 0x2035, 6, 0x4101, 0x00d8, 0x0100, 0xfb28, 0x010f,
            ],
        )];
        MethodAnalysis::build(&class, &class.methods[0]).unwrap();
        assert!(
            native_java::render_method("sample.Widen", &class, &class.methods[0]).is_err(),
            "loop {from} to {to}"
        );
    }
}

#[test]
#[ignore = "requires javac and java"]
fn jvm_all_byte_values_short_overload_and_exception_identity() {
    use std::process::Command;
    let class = class();
    let mut sources = String::new();
    for method in &class.methods {
        sources.push_str(
            &native_java::render_method("sample.Widen", &class, method)
                .unwrap()
                .source,
        );
    }
    let dir = std::env::temp_dir().join(format!("rdx-byte-short-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    let file = dir.join("sample/Widen.java");
    std::fs::write(&file, format!(r#"package sample; public class Widen {{
{sources}
public static void main(String[] args) {{
 for(int n=-128;n<=127;n++) {{
  byte b=(byte)n; Overloads.fail=false;
  if(call(b)!=10000+n || !Overloads.which.equals("short") || ret(b)!=n) throw new AssertionError(n);
  short[] a=new short[]{{999}}; array(a,0,b); field(b);
  if(a[0]!=n || Overloads.stored!=n) throw new AssertionError("store "+n);
  for(short initial:new short[]{{-32768,-129,-128,0,127,128,32767}}) for(int count:new int[]{{-1,0,1,4}})
   if(carry(count,initial,b)!=(count>0?n:initial)) throw new AssertionError("carry "+n);
 }}
 Overloads.fail=true;
 try {{ call((byte)-7); throw new AssertionError(); }}
 catch(RuntimeException e) {{ if(e!=Overloads.sentinel || !Overloads.which.equals("short")) throw new AssertionError(e); }}
}}
}}
class Overloads {{
 static short stored; static String which=""; static boolean fail;
 static final RuntimeException sentinel=new RuntimeException();
 static int use(short value) {{ which="short"; if(fail) throw sentinel; return 10000+value; }}
 static int use(byte value) {{ which="byte"; return 20000+value; }}
}}
"#)).unwrap();
    let home =
        std::env::var("RDX_JAVA25_HOME").expect("set RDX_JAVA25_HOME to a JDK 25 installation");
    let result = Command::new(format!("{home}/bin/javac"))
        .arg(file)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let result = Command::new(format!("{home}/bin/java"))
        .args(["-cp", dir.to_str().unwrap(), "sample.Widen"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}
