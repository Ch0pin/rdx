//! A reflection-style search with a shared normal/exception failure path.
use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_java,
};
use std::{collections::BTreeMap, sync::Arc};
#[derive(Default)]
struct Asm {
    words: Vec<u16>,
    labels: BTreeMap<&'static str, usize>,
    fixups: Vec<(usize, &'static str)>,
}
impl Asm {
    fn mark(&mut self, name: &'static str) {
        self.labels.insert(name, self.words.len());
    }
    fn emit(&mut self, words: &[u16]) {
        self.words.extend(words);
    }
    fn branch(&mut self, op: u16, label: &'static str) {
        self.fixups.push((self.words.len(), label));
        self.emit(&[op, 0]);
    }
    fn finish(mut self) -> (Vec<u16>, BTreeMap<&'static str, usize>) {
        for (pc, label) in self.fixups {
            self.words[pc + 1] = (self.labels[label] as isize - pc as isize) as i16 as u16;
        }
        (self.words, self.labels)
    }
}
fn fixture() -> (DexClass, DexMethod) {
    fixture_with_parent(false)
}
fn fixture_with_parent(parent: bool) -> (DexClass, DexMethod) {
    let symbols = Arc::new(DexSymbols {
        types: [
            "Lsample/Api;",
            "Ljava/lang/Class;",
            "Ljava/lang/reflect/Field;",
            "Ljava/lang/String;",
            "Ljava/lang/reflect/AccessibleObject;",
        ]
        .into_iter()
        .map(Into::into)
        .collect(),
        strings: [
            "start",
            "getDeclaredFields",
            "getName",
            "equals",
            "install",
            "getSuperclass",
            "failure",
            "mResources",
            "isAccessible",
        ]
        .into_iter()
        .map(Into::into)
        .collect(),
        protos: vec![
            ("V".into(), vec![]),
            ("[Ljava/lang/reflect/Field;".into(), vec![]),
            ("Ljava/lang/String;".into(), vec![]),
            ("Z".into(), vec!["Ljava/lang/Object;".into()]),
            ("V".into(), vec!["Ljava/lang/reflect/Field;".into()]),
            ("Ljava/lang/Class;".into(), vec![]),
            ("Z".into(), vec![]),
        ],
        methods: vec![
            (0, 0, 0),
            (1, 1, 1),
            (2, 2, 2),
            (3, 3, 3),
            (0, 4, 4),
            (1, 5, 5),
            (0, 0, 6),
            (4, 6, 8),
        ],
        ..Default::default()
    });
    let mut a = Asm::default();
    a.emit(&[0x0012, 0x1612]); // result=false, true
    a.mark("try");
    a.emit(&[0x0071, 0, 0]);
    a.mark("outer");
    a.branch(0x0838, "failure");
    a.emit(&[0x106e, 1, 8, 0x010c, 0x1221, 0x0312]);
    a.mark("inner");
    a.branch(0x2335, "absent");
    a.emit(&[
        0x0446, 0x0301, 0x106e, 2, 4, 0x050c, 0x071a, 7, 0x206e, 3, 0x0075, 0x050a,
    ]);
    a.branch(0x0539, "found");
    a.emit(&[0x03d8, 0x0103]);
    a.branch(0x0029, "inner");
    a.mark("absent");
    a.emit(&[0x0412]);
    a.mark("found");
    a.branch(0x0438, "next");
    if parent {
        a.emit(&[0x106e, 7, 4, 0x050a]);
    }
    a.emit(&[0x1071, 4, 4, 0x6001]);
    a.branch(0x0029, "join");
    a.mark("next");
    a.emit(&[0x106e, 5, 8, 0x080c]);
    a.mark("end");
    a.branch(0x0029, "outer");
    a.mark("failure");
    a.emit(&[0x0071, 6, 0]);
    a.mark("join");
    a.emit(&[0x000f]);
    let (instructions, labels) = a.finish();
    let m = DexMethod {
        declaring_type: "Lsample/Example;".into(),
        name: "run".into(),
        return_type: "Z".into(),
        parameters: vec!["Ljava/lang/Class;".into()],
        thrown_types: vec![],
        access_flags: 9,
        code: Some(DexCode {
            registers: 9,
            ins: 1,
            outs: 2,
            tries: 1,
            try_regions: vec![DexTryRegion {
                start: labels["try"] as u32,
                end: labels["end"] as u32,
                catches: vec![(None, labels["failure"] as u32)].into(),
            }],
            instructions,
            offset: 0,
        }),
    };
    let c = DexClass {
        symbols,
        descriptor: "Lsample/Example;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 0x11,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![],
    };
    if parent {
        c.symbols
            .hierarchy
            .set(Arc::new(
                rdx::native_hierarchy::TypeHierarchy::from_classes([&c]).unwrap(),
            ))
            .unwrap();
    }
    (c, m)
}
#[test]
fn shared_failure_after_protected_search_loop() {
    let (c, m) = fixture();
    let s = native_java::render_method("sample.Example", &c, &m)
        .unwrap()
        .source;
    assert!(s.contains("catch"));
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn jvm_shared_failure_runs_once_and_escapes_its_own_faults() {
    let (c, m) = fixture();
    let source = native_java::render_method("sample.Example", &c, &m)
        .unwrap()
        .source;
    let dir = std::env::temp_dir().join(format!("rdx-shared-catch-loop-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());
    let (parent_c, parent_m) = fixture_with_parent(true);
    let parent_source = native_java::render_method("sample.Example", &parent_c, &parent_m)
        .unwrap()
        .source
        .replacen("run(", "parent(", 1);
    let (early_c, mut early_m) = fixture();
    let code = early_m.code.as_mut().unwrap();
    let install = code
        .instructions
        .windows(3)
        .position(|w| w == [0x1071, 4, 4])
        .unwrap();
    let join = code.instructions.len() - 1;
    code.instructions[install..install + 3].copy_from_slice(&[0x0639, (join - install) as u16, 0]);
    let early_source = native_java::render_method("sample.Example", &early_c, &early_m)
        .unwrap()
        .source
        .replacen("run(", "early(", 1);
    let java = format!(
        r#"package sample;
class Api {{
 static int mode; static String trace;
 static RuntimeException original=new RuntimeException(), handlerFailure=new RuntimeException();
 static Error error=new AssertionError(); static java.io.IOException checked=new java.io.IOException();
 static void start() throws Throwable {{ trace+="S"; if(mode==1)throw original;if(mode==2)throw error;if(mode==3)throw checked; }}
 static void install(java.lang.reflect.Field field) {{ trace+="I";if(mode==4)throw original; }}
 static void failure() {{ trace+="F";if(mode==5)throw handlerFailure; }}
}}
class Holder {{ int mResources; }} class Leaf extends Holder {{ int other; }} class Missing {{ int other; }}
class Example {{ {source} {parent_source} {early_source} }}
public class Runner {{
 static boolean reference(Class<?> type) {{
  boolean result=false;
  try {{ Api.start(); while(type!=null) {{ java.lang.reflect.Field found=null;
   for(java.lang.reflect.Field field:type.getDeclaredFields()) {{ if(field.getName().equals("mResources")) {{ found=field;break; }} }}
   if(found!=null) {{ Api.install(found);result=true;break; }} type=type.getSuperclass();
  }} }} catch(Throwable caught) {{}}
  if(!result)Api.failure(); return result;
 }}
 public static void main(String[] args) throws Throwable {{
  Api.mode=0;
  for(Class<?> type:new Class<?>[]{{Holder.class,Leaf.class,Missing.class,null}}) {{
   Api.trace="";
   if(Example.early(type)||!Api.trace.equals(type==Holder.class||type==Leaf.class?"S":"SF"))throw new AssertionError("conditional early exit: "+Api.trace);
  }}
  for(int mode=0;mode<=5;mode++)for(Class<?> type:new Class<?>[]{{Holder.class,Leaf.class,Missing.class,null}}) {{
   Api.mode=mode; Api.trace=""; boolean expected=false;Throwable expectedFailure=null;
   try{{expected=reference(type);}}catch(Throwable t){{expectedFailure=t;}}String trace=Api.trace;
   for(boolean parent:new boolean[]{{false,true}}) {{
    Api.trace="";boolean actual=false;Throwable actualFailure=null;
    try{{actual=parent?Example.parent(type):Example.run(type);}}catch(Throwable t){{actualFailure=t;}}
    if(actual!=expected||actualFailure!=expectedFailure||!trace.equals(Api.trace))throw new AssertionError(mode+":"+type+":"+Api.trace+" expected "+trace);
   }}
  }}
 }}
}}"#
    );
    let path = dir.join("Runner.java");
    std::fs::write(&path, java).unwrap();
    let o = std::process::Command::new("javac")
        .arg("-d")
        .arg(&dir)
        .arg(path)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "{}\n{source}",
        String::from_utf8_lossy(&o.stderr)
    );
    let o = std::process::Command::new("java")
        .arg("-cp")
        .arg(&dir)
        .arg("sample.Runner")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}
#[test]
fn changed_failure_inputs_reject_invariant_projection() {
    let (mut c, mut m) = fixture();
    let symbols = Arc::get_mut(&mut c.symbols).unwrap();
    symbols
        .protos
        .push(("V".into(), vec!["Ljava/lang/Class;".into()]));
    symbols.methods[6].1 = 7;
    let code = m.code.as_mut().unwrap();
    let handler = code.try_regions[0].catches[0].1 as usize;
    code.instructions[handler] = 0x1071;
    code.instructions[handler + 2] = 8;
    assert!(native_java::render_method("sample.Example", &c, &m).is_err());
}
#[test]
fn handler_write_observed_at_join_rejects_projection() {
    let (c, mut m) = fixture();
    let code = m.code.as_mut().unwrap();
    let handler = code.try_regions[0].catches[0].1 as usize;
    code.instructions[handler..handler + 3].copy_from_slice(&[0x1012, 0, 0]);
    assert!(native_java::render_method("sample.Example", &c, &m).is_err());
}
#[test]
fn changed_join_value_before_throw_rejects_entry_snapshot() {
    let (c, mut m) = fixture();
    let code = m.code.as_mut().unwrap();
    code.instructions[2..5].copy_from_slice(&[0x1012, 0, 0]);
    assert!(native_java::render_method("sample.Example", &c, &m).is_err());
}
#[test]
fn typed_catch_keeps_strict_dispatch_boundary() {
    let (c, mut m) = fixture();
    let region = &mut m.code.as_mut().unwrap().try_regions[0];
    region.catches = vec![(
        Some("Ljava/lang/RuntimeException;".into()),
        region.catches[0].1,
    )]
    .into();
    assert!(native_java::render_method("sample.Example", &c, &m).is_err());
}
#[test]
fn navigation_offsets_match_emitted_names() {
    let (c, m) = fixture();
    let output = native_java::render_method("sample.Example", &c, &m).unwrap();
    for name in [
        "start",
        "getDeclaredFields",
        "getName",
        "equals",
        "install",
        "getSuperclass",
        "failure",
    ] {
        let link = output
            .links
            .iter()
            .find(|link| link.label.contains(&format!(".{name}(")))
            .unwrap();
        assert_eq!(
            output
                .source
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect::<String>(),
            name
        );
    }
}
#[test]
fn reference_exit_accepts_proven_parent_receiver_requirements() {
    let (c, m) = fixture_with_parent(true);
    let source = native_java::render_method("sample.Example", &c, &m)
        .unwrap()
        .source;
    assert!(source.contains("java.lang.reflect.Field"));
    assert!(source.contains(".isAccessible()"));
}
