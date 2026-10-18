//! G06: late switch iterations, nested loops, terminal guards and exact inputs.
pub use rdx::{native_cfg, native_dex, native_dominators, native_ir, native_ssa};
use rdx::{native_dex::DexSymbols, native_java};
use std::sync::Arc;
#[path = "../src/native_java/nested_switch_continuation.rs"]
mod proof;
#[path = "../src/native_java/switch_continuation_tail.rs"]
mod switch_continuation_tail;

fn fixture() -> native_dex::DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class.descriptor = "Lsample/NestedSwitch;".into();
    class.methods.retain(|m| m.name.as_ref() == "answer");
    class.symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Effects;".into()],
        strings: vec!["step".into(), "failure".into()],
        protos: vec![
            ("V".into(), vec!["I".into()]),
            ("Ljava/lang/RuntimeException;".into(), vec![]),
        ],
        methods: vec![(0, 0, 0), (0, 1, 1)],
        ..Default::default()
    });
    let m = &mut class.methods[0];
    m.declaring_type = class.descriptor.clone();
    m.name = "run".into();
    m.access_flags = 9;
    m.parameters = vec!["I".into(), "I".into()];
    m.return_type = "I".into();
    let code = m.code.as_mut().unwrap();
    code.registers = 6;
    code.ins = 2;
    code.outs = 1;
    code.instructions = vec![
        0x0012, 0x0112, 0x053a, 33, 0x4135, 30, 0x0212, 0x03dd, 0x0101, 0x032b, 31, 0, 0x0328,
        0x00d8, 0x0100, 0x01d8, 0x0101, 0xf128, 0x0212, 0x5235, 7, 0x00d8, 0x0100, 0x02d8, 0x0102,
        0xfa28, 0x1071, 0, 0, 0xf028, 0x1071, 0, 1, 0xee28, 0x000f, 0x0071, 1, 0, 0x030c, 0x0327,
        0x0100, 2, 0, 0, 9, 0, 21, 0,
    ];
    class
}

#[test]
fn nested_switch_certificate_covers_late_loop_and_preserves_terminal_prefix() {
    let class = fixture();
    let code = class.methods[0].code.as_ref().unwrap();
    assert!(
        switch_continuation_tail::select(code).is_none(),
        "nested cycle needs its own proof"
    );
    let plans = proof::select_all(code);
    assert_eq!(plans.len(), 1);
    let plan = &plans[0];
    assert_eq!(
        (plan.header, plan.guard, plan.latch, plan.exit, plan.switch),
        (2, 4, 17, 34, 9)
    );
    assert!(plan.owned.contains(&19) && plan.owned.contains(&30));
    assert!(plan.edges.contains(&(29, 13)));
    let rendered =
        native_java::render_method("sample.NestedSwitch", &class, &class.methods[0]).unwrap();
    assert!(
        rendered.source.contains("sample.Effects.failure()"),
        "{}",
        rendered.source
    );
    assert!(rendered.source.contains("switch ("), "{}", rendered.source);
}

#[test]
fn nested_switch_certificate_rejects_irreducible_entry_second_exit_and_protection() {
    let mut class = fixture();
    class.methods[0].code.as_mut().unwrap().instructions[12] = 0x0928; // enter nested body21 directly
    assert!(proof::select_all(class.methods[0].code.as_ref().unwrap()).is_empty());
    let mut class = fixture();
    class.methods[0].code.as_mut().unwrap().instructions[33] = 0x0128; // alternate normal exit frame
    assert!(proof::select_all(class.methods[0].code.as_ref().unwrap()).is_empty());
    let mut class = fixture();
    let code = class.methods[0].code.as_mut().unwrap();
    code.tries = 1;
    code.try_regions.push(native_dex::DexTryRegion {
        start: 35,
        end: 38,
        catches: vec![(None, 39)].into(),
    });
    assert!(proof::select_all(code).is_empty());
}

#[test]
fn nested_switch_certificate_proves_outlined_terminal_inputs_and_exact_identity() {
    let mut class = fixture();
    let code = class.methods[0].code.as_ref().unwrap();
    let identity = proof::CodeIdentity::new(code);
    assert!(identity.matches(code));
    Arc::get_mut(&mut class.symbols).unwrap().protos[1]
        .1
        .push("I".into());
    let code = class.methods[0].code.as_mut().unwrap();
    code.instructions[35] = 0x1071;
    code.instructions[37] = 2; // v2 is undefined on the header's negative-mode error path
    assert!(!identity.matches(code));
    assert!(proof::select_all(code).is_empty());
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn nested_switch_continuation_jvm_preserves_effect_trace_and_exception_identity() {
    let class = fixture();
    let method =
        native_java::render_method("sample.NestedSwitch", &class, &class.methods[0]).unwrap();
    let source = format!(
        r#"public class NestedSwitch {{
        static class Effects {{static String trace;static boolean fail;static RuntimeException error=new RuntimeException();
        static void step(int value){{trace+="/"+value;if(fail)throw error;}}
        static RuntimeException failure(){{trace+="/failure";return error;}}}}
        {}
        public static void main(String[]args){{
            for(int count:new int[]{{-1,0,1,2,5}})for(int limit:new int[]{{-1,0,1,3}})for(boolean fail:new boolean[]{{false,true}}){{
                String expected="";int result=0;boolean abrupt=limit<0;
                if(abrupt)expected="/failure";
                else for(int outer=0;outer<count;outer++){{
                    if((outer&1)==0){{result+=Math.max(0,limit);expected+="/"+result;if(fail){{abrupt=true;break;}}result++;}}
                    else{{expected+="/"+outer;if(fail){{abrupt=true;break;}}}}
                }}
                Effects.trace="";Effects.fail=fail;
                try{{int actual=run(count,limit);if(abrupt||actual!=result)throw new AssertionError("result "+actual+" expected "+result);}}
                catch(RuntimeException error){{if(!abrupt||error!=Effects.error)throw new AssertionError("identity",error);}}
                if(!Effects.trace.equals(expected))throw new AssertionError("trace "+Effects.trace+" expected "+expected);
            }}
        }}
    }}"#,
        method.source.replace("sample.Effects", "Effects")
    );
    let dir = std::env::temp_dir().join(format!("rdx-nested-switch-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("NestedSwitch.java"), &source).unwrap();
    for (program, arg) in [("javac", "NestedSwitch.java"), ("java", "NestedSwitch")] {
        let result = std::process::Command::new(program)
            .arg(arg)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{program}: {}\n{source}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}
