use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_java,
};
use std::sync::Arc;

fn fixture(words: Vec<u16>, ranges: &[(u32, u32, u32)]) -> DexClass {
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
            strings: vec!["effect".into()],
            types: vec!["Lsample/Monitor;".into()],
            protos: vec![("I".into(), vec![])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/Monitor;".into(),
            name: "test".into(),
            return_type: "I".into(),
            parameters: vec!["Ljava/lang/Object;".into()],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 3,
                ins: 1,
                outs: 0,
                tries: ranges.len() as u16,
                try_regions: ranges
                    .iter()
                    .map(|&(start, end, handler)| DexTryRegion {
                        start,
                        end,
                        catches: vec![(None, handler)].into(),
                    })
                    .collect(),
                instructions: words,
                offset: 0,
            }),
        }],
    }
}
fn detached() -> DexClass {
    fixture(
        vec![
            0x021d, 0x0071, 0, 0, 0x000a, 0x021e, 0x000f, 0x010d, 0x021e, 0x0127,
        ],
        &[(1, 6, 7), (8, 9, 7)],
    )
}
fn render(class: &DexClass) -> anyhow::Result<rdx::engine::DecompiledCode> {
    native_java::render_method("sample.Monitor", class, &class.methods[0])
}
#[test]
fn detached_release_keeps_result_in_scope_and_call_inside_monitor() {
    let code = render(&detached()).unwrap();
    let begin = code.source.find("synchronized (p0)").unwrap();
    let call = code.source.find("sample.Monitor.effect()").unwrap();
    let end = code.source[call..].find('}').unwrap() + call;
    assert!(begin < call && call < end, "{}", code.source);
    assert_eq!(code.source.matches(".effect()").count(), 1);
    assert!(code.source[end..].contains("return "), "{}", code.source);
    assert!(
        code.links
            .iter()
            .any(|link| link.label == "sample.Monitor.effect()I")
    );
    for link in &code.links {
        assert!(link.start < link.end && link.end <= code.source.chars().count());
    }
}
#[test]
fn interleaved_cleanup_and_release_gotos_render() {
    let class = fixture(
        vec![
            0x021d, 0x0071, 0, 0, 0x000a, 0x0328, 0x010d, 0x0328, 0x021e, 0x0328, 0x021e, 0x0127,
            0x000f,
        ],
        &[(1, 11, 6)],
    );
    assert!(render(&class).unwrap().source.contains("synchronized (p0)"));
}
#[test]
fn malformed_monitor_shapes_fail_closed() {
    for (pc, word) in [
        (5, 0x011e),
        (8, 0x001e),
        (9, 0x0027),
        (7, 0x020d),
        (4, 0x020a),
        (5, 0x000f),
        (5, 0x021d),
    ] {
        let mut class = detached();
        class.methods[0].code.as_mut().unwrap().instructions[pc] = word;
        assert!(
            render(&class).is_err(),
            "accepted malformed monitor at {pc}"
        );
    }
    let mut class = detached();
    class.methods[0].code.as_mut().unwrap().try_regions[0].end = 2;
    assert!(render(&class).is_err());
    let mut class = detached();
    class.methods[0].code.as_mut().unwrap().try_regions[0].catches =
        vec![(Some("Ljava/lang/Exception;".into()), 7)].into();
    assert!(render(&class).is_err());
}
#[test]
fn external_jump_into_protected_body_is_rejected() {
    // An alternate entry before acquisition reaches the protected call directly.
    let class = fixture(
        vec![
            0x0238, 3, 0x021d, 0x0071, 0, 0, 0x000a, 0x021e, 0x000f, 0x010d, 0x021e, 0x0127,
        ],
        &[(3, 8, 9)],
    );
    assert!(render(&class).is_err());
}

