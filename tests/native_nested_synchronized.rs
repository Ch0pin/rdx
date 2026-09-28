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
fn render(class: &DexClass) -> anyhow::Result<rdx::engine::DecompiledCode> {
    native_java::render_method("sample.Monitor", class, &class.methods[0])
}
#[test]
fn nested_monitor_retains_two_acquisitions_and_exact_cleanup() {
    let mut class = fixture(
        vec![
            0x021d, 0x021d, 0x0071, 0, 0, 0x000a, 0x021e, 0x021e, 0x000f, 0x010d, 0x021e, 0x0127,
            0x010d, 0x021e, 0x0127,
        ],
        &[(1, 2, 12), (2, 7, 9), (7, 8, 12), (10, 11, 9), (11, 14, 12)],
    );
    let source = render(&class).unwrap().source;
    assert_eq!(source.matches("synchronized (p0)").count(), 2, "{source}");
    assert_eq!(source.matches(".effect()").count(), 1, "{source}");
    class.methods[0].code.as_mut().unwrap().try_regions.pop();
    assert!(
        render(&class).is_err(),
        "unprotected inner rethrow accepted"
    );
}
#[test]
fn nested_monitor_converging_releases_preserve_outer_ownership() {
    let class = fixture(
        vec![
            0x1012, 0x021d, 0x021d, 0x1012, 0x0038, 4, 0x021e, 0x0228, 0x021e, 0x021e, 0x000f,
            0x010d, 0x021e, 0x0127, 0x010d, 0x021e, 0x0127,
        ],
        &[
            (2, 3, 14),
            (3, 9, 11),
            (9, 10, 14),
            (12, 13, 11),
            (13, 16, 14),
        ],
    );
    let source = render(&class).unwrap().source;
    assert_eq!(source.matches("synchronized (p0)").count(), 2, "{source}");
}
fn split_fixture() -> DexClass {
    let mut class = fixture(
        vec![
            0x021d, 0x0338, 9, 0x021e, 0x0071, 0, 0, 0x000a, 0x0029, 9, 0x021d, 0x0071, 0, 0,
            0x000a, 0x021e, 0x021e, 0x000f, 0x010d, 0x021e, 0x0127, 0x010d, 0x021e, 0x0127,
        ],
        &[
            (1, 4, 21),
            (10, 11, 21),
            (11, 16, 18),
            (16, 17, 21),
            (19, 20, 18),
            (20, 23, 21),
        ],
    );
    let method = &mut class.methods[0];
    method.parameters.push("Z".into());
    method.code.as_mut().unwrap().registers = 4;
    method.code.as_mut().unwrap().ins = 2;
    class
}
#[test]
fn split_monitor_continuation_executes_after_release() {
    let source = render(&split_fixture()).unwrap().source;
    assert_eq!(source.matches("synchronized (p0)").count(), 2, "{source}");
    assert!(source.contains("break monitorBody"), "{source}");
    assert_eq!(source.matches(".effect()").count(), 2, "{source}");
}
#[test]
#[ignore = "requires javac and java"]
fn nested_and_split_monitor_java_preserves_lock_and_exception_identity() {
    let source = render(&duplicated_release_fixture()).unwrap().source;
    let directory = std::env::temp_dir().join(format!("rdx-nested-monitor-{}", std::process::id()));
    std::fs::create_dir_all(directory.join("sample")).unwrap();
    let java = format!(
        r#"package sample;
public class Monitor {{
    static Object lock;
    static boolean expectedHeld;
    static boolean fail;
    static int effects;
    static final RuntimeException FAILURE = new RuntimeException("identity");
    static int effect() {{
        if (Thread.holdsLock(lock) != expectedHeld) throw new AssertionError("release timing");
        effects++;
        if(fail) throw FAILURE;
        return expectedHeld ? 17 : 23;
    }}
    {source}
    public static void main(String[] args) throws Exception {{
        for (boolean early : new boolean[] {{false,true}}) {{
            lock = new Object(); expectedHeld = !early; fail = false; effects = 0;
            int result = test(lock, early);
            if (result != (early ? 23 : 17) || effects != 1 || Thread.holdsLock(lock)) throw new AssertionError("normal");
            fail = true;
            try {{ test(lock,early); throw new AssertionError("missing throw"); }}
            catch (RuntimeException e) {{ if(e != FAILURE) throw new AssertionError("identity"); }}
            if(Thread.holdsLock(lock)) throw new AssertionError("leaked lock");
            final boolean[] acquired = {{false}};
            Thread thread = new Thread(() -> {{ synchronized(lock) {{ acquired[0] = true; }} }});
            thread.start(); thread.join(2000);
            if(!acquired[0]) throw new AssertionError("other thread blocked");
        }}
        effects = 0;
        try {{ test(null,false); throw new AssertionError("missing null failure"); }}
        catch(NullPointerException expected) {{ if(effects != 0) throw new AssertionError("effect before null failure"); }}
        System.out.println("nested monitor semantics verified");
    }}
}}"#
    );
    std::fs::write(directory.join("sample/Monitor.java"), java).unwrap();
    let compiled = std::process::Command::new("javac")
        .arg(directory.join("sample/Monitor.java"))
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}\n{source}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let run = std::process::Command::new("java")
        .arg("-cp")
        .arg(&directory)
        .arg("sample.Monitor")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}\n{source}",
        String::from_utf8_lossy(&run.stderr)
    );
}
#[test]
fn nested_monitors_reject_overwritten_outer_lock_in_body_and_cleanup() {
    for cleanup_write in [false, true] {
        let mut class = fixture(
            vec![
                0x011d,
                0x021d,
                if cleanup_write { 0x0012 } else { 0x0112 },
                0x021e,
                0x011e,
                0x000f,
                if cleanup_write { 0x010d } else { 0x000d },
                0x021e,
                if cleanup_write { 0x0127 } else { 0x0027 },
                0x000d,
                0x011e,
                0x0027,
            ],
            &[(1, 2, 9), (2, 4, 6), (4, 5, 9), (7, 8, 6), (8, 11, 9)],
        );
        class.methods[0]
            .parameters
            .push("Ljava/lang/Object;".into());
        class.methods[0].code.as_mut().unwrap().ins = 2;
        let error = render(&class).unwrap_err().to_string();
        assert!(error.contains("outer lock"), "{error}");
    }
}
#[test]
fn outside_monitor_continuation_cannot_branch_into_protected_body() {
    let mut class = split_fixture();
    let words = &mut class.methods[0].code.as_mut().unwrap().instructions;
    words[4] = 0x0038;
    words[5] = 6;
    words[6] = 0;
    assert!(render(&class).is_err(), "outside tail branch accepted");
}

