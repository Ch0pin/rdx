//! Consecutive predicates share their original success body and preserve order.
pub use rdx::{native_cfg, native_dex, native_ir};
#[path = "../src/native_java/condition_chain.rs"]
mod certificate;
use native_dex::{DexClass, DexSymbols, DexTryRegion};
use std::{process::Command, sync::Arc};

fn fixture(count: usize, opcode: u8) -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class
        .methods
        .retain(|method| method.name.as_ref() == "answer");
    class.symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Effects;".into()],
        strings: ["effect", "fallback", "step", "finish"]
            .into_iter()
            .map(String::from)
            .collect(),
        protos: vec![
            ("V".into(), vec![]),
            ("V".into(), vec!["I".into()]),
            ("I".into(), vec![]),
        ],
        methods: vec![(0, 0, 0), (0, 0, 1), (0, 1, 2), (0, 2, 3)],
        ..Default::default()
    });
    let method = &mut class.methods[0];
    method.name = format!("run_{opcode:02x}").into();
    method.access_flags = 9;
    method.return_type = "I".into();
    method.parameters = vec!["I".into()];
    let code = method.code.as_mut().unwrap();
    code.registers = 5;
    code.ins = 1;
    code.outs = 1;
    code.tries = 0;
    code.try_regions.clear();
    let fallback = 3 + count * 2;
    let target = fallback + 4;
    let finish = target + 12;
    let mut words = vec![0x1012, 0x2112, 0x3212];
    for index in 0..count {
        let pc = words.len();
        let register = (index % 3) as u16;
        words.push(if opcode < 0x38 {
            (register << 12) | (4 << 8) | u16::from(opcode)
        } else {
            (register << 8) | u16::from(opcode)
        });
        words.push((target - pc) as u16);
    }
    words.extend_from_slice(&[
        0x0071,
        1,
        0,
        (((finish - (fallback + 3)) as u16) << 8) | 0x28,
        0x0071,
        0,
        0,
        0x0312, // shared effect(); i=0
        0x4335,
        8,
        0x1071,
        2,
        3, // if i>=p0 -> finish; step(i)
        0x03d8,
        0x0103,
        0xf928, // i++; goto guard
        0x0071,
        3,
        0,
        0x000a,
        0x000f,
    ]);
    code.instructions = words;
    class
}

fn source(class: &DexClass) -> anyhow::Result<String> {
    Ok(rdx::native_java::render_method("sample.Entry", class, &class.methods[0])?.source)
}

#[test]
fn all_predicate_families_keep_one_shared_effectful_success_body() {
    for opcode in 0x32..=0x3d {
        let class = fixture(3, opcode);
        let code = class.methods[0].code.as_ref().unwrap();
        let original = code.instructions.clone();
        let plan = certificate::plan(code, 3).expect("same-target condition chain");
        assert_eq!(plan.branches.len(), 3);
        assert_eq!((plan.target, plan.fallthrough), (13, 9));
        assert!(certificate::validate(code, &plan));
        assert!(certificate::matches(code, &plan));
        let text = source(&class).unwrap_or_else(|error| panic!("opcode={opcode:x}: {error:#}"));
        for call in [
            "Effects.effect(",
            "Effects.fallback(",
            "Effects.step(",
            "Effects.finish(",
        ] {
            assert_eq!(
                text.matches(call).count(),
                1,
                "opcode={opcode:x} call={call}\n{text}"
            );
        }
        assert_eq!(text.matches("while (true)").count(), 1, "{text}");
        assert_eq!(
            class.methods[0].code.as_ref().unwrap().instructions,
            original
        );
    }
}

#[test]
fn exact_maximal_chain_budget_and_common_protection_are_explicit() {
    for count in [1, 2, 8, 9] {
        let class = fixture(count, 0x32);
        let code = class.methods[0].code.as_ref().unwrap();
        let selected = certificate::plan(code, 3);
        assert_eq!(
            selected.is_some(),
            (2..=8).contains(&count),
            "count={count}"
        );
        if let Some(plan) = selected {
            assert_eq!(plan.branches.len(), count);
            assert!(certificate::validate(code, &plan));
        }
    }
    let mut class = fixture(3, 0x32);
    let code = class.methods[0].code.as_mut().unwrap();
    code.tries = 1;
    code.try_regions = vec![DexTryRegion {
        start: 3,
        end: 9,
        catches: vec![(None, 29)].into(),
    }];
    let plan = certificate::plan(code, 3).expect("identical original protection");
    assert_eq!(plan.branches.len(), 3);
    assert!(certificate::validate(code, &plan));
}

