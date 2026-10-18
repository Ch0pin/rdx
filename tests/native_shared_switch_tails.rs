//! Mutually exclusive cases may each contain the same closed effectful DEX tail.
use rdx::{
    native_dex::{self, DexClass, DexSymbols, DexTryRegion},
    native_java,
};
use std::sync::Arc;
fn fixture(allocation: bool) -> (DexClass, usize) {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class.methods.retain(|m| m.name.as_ref() == "answer");
    class.symbols = Arc::new(DexSymbols {
        strings: vec![
            "first".into(),
            "second".into(),
            "touch".into(),
            "<init>".into(),
        ],
        types: vec!["Lsample/Effects;".into(), "Lsample/Token;".into()],
        protos: vec![("V".into(), vec![]), ("Ljava/lang/Object;".into(), vec![])],
        methods: vec![(0, 0, 0), (0, 0, 1), (0, 1, 2), (1, 0, 3)],
        ..Default::default()
    });
    let mut w = vec![0x012b, 0, 0, 0x0028, 0x0071, 0, 0];
    let shared = w.len();
    if allocation {
        w.extend([0x0022, 1, 0x1070, 3, 0]);
    } else {
        w.extend([0x0071, 2, 0, 0x000c]);
    }
    let exit_goto = w.len();
    w.push(0x0028);
    let case1 = w.len();
    w.extend([0x0071, 1, 0]);
    let back = w.len();
    w.push((((shared as isize - back as isize) as i8 as u8 as u16) << 8) | 0x28);
    let exit = w.len();
    w.push(0x0011);
    if w.len() % 2 != 0 {
        w.push(0);
    }
    let payload = w.len();
    w.extend([0x0100, 2, 0, 0, 4, 0, case1 as u16, 0]);
    w[1] = payload as u16;
    w[3] = (((shared - 3) as u16) << 8) | 0x28;
    w[exit_goto] = (((exit - exit_goto) as u16) << 8) | 0x28;
    let m = &mut class.methods[0];
    m.name = if allocation { "allocated" } else { "selected" }.into();
    m.access_flags = 9;
    m.parameters = vec!["I".into()];
    m.return_type = "Ljava/lang/Object;".into();
    let c = m.code.as_mut().unwrap();
    c.registers = 2;
    c.ins = 1;
    c.outs = 1;
    c.instructions = w;
    (class, shared)
}
fn source(class: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.Hello", class, &class.methods[0])?.source)
}
#[test]
fn effectful_shared_switch_tails_are_duplicated_only_in_exclusive_cases() {
    for allocation in [false, true] {
        let (class, _) = fixture(allocation);
        let java = source(&class).unwrap();
        assert!(java.contains("switch ("), "{java}");
        assert_eq!(
            java.matches(if allocation {
                "new sample.Token("
            } else {
                "sample.Effects.touch("
            })
            .count(),
            3,
            "{java}"
        );
        assert_eq!(java.matches("sample.Effects.first(").count(), 1, "{java}");
        assert_eq!(java.matches("sample.Effects.second(").count(), 1, "{java}");
    }
}
#[test]
fn shared_switch_tail_does_not_cross_protected_or_allocation_boundaries() {
    let (mut class, shared) = fixture(false);
    class.methods[0]
        .code
        .as_mut()
        .unwrap()
        .try_regions
        .push(DexTryRegion {
            start: shared as u32,
            end: (shared + 3) as u32,
            catches: vec![(
                Some(Arc::from("Ljava/lang/RuntimeException;")),
                shared as u32,
            )]
            .into(),
        });
    let error = source(&class).unwrap_err().to_string();
    assert!(
        error.contains("shared switch tail crosses protected region")
            || error.contains("try regions not reconstructed"),
        "{error}"
    );
    let (mut class, shared) = fixture(true);
    // Removing the constructor leaves a live uninitialized allocation at the join.
    class.methods[0].code.as_mut().unwrap().instructions[shared + 2..shared + 5].fill(0);
    assert!(source(&class).is_err());
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn shared_switch_tails_jvm_preserve_effect_counts_and_exception_identity() {
    use std::{fs, process::Command};
    let dir = std::env::temp_dir().join(format!("rdx-switch-tails-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let mut java = String::from(
        r#"public class SwitchTails {
        static class Effects {
            static int firstCount,secondCount,touches,creates,fail;
            static final RuntimeException error=new RuntimeException("identity");
            static final Object value=new Object();
            static void first(){firstCount++;if(fail==1)throw error;}
            static void second(){secondCount++;if(fail==2)throw error;}
            static Object touch(){touches++;if(fail==3)throw error;return value;}
            static void reset(int f){firstCount=secondCount=touches=creates=0;fail=f;}
        }
        static class Token {Token(){Effects.creates++;Effects.touch();}}
    "#,
    );
    for allocation in [false, true] {
        java.push_str(
            &source(&fixture(allocation).0)
                .unwrap()
                .replace("sample.Effects", "Effects")
                .replace("sample.Token", "Token"),
        );
    }
    java.push_str(r#"public static void main(String[] args){
        for(int key:new int[]{Integer.MIN_VALUE,-1,0,1,2,Integer.MAX_VALUE})
        for(int failure=0;failure<4;failure++) for(boolean allocation:new boolean[]{false,true}) {
            Effects.reset(failure);
            boolean early=(key==0&&failure==1)||(key==1&&failure==2);
            boolean throwsExpected=early||failure==3;
            try {Object result=allocation?allocated(key):selected(key);
                if(throwsExpected)throw new AssertionError("missing throw");
                if(allocation ? !(result instanceof Token) : result!=Effects.value)throw new AssertionError("result");
            } catch(RuntimeException ex){if(!throwsExpected||ex!=Effects.error)throw new AssertionError("exception identity",ex);}
            if(Effects.firstCount!=(key==0?1:0)||Effects.secondCount!=(key==1?1:0)
                ||Effects.touches!=(early?0:1)||Effects.creates!=((allocation&&!early)?1:0))
                throw new AssertionError("effect count key="+key+" failure="+failure);
        }
    }}"#);
    fs::write(dir.join("SwitchTails.java"), &java).unwrap();
    for (program, arg) in [("javac", "SwitchTails.java"), ("java", "SwitchTails")] {
        let output = Command::new(program)
            .arg(arg)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{program}: {}\n{java}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn default_arm_can_bypass_an_earlier_shared_case_return() {
    let (mut class, _) = fixture(false);
    let method = &mut class.methods[0];
    method.return_type = "I".into();
    method.code.as_mut().unwrap().instructions = vec![
        0x0012, 0x012b, 13, 0, 0x0528, 0x1012, 0x0228, 0x2012, 0x000f, 0x0071, 0, 0, 0xf012,
        0x000f, 0x0100, 2, 0, 0, 4, 0, 6, 0,
    ];
    let java = source(&class).unwrap();
    assert!(java.contains("return 1;"), "{java}");
    assert!(java.contains("return 2;"), "{java}");
    assert!(java.contains("return -1;"), "{java}");
    assert_eq!(java.matches("sample.Effects.first()").count(), 1, "{java}");
}

// The shared call overwrites v0 only on success. Its handler discards the
// exception and returns the original v0 through the normal bare return.
fn protected_shared_return_fixture() -> DexClass {
    let (mut class, _) = fixture(false);
    let symbols = Arc::get_mut(&mut class.symbols).unwrap();
    symbols.protos[1] = ("I".into(), vec![]);
    let method = &mut class.methods[0];
    method.name = "protectedSelected".into();
    method.return_type = "I".into();
    let code = method.code.as_mut().unwrap();
    code.instructions = vec![
        0x7012, // 0: const/4 v0, 7 (handler snapshot)
        0x012b, 17, 0,      // 1: packed-switch v1, payload at 18
        0x0428, // 4: default -> shared call at 8
        0x0071, 0, 0, // 5: first(), outside try
        0x0071, 2, 0,      // 8: touch(), protected
        0x000a, // 11: move-result v0, in the protected call/result pair
        0x000f, // 12: normal and catch return v0, no move-exception
        0x0071, 1, 0,      // 13: second(), outside try
        0xf828, // 16: goto shared call at 8
        0x0000, // 17: alignment
        0x0100, 2, 0, 0, 4, 0, 12, 0,
    ];
    code.tries = 1;
    code.try_regions = vec![DexTryRegion {
        start: 8,
        end: 12,
        catches: vec![(Some(Arc::from("Ljava/lang/IllegalArgumentException;")), 12)].into(),
    }];
    class
}

#[test]
fn bounded_shared_switch_try_preserves_bare_handler_and_result_snapshot() {
    let java = source(&protected_shared_return_fixture()).unwrap();
    assert!(java.contains("switch ("), "{java}");
    assert_eq!(java.matches("sample.Effects.touch()").count(), 3, "{java}");
    assert_eq!(
        java.matches("catch (java.lang.IllegalArgumentException")
            .count(),
        3,
        "{java}"
    );
    assert_eq!(java.matches("sample.Effects.first()").count(), 1, "{java}");
    assert_eq!(java.matches("sample.Effects.second()").count(), 1, "{java}");
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn bounded_shared_switch_try_jvm_matches_dex_effects_results_and_fault_identity() {
    use std::{fs, process::Command};
    let dir =
        std::env::temp_dir().join(format!("rdx-protected-switch-tails-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let rendered = source(&protected_shared_return_fixture())
        .unwrap()
        .replace("sample.Effects", "Effects");
    let java = format!(
        r#"public class ProtectedSwitchTails {{
        static class Effects {{
            static int firstCount, secondCount, touches, fail;
            static final IllegalArgumentException caught = new IllegalArgumentException("caught identity");
            static final Error uncaught = new AssertionError("uncaught identity");
            static void first() {{ firstCount++; if(fail==1) throw caught; }}
            static void second() {{ secondCount++; if(fail==2) throw caught; }}
            static int touch() {{ touches++; if(fail==3) throw caught; if(fail==4) throw uncaught; return 42; }}
            static void reset(int failure) {{ firstCount=secondCount=touches=0; fail=failure; }}
        }}
        {rendered}
        // Direct execution of the fixture's DEX control flow and try interval.
        static int dex(int key) {{
            int v0=7;
            switch(key) {{ case 0: Effects.first(); break; case 1: Effects.second(); break; default: break; }}
            try {{ v0=Effects.touch(); }} catch(IllegalArgumentException ignored) {{}}
            return v0;
        }}
        static Object outcome(boolean nativeSource,int key) {{
            try {{ return Integer.valueOf(nativeSource?protectedSelected(key):dex(key)); }}
            catch(Throwable fault) {{ return fault; }}
        }}
        public static void main(String[] args) {{
            for(int key:new int[]{{Integer.MIN_VALUE,-1,0,1,2,Integer.MAX_VALUE}})
            for(int failure=0;failure<5;failure++) {{
                Effects.reset(failure);
                Object expected=outcome(false,key);
                int first=Effects.firstCount,second=Effects.secondCount,touches=Effects.touches;
                Effects.reset(failure);
                Object actual=outcome(true,key);
                if(expected instanceof Throwable ? actual!=expected : !expected.equals(actual))
                    throw new AssertionError("result or identity key="+key+" failure="+failure);
                boolean prefixFailure=(key==0&&failure==1)||(key==1&&failure==2);
                if(Effects.firstCount!=first || Effects.secondCount!=second || Effects.touches!=touches
                    ||first!=(key==0?1:0)||second!=(key==1?1:0)||touches!=(prefixFailure?0:1))
                    throw new AssertionError("effects key="+key+" failure="+failure);
                if(prefixFailure && actual!=Effects.caught) throw new AssertionError("prefix exception swallowed");
                if(!prefixFailure && failure==3 && !Integer.valueOf(7).equals(actual))
                    throw new AssertionError("handler lost entry snapshot");
                if(!prefixFailure && failure==4 && actual!=Effects.uncaught)
                    throw new AssertionError("uncaught exception identity");
            }}
        }}
    }}"#
    );
    fs::write(dir.join("ProtectedSwitchTails.java"), &java).unwrap();
    for (program, arg) in [
        ("javac", "ProtectedSwitchTails.java"),
        ("java", "ProtectedSwitchTails"),
    ] {
        let output = Command::new(program)
            .arg(arg)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{program}: {}\n{java}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn shared_switch_try_rejects_entry_into_protected_interior() {
    let mut class = protected_shared_return_fixture();
    // The second case jumps to touch() at 8, bypassing first() at 5 inside
    // the enlarged protected interval. Only the original start is admissible.
    class.methods[0].code.as_mut().unwrap().try_regions[0].start = 5;
    let error = source(&class).unwrap_err().to_string();
    assert!(
        error.contains("shared switch tail crosses protected region"),
        "{error}"
    );
}

#[test]
fn shared_switch_try_rejects_handler_beyond_closed_arm_region() {
    let mut class = protected_shared_return_fixture();
    let code = class.methods[0].code.as_mut().unwrap();
    let detached_handler = code.instructions.len() as u32;
    code.instructions.push(0x000f);
    code.try_regions[0].catches = vec![(
        Some(Arc::from("Ljava/lang/IllegalArgumentException;")),
        detached_handler,
    )]
    .into();
    let error = source(&class).unwrap_err().to_string();
    assert!(
        error.contains("shared switch tail crosses protected region"),
        "{error}"
    );
}
