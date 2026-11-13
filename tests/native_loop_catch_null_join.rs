//! A reflection failure and a failed instanceof share a null assignment in a loop.
use rdx::{
    native_dex::{self, DexClass, DexSymbols, DexTryRegion},
    native_java,
};
use std::sync::Arc;

fn fixture() -> DexClass {
    let mut c = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    c.methods.retain(|m| m.name.as_ref() == "answer");
    c.descriptor = "Lsample/ReflectionScan;".into();
    c.symbols = Arc::new(DexSymbols {
        strings: [
            "<init>", "iterator", "hasNext", "next", "getClass", "invoke", "length", "add", "e",
        ]
        .into_iter()
        .map(Into::into)
        .collect(),
        types: [
            "Ljava/util/ArrayList;",
            "Ljava/util/List;",
            "Ljava/util/Iterator;",
            "Ljava/lang/String;",
            "Ljava/lang/Object;",
            "[Ljava/lang/Object;",
            "Ljava/lang/reflect/Method;",
            "Lsample/ValidationCheckResult;",
            "Lsample/ReflectionScan;",
        ]
        .into_iter()
        .map(Into::into)
        .collect(),
        protos: vec![
            ("V".into(), vec![]),
            ("Ljava/util/Iterator;".into(), vec![]),
            ("Z".into(), vec![]),
            ("Ljava/lang/Object;".into(), vec![]),
            ("Ljava/lang/Class;".into(), vec![]),
            (
                "Ljava/lang/Object;".into(),
                vec!["Ljava/lang/Object;".into(), "[Ljava/lang/Object;".into()],
            ),
            ("I".into(), vec![]),
            (
                "V".into(),
                vec!["Ljava/lang/String;".into(), "Ljava/lang/String;".into()],
            ),
            ("Z".into(), vec!["Ljava/lang/Object;".into()]),
        ],
        methods: vec![
            (0, 0, 0),
            (1, 1, 1),
            (2, 2, 2),
            (2, 3, 3),
            (4, 4, 4),
            (6, 5, 5),
            (3, 6, 6),
            (7, 7, 0),
            (0, 8, 7),
        ],
        fields: vec![(8, 6, 8)],
        ..Default::default()
    });
    let m = &mut c.methods[0];
    m.declaring_type = c.descriptor.clone();
    m.name = "scan".into();
    m.access_flags = 0x11;
    m.parameters = vec!["Ljava/util/List;".into()];
    m.return_type = "Ljava/util/ArrayList;".into();
    let b = m.code.as_mut().unwrap();
    b.registers = 7;
    b.ins = 2;
    b.outs = 3;
    b.tries = 1;
    b.instructions = vec![
        0x0022, 0, 0x1070, 0, 0, 0x1072, 1, 6, 0x060c, 0x1072, 2, 6, 0x010a, 0x0138, 0x0036,
        0x1072, 3, 6, 0x010c, 0x011f, 3, 0x106e, 4, 1, 0x0212, 0x5354, 0, 0x0338, 0x000b, 0x1024,
        5, 1, 0x040c, 0x306e, 5, 0x0423, 0x030c, 0x0228, 0x2307, 0x3420, 3, 0x0438, 5, 0x031f, 3,
        0x0228, 0x2307, 0x0338, 0xffda, 0x106e, 6, 3, 0x040a, 0x043d, 3, 0x3207, 0x0238, 0xffd1,
        0x0322, 7, 0x3070, 7, 0x0213, 0x206e, 8, 0x0030, 0xc728, 0x0011,
    ];
    b.try_regions = vec![DexTryRegion {
        start: 0x19,
        end: 0x2d,
        catches: vec![(Some("Ljava/lang/Exception;".into()), 0x2e)].into(),
    }];
    c
}
fn source(c: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.ReflectionScan", c, &c.methods[0])?.source)
}

#[test]
fn shared_null_handler_continues_iteration() {
    let source = source(&fixture()).unwrap();
    println!("{source}");
    assert!(source.contains("catch (java.lang.Exception"), "{source}");
    assert_eq!(source.matches(".invoke(").count(), 1, "{source}");
    assert!(source.contains("sample.ValidationCheckResult"), "{source}");
}

