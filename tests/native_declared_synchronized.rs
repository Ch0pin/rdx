use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_java,
};
use std::sync::Arc;

fn fixture(static_method: bool, nested: bool) -> DexClass {
    let (words, ranges) = if nested {
        (
            vec![
                0x021d, 0x021d, 0x2071, 0, 0x0032, 0x000a, 0x021e, 0x021e, 0x000f, 0x010d, 0x021e,
                0x0127, 0x010d, 0x021e, 0x0127,
            ],
            vec![(1, 2, 12), (2, 6, 9), (9, 12, 12)],
        )
    } else if static_method {
        (
            vec![
                0x021c, 0, 0x021d, 0x2071, 0, 0x0032, 0x000a, 0x021e, 0x000f, 0x010d, 0x021e,
                0x0127,
            ],
            vec![(3, 8, 9), (10, 11, 9)],
        )
    } else {
        (
            vec![
                0x021d, 0x2071, 0, 0x0032, 0x000a, 0x021e, 0x000f, 0x010d, 0x021e, 0x0127,
            ],
            vec![(1, 6, 7), (8, 9, 7)],
        )
    };
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
            types: vec!["Lsample/Monitor;".into(), "Lsample/Effect;".into()],
            strings: vec!["effect".into()],
            protos: vec![("I".into(), vec!["Ljava/lang/Object;".into(), "Z".into()])],
            methods: vec![(1, 0, 0)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/Monitor;".into(),
            name: "test".into(),
            return_type: "I".into(),
            parameters: vec!["Z".into()],
            thrown_types: vec![],
            access_flags: 0x20011 | if static_method { 8 } else { 0 },
            code: Some(DexCode {
                registers: 4,
                ins: if static_method { 1 } else { 2 },
                outs: 2,
                tries: ranges.len() as u16,
                instructions: words,
                offset: 0,
                try_regions: ranges
                    .into_iter()
                    .map(|(start, end, handler)| DexTryRegion {
                        start,
                        end,
                        catches: vec![(None, handler)].into(),
                    })
                    .collect(),
            }),
        }],
    }
}

fn source(class: &DexClass) -> anyhow::Result<String> {
    native_java::render_method("sample.Monitor", class, &class.methods[0]).map(|code| code.source)
}

fn with_unprotected_prefix(mut class: DexClass, prefix: &[u16]) -> DexClass {
    let code = class.methods[0].code.as_mut().unwrap();
    code.instructions.splice(1..1, prefix.iter().copied());
    for region in &mut code.try_regions {
        region.start += prefix.len() as u32;
        region.end += prefix.len() as u32;
        Arc::make_mut(&mut region.catches)[0].1 += prefix.len() as u32;
    }
    class
}

#[test]
fn declared_modifier_supplies_only_the_proven_outer_lock() {
    for (static_method, nested, explicit) in [(false, false, 0), (true, false, 0), (false, true, 1)]
    {
        let class = fixture(static_method, nested);
        let code = native_java::render_method("sample.Monitor", &class, &class.methods[0]).unwrap();
        assert!(
            code.source.contains("synchronized int test(boolean p0)"),
            "{}",
            code.source
        );
        assert_eq!(
            code.source.matches("synchronized (").count(),
            explicit,
            "{}",
            code.source
        );
        assert_eq!(code.source.matches("Effect.effect(").count(), 1);
        assert_eq!(
            code.links
                .iter()
                .filter(|link| link.label == "sample.Effect.effect(Ljava/lang/Object;Z)I")
                .count(),
            1
        );
    }
    let mut explicit = fixture(false, false);
    explicit.methods[0].access_flags &= !0x20000;
    assert_eq!(
        source(&explicit).unwrap().matches("synchronized (").count(),
        1
    );
    let mut copied = fixture(true, false);
    let code = copied.methods[0].code.as_mut().unwrap();
    code.instructions.insert(2, 0x2107); // exact declaring-class alias v2 -> v1
    for (pc, word) in [
        (3, 0x011d),
        (6, 0x0031),
        (8, 0x011e),
        (10, 0x000d),
        (11, 0x011e),
        (12, 0x0027),
    ] {
        code.instructions[pc] = word;
    }
    for region in &mut code.try_regions {
        region.start += 1;
        region.end += 1;
        Arc::make_mut(&mut region.catches)[0].1 += 1;
    }
    assert_eq!(
        source(&copied).unwrap().matches("synchronized (").count(),
        0
    );
}