// Independent bounded evaluators for these monitor fixtures. This is a semantic
// regression for the supported shape, not Android/JVM execution or a general VM.
fn dex_trace(class: &DexClass, null_lock: bool, throws: bool) -> Vec<String> {
    dex_trace_at(class, null_lock, throws.then_some(1))
}
fn dex_trace_at(class: &DexClass, null_lock: bool, throw_at: Option<usize>) -> Vec<String> {
    let mut calls = 0;
    let code = class.methods[0].code.as_ref().unwrap();
    let mut pc = 0;
    let mut held = false;
    let mut trace = vec![];
    let mut result = 0;
    for _ in 0..100 {
        let instruction_pc = pc;
        let op = code.instructions[pc] as u8;
        let mut error = None;
        match op {
            0x1d => {
                trace.push("enter".into());
                if null_lock {
                    error = Some("NPE");
                } else {
                    held = true;
                }
                pc += 1;
            }
            0x1e => {
                assert!(held);
                held = false;
                trace.push("release".into());
                pc += 1;
            }
            0x00 => pc += 1,
            0x71 => {
                trace.push("effect".into());
                calls += 1;
                if throw_at == Some(calls) {
                    error = Some("effect-error");
                } else {
                    result = 7;
                }
                pc += 3;
            }
            0x0a | 0x0d => pc += 1,
            0x28 => pc = (pc as isize + (code.instructions[pc] >> 8) as i8 as isize) as usize,
            0x0f => {
                assert!(!held);
                trace.push(format!("return:{result}"));
                return trace;
            }
            0x27 => error = Some("effect-error"),
            _ => panic!("unsupported evaluator opcode {op:x}"),
        }
        if let Some(error) = error {
            if let Some(region) = code.try_regions.iter().find(|region| {
                region.start as usize <= instruction_pc && instruction_pc < region.end as usize
            }) {
                pc = region.catches[0].1 as usize;
            } else {
                assert!(!held);
                trace.push(format!("throw:{error}"));
                return trace;
            }
        }
    }
    panic!("fixture evaluator budget");
}
fn java_trace(source: &str, null_lock: bool, throws: bool) -> Vec<String> {
    java_trace_at(source, null_lock, throws.then_some(1))
}
fn java_trace_at(source: &str, null_lock: bool, throw_at: Option<usize>) -> Vec<String> {
    let mut calls = 0;
    use std::collections::HashMap;
    let mut values = HashMap::<String, i32>::new();
    let mut held = false;
    let mut trace = vec![];
    for line in source.lines().map(str::trim) {
        if line.starts_with("public static ") || line.is_empty() {
            continue;
        }
        if line == "synchronized (p0) {" {
            trace.push("enter".into());
            if null_lock {
                trace.push("throw:NPE".into());
                return trace;
            }
            held = true;
            continue;
        }
        if line == "{" {
            continue;
        } // negative control: ordinary block
        if line == "}" {
            if held {
                held = false;
                trace.push("release".into());
            }
            continue;
        }
        if let Some(value) = line
            .strip_prefix("return ")
            .and_then(|s| s.strip_suffix(';'))
        {
            assert!(!held);
            trace.push(format!("return:{}", values[value]));
            return trace;
        }
        let assignment = if line == "sample.Monitor.effect();" {
            Some(("ignored", "sample.Monitor.effect()"))
        } else {
            line.trim_end_matches(';').split_once(" = ")
        };
        if let Some((left, right)) = assignment {
            let name = left.strip_prefix("int ").unwrap_or(left);
            let value = if right == "sample.Monitor.effect()" {
                trace.push("effect".into());
                calls += 1;
                if throw_at == Some(calls) {
                    if held {
                        trace.push("release".into());
                    }
                    trace.push("throw:effect-error".into());
                    return trace;
                }
                7
            } else {
                *values.get(right).expect("unsupported Java expression")
            };
            values.insert(name.to_string(), value);
            continue;
        }
        assert!(
            line.starts_with("int ") && line.ends_with(';'),
            "unsupported Java line: {line}"
        );
    }
    panic!("missing Java return");
}
#[test]
fn monitor_semantics_normal_null_and_exception_paths_match() {
    for class in [
        detached(),
        fixture(
            vec![
                0x021d, 0x0071, 0, 0, 0x000a, 0x0328, 0x010d, 0x0328, 0x021e, 0x0328, 0x021e,
                0x0127, 0x000f,
            ],
            &[(1, 11, 6)],
        ),
    ] {
        let source = render(&class).unwrap().source;
        for (null_lock, throws) in [(false, false), (true, false), (false, true)] {
            let expected = dex_trace(&class, null_lock, throws);
            assert_eq!(java_trace(&source, null_lock, throws), expected, "{source}");
            let missing_monitor = source.replace("synchronized (p0) {", "{");
            assert_ne!(java_trace(&missing_monitor, null_lock, throws), expected);
        }
    }
}