#[test]
fn effectful_shared_default_is_not_absorbed_into_try() {
    let mut c = fixture();
    let b = c.methods[0].code.as_mut().unwrap();
    // Insert a throwing array-length at the shared default. Both the normal
    // type-test failure and the catch now throw outside the original try.
    b.instructions.insert(0x2e, 0x2321);
    b.instructions[0x0e] = 0x0037;
    b.instructions[0x2d] = 0x0328;
    b.instructions[0x31] = 0xffd9;
    b.instructions[0x3a] = 0xffd0;
    b.instructions[0x43] = 0xc628;
    assert!(source(&c).is_err());
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn generated_java_preserves_reflection_results_and_catch_boundary() {
    use std::{fs, process::Command};
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("sample")).unwrap();
    let method = source(&fixture()).unwrap();
    let java = format!(
        r#"package sample;
public class ReflectionScan {{
 public java.lang.reflect.Method e;
 static String trace="";
 static ReflectionScan active;
 static final RuntimeException directFailure=new IllegalStateException("direct");
 {method}
 public static Object lookup(String key) {{
   trace += key + ";";
   switch(key) {{
     case "nil": return null;
     case "empty": return "";
     case "number": return Integer.valueOf(7);
     case "throw": throw new IllegalArgumentException("wrapped");
     case "error": throw new AssertionError("also wrapped by reflection");
     case "flip": active.e=null; return "flip-value";
     default: return key + "-value";
   }}
 }}
 public Object instanceLookup(String key) {{throw new AssertionError("null receiver");}}
 public static Object wrongSignature() {{throw new AssertionError("wrong arguments");}}
 static void check(boolean value) {{if(!value)throw new AssertionError();}}
 static String summary(java.util.ArrayList values) {{
   String result="";
   for(Object item:values) {{ValidationCheckResult c=(ValidationCheckResult)item;result+=c.key+"="+c.value+";";}}
   return result;
 }}
 public static void main(String[] args) throws Exception {{
   ReflectionScan h=new ReflectionScan(); active=h;
   java.lang.reflect.Method lookup=ReflectionScan.class.getMethod("lookup",String.class);
   h.e=lookup;
   check(summary(h.scan(java.util.Arrays.asList("nil","empty","number","throw","error","one","two"))).equals("one=one-value;two=two-value;"));
   check(trace.equals("nil;empty;number;throw;error;one;two;"));
   trace=""; h.e=null;
   check(h.scan(java.util.Arrays.asList("one","two")).isEmpty() && trace.isEmpty());
   check(h.scan(java.util.Collections.emptyList()).isEmpty());
   h.e=lookup;
   try {{h.scan(null);throw new AssertionError();}} catch(NullPointerException expected) {{}}
   try {{h.scan(java.util.Arrays.asList(null,"one"));throw new AssertionError();}} catch(NullPointerException expected) {{}}
   check(trace.isEmpty());
   try {{h.scan(java.util.Arrays.asList(Integer.valueOf(1),"one"));throw new AssertionError();}} catch(ClassCastException expected) {{}}
   check(trace.isEmpty());
   try {{h.scan(java.util.Arrays.asList("construct","one"));throw new AssertionError();}} catch(RuntimeException expected) {{check(expected==ValidationCheckResult.failure);}}
   check(trace.equals("construct;"));
   trace="";
   java.util.List failingList=new java.util.AbstractList() {{
     public int size() {{return 1;}}
     public Object get(int i) {{throw directFailure;}}
   }};
   try {{h.scan(failingList);throw new AssertionError();}} catch(RuntimeException expected) {{check(expected==directFailure);}}
   check(trace.isEmpty());
   h.e=ReflectionScan.class.getMethod("instanceLookup",String.class);
   check(h.scan(java.util.Arrays.asList("one","two")).isEmpty());
   h.e=ReflectionScan.class.getMethod("wrongSignature");
   check(h.scan(java.util.Arrays.asList("one","two")).isEmpty());
   h.e=Hidden.class.getDeclaredMethod("lookup",String.class);
   check(h.scan(java.util.Arrays.asList("one","two")).isEmpty());
   h.e=lookup; trace="";
   check(summary(h.scan(java.util.Arrays.asList("flip","one"))).equals("flip=flip-value;") && trace.equals("flip;"));
 }}
}}
class ValidationCheckResult {{
 static final RuntimeException failure=new IllegalStateException("constructor outside catch");
 final String key,value;
 ValidationCheckResult(String k,String v) {{ if(k.equals("construct"))throw failure;key=k;value=v; }}
}}
class Hidden {{private static Object lookup(String key) {{throw new AssertionError("inaccessible");}}}}
"#
    );
    fs::write(dir.path().join("sample/ReflectionScan.java"), &java).unwrap();
    for (program, arg) in [
        ("javac", "sample/ReflectionScan.java"),
        ("java", "sample.ReflectionScan"),
    ] {
        let result = Command::new(program)
            .arg(arg)
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{program}: {}\n{java}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
