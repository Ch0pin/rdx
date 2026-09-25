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