#[test]
fn interior_normal_or_exceptional_ingress_and_intervening_effect_decline() {
    for shape in 0..4 {
        let mut class = fixture(3, 0x32);
        let code = class.methods[0].code.as_mut().unwrap();
        match shape {
            0 => {
                code.instructions[0] = 0x0438;
                code.instructions[1] = 5;
            }
            1 => {
                code.tries = 1;
                code.try_regions = vec![DexTryRegion {
                    start: 0,
                    end: 1,
                    catches: vec![(None, 5)].into(),
                }];
            }
            2 => {
                code.instructions[5] = 0x001a;
                code.instructions[6] = 0;
            }
            3 => {
                code.tries = 1;
                code.try_regions = vec![DexTryRegion {
                    start: 5,
                    end: 7,
                    catches: vec![(None, 29)].into(),
                }];
            }
            _ => unreachable!(),
        }
        native_ir::DecodedMethod::decode(code).unwrap();
        native_cfg::ControlFlowGraph::build(code).unwrap();
        assert!(certificate::plan(code, 3).is_none(), "shape={shape}");
    }
}

#[test]
fn stale_predicates_targets_code_frames_and_exception_metadata_cannot_reuse_a_plan() {
    let class = fixture(3, 0x32);
    let plan = certificate::plan(class.methods[0].code.as_ref().unwrap(), 3).unwrap();
    for shape in 0..5 {
        let mut class = fixture(3, 0x32);
        let code = class.methods[0].code.as_mut().unwrap();
        match shape {
            0 => code.instructions[3] = 0x0433,
            1 => code.instructions[8] = 18,
            2 => code.instructions[0] = 0x2012,
            3 => code.registers += 1,
            4 => {
                code.tries = 1;
                code.try_regions = vec![DexTryRegion {
                    start: 0,
                    end: 1,
                    catches: vec![(None, 29)].into(),
                }];
            }
            _ => unreachable!(),
        }
        assert!(!certificate::validate(code, &plan), "shape={shape}");
        assert!(!certificate::matches(code, &plan), "shape={shape}");
    }
}

#[test]
#[ignore = "requires javac/java; all emitted methods compiled unchanged"]
fn jvm_condition_chains_preserve_results_effect_order_and_fault_identity() {
    let mut methods = String::new();
    let mut calls = String::new();
    for opcode in 0x32..=0x3d {
        methods.push_str(&source(&fixture(3, opcode)).unwrap());
        calls.push_str(&format!("case {opcode}: return run_{opcode:02x}(n);\n"));
    }
    let java = HARNESS
        .replace("METHODS", &methods)
        .replace("CASES", &calls);
    let dir = std::env::temp_dir().join(format!("rdx-condition-chain-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    let file = dir.join("sample/Entry.java");
    std::fs::write(&file, java).unwrap();
    let home = std::env::var("RDX_JAVA25_HOME").ok();
    let executable = |name: &str| {
        home.as_ref()
            .map_or_else(|| name.to_string(), |home| format!("{home}/bin/{name}"))
    };
    let compile = Command::new(executable("javac"))
        .arg(&file)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let result = Command::new(executable("java"))
        .args(["-cp", dir.to_str().unwrap(), "sample.Entry"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

const HARNESS: &str = r#"package sample;
public class Entry {
METHODS
static int actual(int opcode,int n) {switch(opcode){CASES default:throw new AssertionError();}}
static boolean selected(int opcode,int n) {
 for(int value=1;value<=3;value++) {
  boolean result;
  switch(opcode) {
   case 50:result=n==value;break;case 51:result=n!=value;break;
   case 52:result=n<value;break;case 53:result=n>=value;break;
   case 54:result=n>value;break;case 55:result=n<=value;break;
   case 56:result=value==0;break;case 57:result=value!=0;break;
   case 58:result=value<0;break;case 59:result=value>=0;break;
   case 60:result=value>0;break;case 61:result=value<=0;break;
   default:throw new AssertionError();
  }
  if(result)return true;
 }
 return false;
}
static int expected(int opcode,int n) {
 if(selected(opcode,n)){Effects.effect();for(int i=0;i<n;i++)Effects.step(i);}
 else Effects.fallback();
 return Effects.finish();
}
public static void main(String[] args) {
 int checks=0;
 for(int opcode=50;opcode<=61;opcode++)for(int n=-3;n<=8;n++)for(int fail=-1;fail<=12;fail++) {
  int[] value={-1,-1};Throwable[] fault=new Throwable[2];String[] trace=new String[2];
  for(int side=0;side<2;side++) {
   Effects.reset(fail);
   try{value[side]=side==0?actual(opcode,n):expected(opcode,n);}catch(RuntimeException caught){fault[side]=caught;}
   trace[side]=Effects.trace;
  }
  if(value[0]!=value[1] || fault[0]!=fault[1] || !trace[0].equals(trace[1]))
   throw new AssertionError(opcode+":"+n+":"+fail+":"+trace[0]+"/"+trace[1]);
  checks++;
 }
 System.out.println("checks="+checks);
}
}
class Effects {
 static final RuntimeException sentinel=new RuntimeException("identity");
 static int failure,calls,steps,route;static String trace;
 static void reset(int at){failure=at;calls=0;steps=0;route=0;trace="";}
 static void operation(String name){trace+=name+",";if(calls++==failure)throw sentinel;}
 static void effect(){operation("E");route=1;}
 static void fallback(){operation("B");route=2;}
 static void step(int index){if(index!=steps++)throw new AssertionError("counter");operation("S"+index);}
 static int finish(){operation("F");return steps*100+route;}
}
"#;