#[test]
#[ignore = "Requires RDX_TEST_APK set to Expedia"]
fn hilt_component_manager_monitor_region() {
    use rdx::{engine::DecompilerEngine, native_engine::NativeDexEngine};
    let path = std::env::var_os("RDX_TEST_APK").unwrap();
    let mut engine = NativeDexEngine::default();
    engine.open(std::path::Path::new(&path)).unwrap();
    let name =
        "com.expedia.bookings.loyalty.nondismissiblebanner.Hilt_NonDismissibleBannerActivity";
    let class = engine.class(name).unwrap();
    let method = class
        .methods
        .iter()
        .find(|m| m.name.as_ref() == "componentManager" && m.return_type.as_ref() == "Lcs5/a;")
        .unwrap();
    let code = native_java::render_method(name, class, method).unwrap();
    assert_eq!(code.source.matches("synchronized (").count(), 1);
    assert_eq!(code.source.matches(".createComponentManager()").count(), 1);
    std::fs::write("target/validation/hilt-componentManager.java", code.source).unwrap();
}

#[test]
fn unprotected_monitor_instructions_return_errors_without_panicking() {
    for opcode in [0x021d, 0x021e] {
        let class = fixture(vec![opcode, 0x0012, 0x000f], &[]);
        let error = render(&class).unwrap_err();
        assert!(
            error.to_string().contains("monitor instruction"),
            "{error:#}"
        );
    }
}

#[test]
fn nonthrowing_tail_outside_try_preserves_release_on_every_path() {
    let mut class = detached();
    class.methods[0].code.as_mut().unwrap().try_regions[0].end = 4;
    let java = render(&class).unwrap();
    for (null, throws) in [(false, false), (true, false), (false, true)] {
        assert_eq!(
            dex_trace(&class, null, throws),
            java_trace(&java.source, null, throws)
        );
    }
}

#[test]
fn synchronized_loop_allows_unprotected_branch_but_not_unprotected_call() {
    let mut class = fixture(
        vec![
            0x021d, 0x2012, 0x0071, 0, 0, 0x00d8, 0xff00, 0x0039, 0xfffb, 0x021e, 0x7012, 0x000f,
            0x010d, 0x021e, 0x0127,
        ],
        &[(1, 7, 12)],
    );
    let java = render(&class).unwrap().source;
    assert!(java.contains("synchronized (p0)"), "{java}");
    assert!(java.contains("while ("), "{java}");
    assert_eq!(java.matches("sample.Monitor.effect()").count(), 1);
    class.methods[0].code.as_mut().unwrap().try_regions[0].end = 2;
    assert!(
        render(&class)
            .unwrap_err()
            .to_string()
            .contains("throwing monitor body")
    );
}

fn split_protection() -> DexClass {
    fixture(
        vec![
            0x021d, 0x0071, 0, 0, 0x000a, 0, 0x0071, 0, 0, 0x000a, 0x021e, 0x000f, 0x010d, 0x021e,
            0x0127,
        ],
        &[(1, 5, 12), (6, 11, 12), (13, 14, 12)],
    )
}

