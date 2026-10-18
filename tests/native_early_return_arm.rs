pub use rdx::{native_cfg, native_dex, native_ir};
#[path = "../src/native_java/early_return_arm.rs"]
mod proof;
use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_java,
};
use std::{fs, process::Command, sync::Arc};
fn fixture(count: usize) -> DexClass {
    let descriptor: Arc<str> = "Lsample/GuardChain;".into();
    let mut words = Vec::new();
    for index in 0..count {
        words.extend([
            0x0013,
            index as u16,
            0x2071,
            0,
            0x0030,
            0x010a,
            0x0139,
            4,
            0x0029,
            0,
        ]);
    }
    let success = words.len();
    words.extend([0x1012, 0x000f]);
    let failure = words.len();
    words.extend([0x0012, 0x000f]);
    for index in 0..count {
        let branch = index * 10 + 8;
        words[branch + 1] = (failure - branch) as u16;
    }
    assert_eq!(success, count * 10);
    DexClass {
        descriptor: descriptor.clone(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            types: vec![descriptor.clone()],
            strings: vec!["check".into()],
            protos: vec![("Z".into(), vec!["I".into(), "I".into()])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: descriptor,
            name: "run".into(),
            access_flags: 9,
            return_type: "Z".into(),
            parameters: vec!["I".into()],
            thrown_types: vec![],
            code: Some(DexCode {
                registers: 4,
                ins: 1,
                outs: 2,
                tries: 0,
                try_regions: vec![],
                offset: 0,
                instructions: words,
            }),
        }],
    }
}
fn source(c: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.GuardChain", c, &c.methods[0])?.source)
}
fn taken_fixture() -> DexClass {
    let mut c = fixture(1);
    c.symbols = Arc::new(DexSymbols {
        types: c.symbols.types.clone(),
        strings: vec!["touch".into()],
        protos: vec![("V".into(), vec!["I".into()])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let m = &mut c.methods[0];
    m.name = "direct".into();
    m.return_type = "I".into();
    m.code.as_mut().unwrap().instructions = vec![
        0x0339, 7, // nonzero -> terminal return-one arm
        0x1071, 0, 3, // zero -> effectful continuation
        0x0012, 0x000f, 0x1012, 0x000f,
    ];
    c
}
#[test]
fn taken_terminal_arm_keeps_its_effectful_fallthrough_continuation() {
    let c = taken_fixture();
    let code = c.methods[0].code.as_ref().unwrap();
    let plan = proof::select(code, 0, code.instructions.len()).unwrap();
    assert!(plan.taken);
    assert_eq!(plan.entry, 7);
    assert_eq!(plan.continuation, 2);
    source(&c).unwrap();
}
#[test]
fn long_guard_chains_continue_at_the_current_depth_after_pure_return_arms() {
    for count in [1, 2, 3, 4, 5, 16, 24, 25, 40, 80] {
        let c = fixture(count);
        let code = c.methods[0].code.as_ref().unwrap();
        let p = proof::select(code, 6, code.instructions.len()).unwrap();
        assert_eq!(p.continuation, 10);
        assert!(!p.taken);
        assert_eq!(p.instructions.len(), 3);
        assert_eq!(
            proof::sequence(code, 6, code.instructions.len())
                .as_ref()
                .map(Vec::len),
            (count >= proof::MIN_GUARDS).then_some(count),
        );
        let text = source(&c).unwrap();
        assert_eq!(text.matches("GuardChain.check(").count(), count, "{text}");
        assert!(
            text.lines()
                .map(|line| line.len() - line.trim_start().len())
                .max()
                .unwrap_or(0)
                <= 16,
            "{text}"
        );
    }
}
#[test]
fn three_four_five_and_eighty_guards_render_on_an_explicit_two_mib_thread() {
    std::thread::Builder::new()
        .name("early-return-2mib".into())
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            for count in [3, 4, 5, 80] {
                let text = source(&fixture(count)).unwrap();
                assert_eq!(text.matches("GuardChain.check(").count(), count);
                assert!(
                    text.lines()
                        .map(|line| line.len() - line.trim_start().len())
                        .max()
                        .unwrap_or(0)
                        <= 16,
                    "{text}"
                );
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
#[test]
fn sequence_requires_original_forward_prefix_edges_and_preserves_short_tail() {
    let c = fixture(5);
    let code = c.methods[0].code.as_ref().unwrap();
    let ir = native_ir::DecodedMethod::decode(code).unwrap();
    let cfg = native_cfg::ControlFlowGraph::build(code).unwrap();
    let plans = proof::sequence_decoded(&ir, &cfg, 6, code.instructions.len()).unwrap();
    assert_eq!(
        plans.iter().map(|p| p.branch).collect::<Vec<_>>(),
        [6, 16, 26, 36, 46]
    );
    for shape in 0..3 {
        let mut broken = cfg.clone();
        let b = broken.block_at[&10];
        match shape {
            0 => broken.blocks[b].successors.clear(),
            1 => {
                let edge = broken.blocks[b].successors[0];
                broken.blocks[b].successors.push(edge);
            }
            _ => broken.blocks[b].successors[0].target = broken.block_at[&0],
        }
        assert!(
            proof::sequence_decoded(&ir, &broken, 6, code.instructions.len()).is_none(),
            "shape {shape}"
        );
    }
}
#[test]
fn effectful_arms_cycles_protection_and_noninstruction_targets_decline() {
    let base = DexCode {
        registers: 2,
        ins: 1,
        outs: 0,
        tries: 0,
        try_regions: vec![],
        offset: 0,
        instructions: vec![0x0138, 6, 0x0071, 1, 0, 0x000e, 0x0071, 1, 0, 0x000e],
    };
    assert!(proof::select(&base, 0, 10).is_none());
    let mut cycle = base;
    cycle.instructions[2] = 0x0028;
    assert!(proof::select(&cycle, 0, 10).is_none());
    let mut protected = fixture(1);
    protected.methods[0].code.as_mut().unwrap().tries = 1;
    let code = protected.methods[0].code.as_ref().unwrap();
    assert!(proof::select(code, 6, code.instructions.len()).is_none());
    let mut malformed = fixture(1);
    let code = malformed.methods[0].code.as_mut().unwrap();
    code.instructions[7] = 1;
    assert!(proof::select(code, 6, code.instructions.len()).is_none());
}
#[test]
fn terminal_copy_retains_return_type_and_initialized_value_checks() {
    let mut c = fixture(40);
    c.methods[0].return_type = "Ljava/lang/Object;".into();
    let words = &mut c.methods[0].code.as_mut().unwrap().instructions;
    for word in words {
        if *word == 0x000f {
            *word = 0x0011;
        }
    }
    assert!(source(&c).is_err());
}
#[test]
#[ignore = "requires javac/java; root runs serially"]
fn jvm_eighty_guard_short_circuit_keeps_values_effect_order_and_exception_identity() {
    let mut text = source(&fixture(80)).unwrap();
    text.push_str(&source(&taken_fixture()).unwrap());
    let dir = std::env::temp_dir().join(format!("rdx-early-return-chain-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let file = dir.join("sample/GuardChain.java");
    fs::write(&file,format!(r#"package sample;public class GuardChain{{
{text}
static String trace;static int throwAt;static final RuntimeException sentinel=new RuntimeException();
static boolean check(int index,int missing){{trace+=index+",";if(index==throwAt)throw sentinel;return index!=missing;}}
static void touch(int value){{trace+="T"+value+",";}}
public static void main(String[] args){{
 for(int n=-2;n<=2;n++){{trace="";if(direct(n)!=(n!=0?1:0)||!trace.equals(n==0?"T0,":""))throw new AssertionError("taken "+n+":"+trace);}}
 for(int missing:new int[]{{-1,0,1,17,39,40,79,80,81}}){{trace="";throwAt=-1;
 boolean result=run(missing);int count=missing>=0&&missing<80?missing+1:80;String expected="";for(int i=0;i<count;i++)expected+=i+",";
 if(result!=(missing<0||missing>=80)||!trace.equals(expected))throw new AssertionError(missing+":"+trace);
 }}
 for(int at:new int[]{{0,17,79}}){{trace="";throwAt=at;try{{run(-1);throw new AssertionError();}}catch(RuntimeException e){{if(e!=sentinel)throw new AssertionError(e);}}String expected="";for(int i=0;i<=at;i++)expected+=i+",";if(!trace.equals(expected))throw new AssertionError(trace);}}
}}
}}"#)).unwrap();
    let c = Command::new("javac").arg(&file).output().unwrap();
    assert!(c.status.success(), "{}", String::from_utf8_lossy(&c.stderr));
    let r = Command::new("java")
        .args(["-cp", dir.to_str().unwrap(), "sample.GuardChain"])
        .output()
        .unwrap();
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
}