#[test]
fn leading_typed_try_remains_separate_from_nested_monitor_regions() {
    let mut class = split_fixture();
    let code = class.methods[0].code.as_mut().unwrap();
    let mut instructions = vec![0x0071, 0, 0];
    instructions.extend_from_slice(&code.instructions);
    let handler = instructions.len() as u32;
    instructions.extend_from_slice(&[0x010d, 0xf012, 0x000f]);
    code.instructions = instructions;
    for region in &mut code.try_regions {
        region.start += 3;
        region.end += 3;
        region.catches = region
            .catches
            .iter()
            .map(|(ty, pc)| (ty.clone(), pc + 3))
            .collect::<Vec<_>>()
            .into();
    }
    code.try_regions.insert(
        0,
        DexTryRegion {
            start: 0,
            end: 3,
            catches: vec![(Some("Ljava/lang/RuntimeException;".into()), handler)].into(),
        },
    );
    code.tries += 1;
    let source = render(&class).unwrap().source;
    assert!(
        source.find("catch (").unwrap() < source.find("synchronized (").unwrap(),
        "{source}"
    );
    assert_eq!(source.matches("synchronized (").count(), 2, "{source}");
    // The same catch is not independent if its handler enters a monitor.
    class.methods[0].code.as_mut().unwrap().try_regions[0].catches =
        vec![(Some("Ljava/lang/RuntimeException;".into()), 3)].into();
    assert!(render(&class).is_err());
}
fn duplicated_release_fixture() -> DexClass {
    let mut class = fixture(
        vec![
            0x021d, 0x0338, 11, 0x1012, 0x0038, 8, 0x021e, 0x0071, 0, 0, 0x000a, 0x0828, 0x021d,
            0x0071, 0, 0, 0x000a, 0x021e, 0x021e, 0x000f, 0x010d, 0x021e, 0x0127, 0x010d, 0x021e,
            0x0127,
        ],
        &[
            (1, 7, 23),
            (12, 13, 23),
            (13, 18, 20),
            (18, 19, 23),
            (21, 22, 20),
            (22, 25, 23),
        ],
    );
    class.methods[0].parameters.push("Z".into());
    class.methods[0].code.as_mut().unwrap().registers = 4;
    class.methods[0].code.as_mut().unwrap().ins = 2;
    class
}
#[test]
fn duplicated_release_merges_live_results_and_ignores_dead_temporaries() {
    let source = render(&duplicated_release_fixture()).unwrap().source;
    assert!(source.contains("break monitorBody"), "{source}");
    assert!(!source.contains("<monitor-merge>"), "{source}");
}