#[test]
fn split_monitor_protection_keeps_each_throwing_effect_protected() {
    let class = split_protection();
    let source = render(&class).unwrap().source;
    assert_eq!(source.matches("synchronized (").count(), 1, "{source}");
    assert_eq!(source.matches(".effect()").count(), 2, "{source}");
    for (null, throw_at) in [
        (true, None),
        (false, None),
        (false, Some(1)),
        (false, Some(2)),
    ] {
        assert_eq!(
            dex_trace_at(&class, null, throw_at),
            java_trace_at(&source, null, throw_at),
            "{source}"
        );
    }
    // Removing protection for the second effect cannot be treated as if its
    // exception automatically released the monitor.
    let mut unprotected = split_protection();
    let code = unprotected.methods[0].code.as_mut().unwrap();
    code.try_regions.remove(1);
    code.tries -= 1;
    assert!(
        render(&unprotected)
            .unwrap_err()
            .to_string()
            .contains("throwing monitor body")
    );
    // A different exception dispatch is likewise not equivalent to the shared
    // catch-all release path.
    let mut different_dispatch = split_protection();
    different_dispatch.methods[0]
        .code
        .as_mut()
        .unwrap()
        .try_regions[1]
        .catches = vec![(Some("Ljava/lang/RuntimeException;".into()), 12)].into();
    assert!(render(&different_dispatch).is_err());
}

fn returning_releases() -> DexClass {
    fixture(
        vec![
            0x021d, 0x0071, 0, 0, 0x000a, 0x0038, 4, 0x021e, 0x000f, 0x0071, 0, 0, 0x000a, 0x021e,
            0x000f, 0x010d, 0x021e, 0x0127,
        ],
        &[(1, 7, 15), (9, 14, 15), (16, 17, 15)],
    )
}

#[test]
fn multiple_releases_with_immediate_returns_reconstruct() {
    let class = returning_releases();
    let source = render(&class).unwrap().source;
    assert_eq!(source.matches("synchronized (").count(), 1, "{source}");
    assert_eq!(source.matches(".effect()").count(), 2, "{source}");
    assert_eq!(source.matches("return ").count(), 2, "{source}");
    let mut after_release = returning_releases();
    after_release.methods[0].code.as_mut().unwrap().instructions[8] = 0x0012;
    assert!(
        render(&after_release).is_err(),
        "post-release effects must not move inside monitor"
    );
    let mut wrong_lock = returning_releases();
    wrong_lock.methods[0].code.as_mut().unwrap().instructions[13] = 0x011e;
    assert!(render(&wrong_lock).is_err());
}

