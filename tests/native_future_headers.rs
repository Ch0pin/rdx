use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;
const FUTURE: &str = "Ljava/util/concurrent/Future;";
const INT: &str = "Ljava/lang/InterruptedException;";
const EXEC: &str = "Ljava/util/concurrent/ExecutionException;";
fn class(owner: &str, parent: &str) -> DexClass {
    DexClass {
        symbols: Arc::new(DexSymbols::default()),
        descriptor: owner.into(),
        superclass: Some(parent.into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![],
    }
}
fn code(words: Vec<u16>, ins: u16, regs: u16) -> DexCode {
    DexCode {
        registers: regs,
        ins,
        outs: 1,
        tries: 0,
        try_regions: vec![],
        instructions: words,
        offset: 0,
    }
}
fn getter(owner: &str, parent: &str) -> DexClass {
    let mut c = class(owner, parent);
    if parent == "Ljava/lang/Object;" {
        c.interfaces = vec![FUTURE.into()];
    }
    c.symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Effects;".into(), INT.into()],
        strings: vec!["before".into(), "problem".into()],
        protos: vec![("V".into(), vec![])],
        methods: vec![(0, 0, 0)],
        fields: vec![(0, 1, 1)],
        ..Default::default()
    });
    c.methods.push(DexMethod {
        declaring_type: c.descriptor.clone(),
        name: "get".into(),
        return_type: "Ljava/lang/Object;".into(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 1,
        code: Some(code(vec![0x0071, 0, 0, 0x0062, 0, 0x0027], 1, 2)),
    });
    c
}
fn attach<'a>(classes: impl IntoIterator<Item = &'a DexClass>) {
    let classes: Vec<_> = classes.into_iter().collect();
    let h = Arc::new(TypeHierarchy::from_classes(classes.iter().copied()).unwrap());
    for c in classes {
        c.symbols.hierarchy.set(h.clone()).unwrap();
    }
}
fn source(c: &DexClass) -> Result<String, String> {
    native_java::render_method("sample.Generated", c, &c.methods[0])
        .map(|s| s.source)
        .map_err(|e| format!("{e:#}"))
}
fn assert_original_caller_header_and_caught_identity(source: &str) {
    assert!(
        source.lines().next().unwrap().contains(
            "throws java.lang.InterruptedException, java.util.concurrent.ExecutionException"
        ),
        "{source}"
    );
    assert!(source.contains("p0.get();"), "{source}");
    let caught = source
        .split("catch (java.lang.Throwable ")
        .nth(1)
        .unwrap()
        .split(')')
        .next()
        .unwrap();
    assert!(
        source.contains(&format!("throw {caught};"))
            || source.lines().any(
                |line| line.trim_start().starts_with("throw _rdx_caught_throw_")
                    && line.contains(&format!("({caught});"))
            ),
        "{source}"
    );
}
#[test]
fn exact_sdk_header_restores_nonfinal_loaded_override_chain() {
    let parent = getter("Lsample/Parent;", "Ljava/lang/Object;");
    let child = getter("Lsample/Child;", "Lsample/Parent;");
    attach([&parent, &child]);
    for c in [&parent, &child] {
        let s = source(c).unwrap();
        assert!(
            s.contains(
                "throws java.util.concurrent.ExecutionException, java.lang.InterruptedException"
            ),
            "{s}"
        );
    }
}
#[test]
fn explicit_narrower_ancestor_contract_intersects_sdk_header() {
    let mut parent = getter("Lsample/Parent;", "Ljava/lang/Object;");
    parent.methods[0].thrown_types = vec![INT.into()];
    let child = getter("Lsample/Child;", "Lsample/Parent;");
    attach([&parent, &child]);
    let s = source(&child).unwrap();
    assert!(s.contains("throws java.lang.InterruptedException"), "{s}");
    assert!(
        !s.contains("throws java.util.concurrent.ExecutionException"),
        "{s}"
    );
}
#[test]
fn incompatible_sibling_unknown_cycle_and_sdk_shadow_remain_rejected() {
    for variant in 0..5 {
        let mut c = getter("Lsample/Root;", "Ljava/lang/Object;");
        let mut other = class("Lsample/Other;", "Ljava/lang/Object;");
        match variant {
            0 => {
                other.access_flags = 0x601;
                let mut m = DexMethod {
                    declaring_type: other.descriptor.clone(),
                    name: "get".into(),
                    return_type: "Ljava/lang/Object;".into(),
                    parameters: vec![],
                    thrown_types: vec![],
                    access_flags: 0x401,
                    code: None,
                };
                m.declaring_type = other.descriptor.clone();
                other.methods.push(m);
                c.interfaces.push(other.descriptor.clone());
            }
            1 => c.interfaces.push("Lvendor/Unknown;".into()),
            2 => {
                other.superclass = Some(c.descriptor.clone());
                c.superclass = Some(other.descriptor.clone());
            }
            3 => {
                other.descriptor = FUTURE.into();
                other.access_flags = 0x601;
            }
            _ => {
                c.interfaces.push(other.descriptor.clone());
                other.interfaces.push(FUTURE.into());
                other.access_flags = 0x601;
                other.methods.push(DexMethod {
                    declaring_type: other.descriptor.clone(),
                    name: "get".into(),
                    return_type: "Ljava/lang/Object;".into(),
                    parameters: vec![],
                    thrown_types: vec![],
                    access_flags: 0x401,
                    code: None,
                });
            }
        }
        attach([&c, &other]);
        assert!(source(&c).is_err(), "variant {variant}");
    }
}
#[test]
fn duplicate_return_and_dispatch_mismatches_cannot_restore_sdk_header() {
    for variant in 0..3 {
        let mut c = getter("Lsample/Root;", "Ljava/lang/Object;");
        if variant == 0 {
            let m = &c.methods[0];
            c.methods.push(DexMethod {
                declaring_type: c.descriptor.clone(),
                name: m.name.clone(),
                return_type: "Ljava/lang/String;".into(),
                parameters: vec![],
                thrown_types: vec![],
                access_flags: 1,
                code: Some(code(vec![0x0012, 0x0011], 1, 2)),
            });
        } else if variant == 1 {
            c.methods[0].return_type = "I".into();
        } else {
            c.methods[0].access_flags = 9;
            c.methods[0].code.as_mut().unwrap().ins = 0;
        }
        attach([&c]);
        let result = source(&c);
        assert!(
            result.is_err()
                || result
                    .as_ref()
                    .is_ok_and(|s| !s.contains("throws java.util.concurrent.ExecutionException")),
            "variant {variant}: {result:?}"
        );
    }
}
fn caller(callee: &DexClass, protected: bool, declared: bool) -> DexClass {
    let mut c = class("Lsample/Caller;", "Ljava/lang/Object;");
    c.symbols = Arc::new(DexSymbols {
        types: vec![callee.descriptor.clone()],
        strings: vec!["get".into()],
        protos: vec![("Ljava/lang/Object;".into(), vec![])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let mut body = code(
        if protected {
            vec![0x106e, 0, 2, 0x000e, 0x000d, 0x0027]
        } else {
            vec![0x106e, 0, 2, 0x000e]
        },
        1,
        3,
    );
    if protected {
        body.tries = 1;
        body.try_regions = vec![DexTryRegion {
            start: 0,
            end: 3,
            catches: vec![(None, 4)].into(),
        }];
    }
    c.methods.push(DexMethod {
        declaring_type: c.descriptor.clone(),
        name: "run".into(),
        return_type: "V".into(),
        parameters: vec![callee.descriptor.clone()],
        thrown_types: if declared {
            vec![INT.into(), EXEC.into()]
        } else {
            vec![]
        },
        access_flags: 9,
        code: Some(body),
    });
    c
}
#[test]
fn unknown_explicit_throw_callee_retains_unknown_protected_effects() {
    let mut callee = getter("Lsample/Root;", "Ljava/lang/Object;");
    Arc::get_mut(&mut callee.symbols).unwrap().types[1] = "Ljava/lang/Throwable;".into();
    let caller = caller(&callee, true, true);
    attach([&callee, &caller]);
    let h = callee.symbols.hierarchy.get().unwrap();
    assert_eq!(
        h.call_checked_effects(&callee.descriptor, "get", &[], "Ljava/lang/Object;", false)
            .unwrap(),
        vec![Arc::<str>::from("Ljava/lang/Throwable;")]
    );
    assert!(source(&callee).is_err());
    // The exact emitted Future declaration and source-only caught bridge are
    // independent of this callee's unsupported body. Effects stay unknown.
    assert_original_caller_header_and_caught_identity(&source(&caller).unwrap());
}
#[test]
fn closed_empty_body_header_provides_exact_protected_rethrow_contract() {
    let mut callee = getter("Lsample/Root;", "Ljava/lang/Object;");
    callee.methods[0].code.as_mut().unwrap().instructions = vec![0x0012, 0x0011];
    let caller = caller(&callee, true, true);
    attach([&callee, &caller]);
    let h = callee.symbols.hierarchy.get().unwrap();
    assert_eq!(
        h.call_checked_effects(&callee.descriptor, "get", &[], "Ljava/lang/Object;", false)
            .unwrap(),
        vec![Arc::<str>::from(EXEC), Arc::<str>::from(INT)]
    );
    let s = source(&caller).unwrap();
    assert!(s.contains("catch (java.lang.Throwable"), "{s}");
}
#[test]
fn unprotected_loaded_caller_blocks_header_change() {
    let callee = getter("Lsample/Root;", "Ljava/lang/Object;");
    let caller = caller(&callee, false, false);
    attach([&callee, &caller]);
    assert!(source(&callee).is_err());
}

#[test]
fn unchecked_missing_body_malformed_and_unknown_callee_effects_stay_unknown() {
    for variant in 0..4 {
        let mut callee = getter("Lsample/Root;", "Ljava/lang/Object;");
        match variant {
            0 => {
                Arc::get_mut(&mut callee.symbols).unwrap().types[1] =
                    "Ljava/lang/RuntimeException;".into()
            }
            1 => callee.methods[0].code = None,
            2 => callee.methods[0].code.as_mut().unwrap().instructions = vec![0x0012, 0x0029],
            _ => {
                let symbols = Arc::get_mut(&mut callee.symbols).unwrap();
                symbols.types[0] = "Lvendor/Unknown;".into();
                callee.methods[0].code.as_mut().unwrap().instructions =
                    vec![0x0071, 0, 0, 0x0012, 0x0011];
            }
        }
        let caller = caller(&callee, true, true);
        attach([&callee, &caller]);
        let h = callee.symbols.hierarchy.get().unwrap();
        assert_eq!(
            h.call_checked_effects(&callee.descriptor, "get", &[], "Ljava/lang/Object;", false)
                .unwrap(),
            vec![Arc::<str>::from("Ljava/lang/Throwable;")],
            "variant {variant}"
        );
        if matches!(variant, 1 | 2) {
            assert!(
                source(&callee).is_err(),
                "malformed/unavailable callee body {variant}"
            );
        }
        assert_original_caller_header_and_caught_identity(&source(&caller).unwrap());
    }
}
#[test]
fn outer_caller_obligations_close_through_exact_wrapper_dependency() {
    for declared in [false, true] {
        let callee = getter("Lsample/Base;", "Ljava/lang/Object;");
        let mut wrapper = getter("Lsample/Wrapper;", "Lsample/Base;");
        wrapper.symbols = Arc::new(DexSymbols {
            types: vec![callee.descriptor.clone()],
            strings: vec!["get".into()],
            protos: vec![("Ljava/lang/Object;".into(), vec![])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        wrapper.methods[0].code = Some(code(vec![0x106f, 0, 1, 0x000c, 0x0011], 1, 2));
        let outer = caller(&wrapper, false, declared);
        attach([&callee, &wrapper, &outer]);
        if declared {
            for c in [&callee, &wrapper] {
                let s = source(c).unwrap();
                assert!(s.contains("throws java.util.concurrent.ExecutionException, java.lang.InterruptedException"),"{s}");
            }
        } else {
            assert!(source(&callee).is_err());
            let s = source(&wrapper).unwrap();
            assert!(
                !s.contains("throws java.util.concurrent.ExecutionException"),
                "{s}"
            );
        }
    }
}
#[test]
#[ignore = "requires javac and java"]
fn jvm_original_future_get_header_keeps_fault_identity_and_effect_order() {
    use std::{fs, process::Command};
    struct Temp(std::path::PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let t = Temp(std::env::temp_dir().join(format!(
            "rdx-future-header-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )));
    fs::create_dir_all(t.0.join("sample")).unwrap();
    let c = getter("Lsample/Generated;", "Ljava/lang/Object;");
    attach([&c]);
    let method = source(&c).unwrap();
    let java = format!(
        r#"package sample;
import java.util.concurrent.*;
final class Generated implements Future<Object>{{public boolean cancel(boolean b){{return false;}}public boolean isCancelled(){{return false;}}public boolean isDone(){{return true;}}public Object get(long n,TimeUnit u)throws ExecutionException,InterruptedException,TimeoutException{{return get();}}{method}}}
final class Effects{{static InterruptedException problem;static Throwable fault;static int calls;static void before(){{calls++;if(fault instanceof RuntimeException)throw(RuntimeException)fault;if(fault instanceof Error)throw(Error)fault;}}}}
public class Verify{{public static void main(String[] args)throws Exception{{Throwable[] faults={{null,new IllegalStateException("before"),new AssertionError("before")}};for(int m=0;m<2;m++)for(Throwable f:faults){{Effects.problem=new InterruptedException("original "+m);Effects.fault=f;Effects.calls=0;Throwable seen=null;try{{new Generated().get();}}catch(Throwable e){{seen=e;}}if(seen!=(f==null?Effects.problem:f)||Effects.calls!=1)throw new AssertionError("identity/order");}}System.out.println("6 original Future identity/order cases passed");}}}}
"#
    );
    let path = t.0.join("sample/Verify.java");
    fs::write(&path, java).unwrap();
    let compile = Command::new("javac")
        .arg("-d")
        .arg(&t.0)
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}\n{method}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new("java")
        .arg("-cp")
        .arg(&t.0)
        .arg("sample.Verify")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout)
            .contains("6 original Future identity/order cases passed")
    );
}

#[test]
#[ignore = "requires javac/java; root runs serially"]
fn jvm_unknown_body_caller_preserves_caught_object_and_original_future_header() {
    use std::{fs, process::Command};
    let mut callee = getter("Lsample/Root;", "Ljava/lang/Object;");
    Arc::get_mut(&mut callee.symbols).unwrap().types[1] = "Ljava/lang/Throwable;".into();
    let caller = caller(&callee, true, true);
    attach([&callee, &caller]);
    assert!(source(&callee).is_err());
    let generated = native_java::render("sample.Caller", &caller)
        .unwrap()
        .source;
    let dir =
        std::env::temp_dir().join(format!("rdx-future-unknown-caller-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/Caller.java"), generated).unwrap();
    fs::write(dir.join("sample/Root.java"),r#"package sample;
import java.util.concurrent.*;
public class Root implements Future<Object>{
public boolean cancel(boolean b){return false;}public boolean isCancelled(){return false;}public boolean isDone(){return true;}
public Object get(long time,TimeUnit unit)throws ExecutionException,InterruptedException,TimeoutException{return get();}
public Object get()throws ExecutionException,InterruptedException {Effects.before();throw Root.<RuntimeException>original(Effects.problem);}
@SuppressWarnings("unchecked")private static <E extends Throwable> RuntimeException original(Throwable value)throws E {throw(E)value;}
}
"#).unwrap();
    fs::write(dir.join("sample/Effects.java"),r#"package sample;
public final class Effects {static Throwable problem,fault;static String trace;static void before(){trace+="B";if(fault instanceof RuntimeException)throw(RuntimeException)fault;if(fault instanceof Error)throw(Error)fault;}}
"#).unwrap();
    fs::write(dir.join("sample/Verify.java"),r#"package sample;public class Verify {
static Throwable expected(Root value){try{value.get();return null;}catch(Throwable failure){return failure;}}
public static void main(String[] args)throws Exception {
Class<?>[] header=Caller.class.getMethod("run",Root.class).getExceptionTypes();
if(!java.util.Arrays.equals(header,new Class<?>[]{InterruptedException.class,java.util.concurrent.ExecutionException.class}))throw new AssertionError("header");
for(Throwable primary:new Throwable[]{new InterruptedException("I"),new java.io.IOException("IO"),new Throwable("T")})for(Throwable fault:new Throwable[]{null,new IllegalStateException("before"),new AssertionError("before")}){
Effects.problem=primary;Effects.fault=fault;Effects.trace="";Throwable wanted=expected(new Root());String trace=Effects.trace;Effects.trace="";Throwable seen=null;
try{Caller.run(new Root());}catch(Throwable failure){seen=failure;}
if(seen!=wanted||seen!=(fault==null?primary:fault)||!Effects.trace.equals(trace)||!trace.equals("B"))throw new AssertionError("identity/order");
}}
}
"#).unwrap();
    let compile = Command::new("javac")
        .args([
            "-Xlint:all",
            "-Werror",
            "-d",
            dir.to_str().unwrap(),
            "sample/Caller.java",
            "sample/Root.java",
            "sample/Effects.java",
            "sample/Verify.java",
        ])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new("java")
        .args(["-cp", dir.to_str().unwrap(), "sample.Verify"])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