#[test]
fn effects_outside_scope_wrong_lock_or_missing_monitor_reject() {
    let mut prefix = fixture(false, false);
    let code = prefix.methods[0].code.as_mut().unwrap();
    code.instructions.insert(0, 0x0012);
    for region in &mut code.try_regions {
        region.start += 1;
        region.end += 1;
        Arc::make_mut(&mut region.catches)[0].1 += 1;
    }
    assert!(
        source(&prefix)
            .unwrap_err()
            .to_string()
            .contains("whole-method monitor")
    );
    let mut suffix = fixture(false, false);
    let code = suffix.methods[0].code.as_mut().unwrap();
    code.instructions[6] = 0x0012;
    code.instructions.insert(7, 0x000f);
    for region in &mut code.try_regions {
        if region.start > 6 {
            region.start += 1;
            region.end += 1;
        }
        Arc::make_mut(&mut region.catches)[0].1 += 1;
    }
    assert!(
        source(&suffix)
            .unwrap_err()
            .to_string()
            .contains("effects follow declared")
    );
    let mut wrong_owner = fixture(true, false);
    wrong_owner.methods[0].code.as_mut().unwrap().instructions[1] = 1;
    assert!(source(&wrong_owner).is_err());
    let mut duplicate_literal = fixture(true, false);
    let code = duplicate_literal.methods[0].code.as_mut().unwrap();
    code.instructions.splice(2..2, [0x021c, 0]);
    for region in &mut code.try_regions {
        region.start += 2;
        region.end += 2;
        Arc::make_mut(&mut region.catches)[0].1 += 2;
    }
    assert!(source(&duplicate_literal).is_err());
    let mut missing = fixture(false, false);
    let code = missing.methods[0].code.as_mut().unwrap();
    code.instructions = vec![0x0012, 0x000f];
    code.tries = 0;
    code.try_regions.clear();
    assert!(source(&missing).is_err());
    let mut no_release = fixture(false, false);
    no_release.methods[0].code.as_mut().unwrap().instructions[5] = 0x000f;
    assert!(source(&no_release).is_err());
}

#[test]
fn pure_prefix_under_lock_may_precede_protection_but_throwing_prefix_may_not() {
    let pure = with_unprotected_prefix(fixture(false, false), &[0x0012]);
    rdx::native_method::MethodAnalysis::build(&pure, &pure.methods[0]).unwrap();
    assert!(!source(&pure).unwrap().contains("synchronized ("));
    // The unprotected call is not harmless even though its result is overwritten.
    for prefix in [
        vec![0x2071, 0, 0x0032],
        vec![0x001a, 0],
        vec![0x001c, 1],
        vec![0x0022, 1],
    ] {
        let throwing = with_unprotected_prefix(fixture(false, false), &prefix);
        rdx::native_method::MethodAnalysis::build(&throwing, &throwing.methods[0]).unwrap();
        assert!(
            source(&throwing)
                .unwrap_err()
                .to_string()
                .contains("throwing monitor body")
        );
    }
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn jvm_preserves_reflection_depth_nested_acquisition_and_throw_identity() {
    use std::{fs, process::Command};
    let dir = std::env::temp_dir().join(format!("rdx-declared-monitor-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    for (static_method, nested, prefix, depth) in [
        (false, false, false, 1),
        (true, false, false, 1),
        (false, true, false, 2),
        (false, false, true, 1),
    ] {
        let class = fixture(static_method, nested);
        let class = if prefix {
            with_unprotected_prefix(class, &[0x0012])
        } else {
            class
        };
        let method = source(&class).unwrap();
        let java = r#"
package sample;
import java.lang.management.*;
import java.lang.reflect.*;
class Effect {
 static final RuntimeException marker=new RuntimeException(); static int depth, calls;
 static int effect(Object lock,boolean fail){
  if(!Thread.holdsLock(lock))throw new AssertionError("not acquired");
  depth=0; calls++;
  for(MonitorInfo m:ManagementFactory.getThreadMXBean().getThreadInfo(new long[]{Thread.currentThread().threadId()},true,true)[0].getLockedMonitors())
   if(m.getIdentityHashCode()==System.identityHashCode(lock))depth++;
  if(fail)throw marker; return 41;
 }
}
class Monitor { METHOD }
public class Harness {
 public static void main(String[] args)throws Exception{
  Monitor instance=new Monitor(); Object lock=STATIC?Monitor.class:instance;
  Method method=Monitor.class.getDeclaredMethod("test",boolean.class);
  if(!Modifier.isSynchronized(method.getModifiers()))throw new AssertionError("reflection");
  for(boolean fail:new boolean[]{false,true}){
   Effect.calls=0;
   try{if(instance.test(fail)!=41||fail)throw new AssertionError("return");}
   catch(RuntimeException e){if(!fail||e!=Effect.marker)throw new AssertionError("throw identity");}
   if(Effect.depth!=DEPTH||Effect.calls!=1||Thread.holdsLock(lock))throw new AssertionError("depth/release: "+Effect.depth);
   final boolean[] entered={false}; Thread thread=new Thread(()->{synchronized(lock){entered[0]=true;}});
   thread.start(); thread.join(1000); if(!entered[0])throw new AssertionError("lock leaked");
  }
  System.out.print("verified");
 }
}
"#.replace("METHOD", &method).replace("STATIC", &static_method.to_string()).replace("DEPTH", &depth.to_string());
        fs::write(dir.join("sample/Harness.java"), java).unwrap();
        let compiled = Command::new("javac")
            .arg(dir.join("sample/Harness.java"))
            .output()
            .unwrap();
        assert!(
            compiled.status.success(),
            "{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        let run = Command::new("java")
            .args(["-cp"])
            .arg(&dir)
            .arg("sample.Harness")
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "{}",
            String::from_utf8_lossy(&run.stderr)
        );
        assert_eq!(run.stdout, b"verified");
    }
}