#[test]
#[ignore = "requires javac and java"]
fn multiple_return_monitor_executes_with_real_jvm_release_semantics() {
    use std::process::Command;
    let source = render(&returning_releases()).unwrap().source;
    let dir = std::path::Path::new("target/validation/multiple-return-monitor-jvm/sample");
    std::fs::create_dir_all(dir).unwrap();
    let java = format!(
        r#"package sample;
public class Monitor {{
    static final Object LOCK = new Object();
    static int first, fail, calls;
    static class Marker extends RuntimeException {{}}
    public static int effect() {{
        if (!Thread.holdsLock(LOCK)) throw new AssertionError("effect without lock");
        calls++;
        if (calls == fail) throw new Marker();
        return calls == 1 ? first : 9;
    }}
    {source}
    public static void main(String[] args) {{
        for (first = 0; first <= 7; first += 7) {{
            for (fail = 0; fail <= 2; fail++) {{
                calls = 0;
                boolean throwsExpected = fail == 1 || (fail == 2 && first == 0);
                try {{
                    int value = test(LOCK);
                    if (throwsExpected || value != (first == 0 ? 9 : 7)) throw new AssertionError("result");
                }} catch (Marker e) {{ if (!throwsExpected) throw new AssertionError("unexpected exception"); }}
                if (Thread.holdsLock(LOCK)) throw new AssertionError("monitor leaked");
                int expectedCalls = fail == 1 || first == 7 ? 1 : 2;
                if (calls != expectedCalls) throw new AssertionError("call order");
            }}
        }}
        calls = 0;
        try {{ test(null); throw new AssertionError("null lock accepted"); }}
        catch (NullPointerException expected) {{}}
        if (calls != 0) throw new AssertionError("effect before lock");
        System.out.println("monitor paths verified");
    }}
}}"#
    );
    let path = dir.join("Monitor.java");
    std::fs::write(&path, java).unwrap();
    let compiled = Command::new("javac").arg(&path).output().unwrap();
    assert!(
        compiled.status.success(),
        "{}\n{source}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let executed = Command::new("java")
        .args([
            "-cp",
            "target/validation/multiple-return-monitor-jvm",
            "sample.Monitor",
        ])
        .output()
        .unwrap();
    assert!(
        executed.status.success(),
        "{}",
        String::from_utf8_lossy(&executed.stderr)
    );
    assert!(String::from_utf8_lossy(&executed.stdout).contains("monitor paths verified"));
}

#[test]
fn exception_alias_moves_preserve_cleanup_rethrow() {
    let class = fixture(
        vec![
            0x021d, 0x0071, 0, 0, 0x000a, 0x021e, 0x000f, 0x010d, 0x1007, 0x021e, 0x0027,
        ],
        &[(1, 6, 7)],
    );
    assert!(render(&class).unwrap().source.contains("synchronized (p0)"));
    let mut wrong = class;
    wrong.methods[0].code.as_mut().unwrap().instructions[8] = 0x2007;
    assert!(
        render(&wrong).is_err(),
        "unrelated reference cannot replace caught exception"
    );
    let mut lock = wrong;
    lock.methods[0].code.as_mut().unwrap().instructions[8] = 0x1207;
    assert!(
        render(&lock).is_err(),
        "cleanup must not overwrite owned lock"
    );
}

#[test]
fn monitor_mixed_early_return_and_post_release_effect() {
    let class = fixture(
        vec![
            0x021d, 0x0071, 0, 0, 0x000a, 0x0038, 4, 0x021e, 0x000f, 0x021e, 0x0071, 0, 0, 0x000a,
            0x000f, 0x010d, 0x021e, 0x0127,
        ],
        &[(1, 7, 15)],
    );
    let source = render(&class).unwrap().source;
    assert_eq!(source.matches("synchronized").count(), 1, "{source}");
    assert_eq!(source.matches(".effect()").count(), 2, "{source}");
    let tail = source.rfind(".effect()").unwrap();
    assert!(
        source[..tail].matches('}').count() >= 2,
        "last effect must be outside lock: {source}"
    );
}

#[test]
fn multiple_releases_can_jump_to_shared_return() {
    let class = fixture(
        vec![
            0x021d, 0x0071, 0, 0, 0x000a, 0x0038, 4, 0x021e, 0x0328, 0x021e, 0x0128, 0x000f,
            0x010d, 0x021e, 0x0127,
        ],
        &[(1, 7, 12)],
    );
    let source = render(&class).unwrap().source;
    assert!(source.contains("synchronized"), "{source}");
    assert_eq!(source.matches(".effect()").count(), 1, "{source}");
}

#[test]
#[ignore = "requires javac and java"]
fn mixed_monitor_exits_execute_with_correct_lock_ownership() {
    use std::process::Command;
    let class = fixture(
        vec![
            0x021d, 0x0071, 0, 0, 0x000a, 0x0038, 4, 0x021e, 0x000f, 0x021e, 0x0071, 0, 0, 0x000a,
            0x000f, 0x010d, 0x1007, 0x021e, 0x0027,
        ],
        &[(1, 7, 15)],
    );
    let source = render(&class).unwrap().source;
    let dir = std::env::temp_dir().join(format!("rdx-mixed-monitor-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    std::fs::write(dir.join("sample/Monitor.java"),format!(r#"package sample;
public class Monitor {{
 static Object lock=new Object();static int calls,mode;
 static int effect(){{calls++;if(Thread.holdsLock(lock)!=(calls==1))throw new AssertionError("lock ownership");
  if(mode==2)throw new IllegalStateException();return calls==1?mode:42;}}
 {source}
 public static void main(String[] args){{for(mode=0;mode<3;mode++){{calls=0;
  try{{int result=test(lock);if(mode==2||result!=(mode==0?42:1))throw new AssertionError();}}
  catch(IllegalStateException e){{if(mode!=2)throw new AssertionError();}}
  if(Thread.holdsLock(lock)||calls!=(mode==0?2:1))throw new AssertionError();
  final boolean[] entered={{false}};
  Thread t=new Thread(()->{{synchronized(lock){{entered[0]=true;}}}});t.start();
  try{{t.join(1000);}}catch(InterruptedException e){{throw new AssertionError(e);}}
  if(!entered[0])throw new AssertionError("lock leaked");
 }}}}
}}"#)).unwrap();
    for (command, args) in [
        ("javac", vec!["sample/Monitor.java"]),
        ("java", vec!["-cp", ".", "sample.Monitor"]),
    ] {
        let result = Command::new(command)
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{command}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn typed_handler_inside_monitor_preserves_outer_cleanup() {
    let mut class = fixture(
        vec![
            0x021d, 0x0071, 0, 0, 0x000a, 0x0328, 0x010d, 0x3012, 0x021e, 0x000f, 0x010d, 0x021e,
            0x0127,
        ],
        &[(1, 5, 10), (6, 8, 10)],
    );
    class.methods[0].code.as_mut().unwrap().try_regions[0].catches =
        vec![(Some("Ljava/lang/RuntimeException;".into()), 6), (None, 10)].into();
    let source = render(&class).unwrap().source;
    assert_eq!(source.matches("synchronized").count(), 1, "{source}");
    assert_eq!(
        source.matches("catch (java.lang.RuntimeException").count(),
        1,
        "{source}"
    );
    assert!(!source.contains("catch (java.lang.Throwable"), "{source}");
    // A typed handler's throwing operation may not escape outer cleanup.
    let mut invalid = fixture(
        vec![
            0x021d, 0x0071, 0, 0, 0x000a, 0x0628, 0x010d, 0x0071, 0, 0, 0x000a, 0x021e, 0x000f,
            0x010d, 0x021e, 0x0127,
        ],
        &[(1, 5, 13)],
    );
    invalid.methods[0].code.as_mut().unwrap().try_regions[0].catches =
        vec![(Some("Ljava/lang/RuntimeException;".into()), 6), (None, 13)].into();
    assert!(
        render(&invalid).is_err(),
        "unprotected handler effects would leak the monitor"
    );
}

#[test]
#[ignore = "requires javac and java"]
fn typed_monitor_handler_executes_under_lock_and_releases_on_rethrow() {
    use std::process::Command;
    let mut class = fixture(
        vec![
            0x021d, 0x0071, 0, 0, 0x000a, 0x0628, 0x010d, 0x0071, 0, 0, 0x000a, 0x021e, 0x000f,
            0x010d, 0x021e, 0x0127,
        ],
        &[(1, 5, 13), (7, 11, 13)],
    );
    class.methods[0].code.as_mut().unwrap().try_regions[0].catches =
        vec![(Some("Ljava/lang/RuntimeException;".into()), 6), (None, 13)].into();
    let source = render(&class).unwrap().source;
    let dir = std::env::temp_dir().join(format!("rdx-typed-monitor-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    std::fs::write(dir.join("sample/Monitor.java"),format!(r#"package sample;
public class Monitor {{
 static Object lock=new Object();static int calls,mode;
 static int effect(){{calls++;if(!Thread.holdsLock(lock))throw new AssertionError("handler lock");
  if((mode>0&&calls==1)||mode==2)throw new IllegalStateException();return calls==1?7:42;}}
 {source}
 public static void main(String[] args){{for(mode=0;mode<3;mode++){{calls=0;
  try{{int result=test(lock);if(mode==2||result!=(mode==0?7:42))throw new AssertionError();}}
  catch(IllegalStateException e){{if(mode!=2)throw new AssertionError();}}
  if(Thread.holdsLock(lock)||calls!=(mode==0?1:2))throw new AssertionError();
  final boolean[] entered={{false}};Thread t=new Thread(()->{{synchronized(lock){{entered[0]=true;}}}});t.start();
  try{{t.join(1000);}}catch(InterruptedException e){{throw new AssertionError(e);}}
  if(!entered[0])throw new AssertionError("lock leaked");
 }}}}
}}"#)).unwrap();
    for (command, args) in [
        ("javac", vec!["sample/Monitor.java"]),
        ("java", vec!["-cp", ".", "sample.Monitor"]),
    ] {
        let result = Command::new(command)
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{command}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}
