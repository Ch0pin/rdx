use rdx::{
    native_cfg,
    native_dex::{self, DexClass, DexSymbols, DexTryRegion},
    native_ir, native_java, native_ssa,
};
use std::{fs, process::Command, sync::Arc};
#[path = "../src/native_java/detached_loop_handler.rs"]
mod detached_loop_handler;
fn fixture(kind: usize) -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class
        .methods
        .retain(|method| method.name.as_ref() == "answer");
    class.descriptor = "Lsample/HandlerTail;".into();
    class.symbols = Arc::new(DexSymbols {
        types: vec![
            "Lsample/HandlerTail;".into(),
            "Ljava/lang/Throwable;".into(),
        ],
        strings: vec!["work".into(), "getCause".into()],
        protos: vec![
            ("V".into(), vec!["I".into()]),
            ("Ljava/lang/Throwable;".into(), vec![]),
        ],
        methods: vec![(0, 0, 0), (1, 1, 1)],
        ..Default::default()
    });
    let method = &mut class.methods[0];
    method.declaring_type = class.descriptor.clone();
    method.name = ["original", "parameterReceiver", "updatedReceiverSnapshot"][kind].into();
    method.access_flags = 9;
    method.return_type = "V".into();
    method.parameters = vec!["I".into(), "Ljava/lang/Throwable;".into()];
    if kind == 2 {
        method.parameters.push("Ljava/lang/Throwable;".into());
    }
    method.thrown_types = vec!["Ljava/lang/Throwable;".into()];
    let code = method.code.as_mut().unwrap();
    code.registers = if kind == 2 { 5 } else { 4 };
    code.ins = if kind == 2 { 3 } else { 2 };
    code.outs = 1;
    code.tries = 1;
    code.instructions = if kind == 2 {
        vec![
            0x0012, 0x2035, 9, 0x4307, 0x1071, 0, 0, 0x00d8, 0x0100, 0xf828, 0x000e, 0x010d,
            0x106e, 1, 3, 0x000c, 0x0038, 3, 0x0027, 0x0127,
        ]
    } else {
        vec![
            0x0012,
            0x2035,
            8,
            0x1071,
            0,
            0,
            0x00d8,
            0x0100,
            0xf928,
            0x000e,
            0x010d,
            0x106e,
            1,
            if kind == 0 { 1 } else { 3 },
            0x000c,
            0x0038,
            3,
            0x0027,
            0x0127,
        ]
    };
    code.try_regions = vec![DexTryRegion {
        start: 3,
        end: if kind == 2 { 7 } else { 6 },
        catches: vec![(None, if kind == 2 { 11 } else { 10 })].into(),
    }];
    class
}

fn advanced_fixture(kind: usize) -> DexClass {
    if kind < 3 {
        return fixture(kind);
    }
    let mut class = fixture(if kind == 3 { 0 } else { 2 });
    let method = &mut class.methods[0];
    method.name = ["originalWithGap", "priorThrowSnapshot", "phiSnapshot"][kind - 3].into();
    let code = method.code.as_mut().unwrap();
    if kind == 3 {
        // Unreachable string resolution and return are physically inside the
        // handler span. Neither instruction belongs to its reachable proof.
        code.instructions.splice(18..18, [0x001a, 0, 0x000e]);
        code.instructions[16] = 6;
    } else {
        code.instructions = if kind == 4 {
            vec![
                0x0012, 0x2035, 12, 0x1071, 0, 0, 0x4307, 0x1071, 0, 0, 0x00d8, 0x0100, 0xf528,
                0x000e, 0x010d, 0x106e, 1, 3, 0x000c, 0x0038, 3, 0x0027, 0x0127,
            ]
        } else {
            method.parameters.push("Ljava/lang/Throwable;".into());
            code.registers = 6;
            code.ins = 4;
            vec![
                0x0012, 0x2035, 12, 0x4307, 0x0038, 3, 0x5307, 0x1071, 0, 0, 0x00d8, 0x0100,
                0xf528, 0x000e, 0x010d, 0x106e, 1, 3, 0x000c, 0x0038, 3, 0x0027, 0x0127,
            ]
        };
        code.try_regions[0].end = 10;
        code.try_regions[0].catches = vec![(None, 14)].into();
    }
    class
}
#[test]
fn sparse_handler_nodes_and_each_throwing_predecessor_keep_exact_snapshots() {
    for kind in 3..6 {
        let class = advanced_fixture(kind);
        let code = class.methods[0].code.as_ref().unwrap();
        rdx::native_method::MethodAnalysis::build(&class, &class.methods[0]).unwrap();
        let handler = code.try_regions[0].catches[0].1 as usize;
        let plan = detached_loop_handler::prove(code, handler, 1..handler - 1).unwrap();
        if kind == 3 {
            assert!(!plan.instructions.iter().any(|pc| (18..21).contains(pc)));
        }
        let rendered = native_java::render_method("sample.HandlerTail", &class, &class.methods[0]);
        if kind == 3 {
            assert!(
                rendered
                    .unwrap_err()
                    .to_string()
                    .contains("unreachable instruction region")
            );
        } else {
            rendered.unwrap_or_else(|error| panic!("fixture {kind}: {error:#}"));
        }
    }
    assert_eq!(
        dex_trace(&advanced_fixture(4), 2, 0, 1),
        "work:0|cause:A|throw:causeA"
    );
    assert_eq!(
        dex_trace(&advanced_fixture(4), 2, 1, 1),
        "work:0|work:0|work:1|cause:B|throw:causeB"
    );
    assert_eq!(
        dex_trace(&advanced_fixture(5), 2, 0, 1),
        "work:0|cause:B|throw:causeB"
    );
    assert_eq!(
        dex_trace(&advanced_fixture(5), 2, 1, 1),
        "work:0|work:1|cause:C|throw:causeC"
    );
}

#[test]
fn undefined_branch_phi_cannot_become_a_handler_snapshot_seed() {
    let mut class = advanced_fixture(5);
    let code = class.methods[0].code.as_mut().unwrap();
    // The conditional arm copies genuinely undefined v1, rather than C.
    code.instructions[6] = 0x1307;
    let analysis = rdx::native_method::MethodAnalysis::build(&class, &class.methods[0]).unwrap();
    let types = analysis.infer_types().unwrap();
    let id = analysis
        .ssa()
        .instructions
        .iter()
        .find(|instruction| instruction.pc == 15)
        .unwrap()
        .reads
        .iter()
        .find(|read| read.register == 3)
        .unwrap()
        .words[0];
    assert!(
        types.values[id]
            .assignment
            .contains(&rdx::native_types::AssignmentBound::Undefined)
    );
    let code = class.methods[0].code.as_ref().unwrap();
    assert!(detached_loop_handler::prove(code, 14, 1..13).is_none());
    assert!(native_java::render_method("sample.HandlerTail", &class, &class.methods[0]).is_err());
}

#[test]
fn handler_tail_proof_retains_receivers_and_original_exception_owners() {
    for kind in 0..3 {
        let class = fixture(kind);
        let code = class.methods[0].code.as_ref().unwrap();
        let plan = detached_loop_handler::prove(
            code,
            if kind == 2 { 11 } else { 10 },
            1..if kind == 2 { 10 } else { 9 },
        )
        .unwrap();
        assert_eq!(plan.instructions.len(), 6);
        assert_eq!(
            plan.live_in_registers,
            if kind == 0 { vec![] } else { vec![3] }
        );
        assert!(detached_loop_handler::validate(code, &plan));
        let mut poisoned = plan.clone();
        poisoned.raw_words[1].1[2] = if kind == 0 { 3 } else { 1 };
        assert!(!detached_loop_handler::validate(code, &poisoned));
        let mut poisoned = plan.clone();
        poisoned.normal_edges.pop();
        assert!(!detached_loop_handler::validate(code, &poisoned));
        let mut poisoned = plan.clone();
        poisoned.owners.clear();
        assert!(!detached_loop_handler::validate(code, &poisoned));
    }
}
#[test]
fn handler_external_entries_cycles_protected_ownership_and_allocations_decline() {
    for (pc, word) in [
        (9, 0x0128),
        (9, 0x0228),
        (9, 0x0528),
        (16, 0xfffb),
        (11, 0x0122),
        (11, 0x011d),
    ] {
        let mut class = fixture(0);
        let code = class.methods[0].code.as_mut().unwrap();
        code.instructions[pc] = word;
        assert!(
            detached_loop_handler::prove(code, 10, 1..9).is_none(),
            "{pc}:{word:#x}"
        );
    }
    let mut class = fixture(0);
    let code = class.methods[0].code.as_mut().unwrap();
    code.try_regions.push(DexTryRegion {
        start: 11,
        end: 14,
        catches: vec![(None, 10)].into(),
    });
    code.tries = 2;
    assert!(detached_loop_handler::prove(code, 10, 1..9).is_none());
}
#[test]
fn terminal_handler_total_nodes_are_bounded_at_32() {
    for count in [26usize, 27] {
        let mut class = fixture(0);
        let code = class.methods[0].code.as_mut().unwrap();
        code.instructions
            .splice(17..17, std::iter::repeat_n(0, count));
        code.instructions[16] = (3 + count) as u16;
        assert_eq!(
            detached_loop_handler::prove(code, 10, 1..9).is_some(),
            count == 26
        );
    }
}

#[test]
fn terminal_catch_stays_in_one_ordinary_loop_without_new_dispatch_state() {
    for kind in 0..3 {
        let class = fixture(kind);
        let source = native_java::render_method("sample.HandlerTail", &class, &class.methods[0])
            .unwrap_or_else(|error| panic!("fixture {kind}: {error:#}"))
            .source;
        assert_eq!(source.matches("while (true)").count(), 1, "{source}");
        assert!(source.contains("catch (java.lang.Throwable"), "{source}");
        assert!(!source.contains("switch ("), "{source}");
    }
}

// This interpreter follows raw loop branches, register moves, invokes and the
// original handler table. It does not mimic the reconstructed try/catch shape.
fn dex_trace(class: &DexClass, limit: i32, fail_at: i32, cause_mode: i32) -> String {
    #[derive(Clone, Copy)]
    enum Value {
        Empty,
        Int(i32),
        Ref(&'static str),
        Null,
    }
    let code = class.methods[0].code.as_ref().unwrap();
    let mut regs = vec![Value::Empty; usize::from(code.registers)];
    regs[2] = Value::Int(limit);
    regs[3] = Value::Ref("A");
    if code.registers >= 5 {
        regs[4] = Value::Ref("B");
    }
    if code.registers == 6 {
        regs[5] = Value::Ref("C");
    }
    let mut pc = 0usize;
    let mut caught = None;
    let mut pending = None;
    let mut trace = vec![];
    for _ in 0..128 {
        let at = pc;
        let word = code.instructions[pc];
        let reg = usize::from(word >> 8);
        let mut thrown = None;
        match word as u8 {
            0x12 => {
                regs[reg & 15] = Value::Int((word as i16 >> 12) as i32);
                pc += 1;
            }
            0x35 => {
                let Value::Int(left) = regs[reg & 15] else {
                    panic!("left");
                };
                let Value::Int(right) = regs[reg >> 4] else {
                    panic!("right");
                };
                pc = if left >= right {
                    (pc as isize + code.instructions[pc + 1] as i16 as isize) as usize
                } else {
                    pc + 2
                };
            }
            0x07 => {
                regs[reg & 15] = regs[reg >> 4];
                pc += 1;
            }
            0x71 => {
                let Value::Int(index) = regs[usize::from(code.instructions[pc + 2] & 15)] else {
                    panic!("work input");
                };
                trace.push(format!("work:{index}"));
                if index == fail_at {
                    thrown = Some("original");
                }
                pc += 3;
            }
            0xd8 => {
                let source = usize::from(code.instructions[pc + 1] & 255);
                let Value::Int(value) = regs[source] else {
                    panic!("add");
                };
                regs[reg] = Value::Int(value + (code.instructions[pc + 1] >> 8) as i8 as i32);
                pc += 2;
            }
            0x28 => pc = (pc as isize + (word >> 8) as i8 as isize) as usize,
            0x0d => {
                regs[reg] = Value::Ref(caught.take().unwrap());
                pc += 1;
            }
            0x6e => {
                let Value::Ref(receiver) = regs[usize::from(code.instructions[pc + 2] & 15)] else {
                    panic!("cause receiver");
                };
                trace.push(format!("cause:{receiver}"));
                if cause_mode == 2 {
                    thrown = Some("causeFault");
                } else {
                    pending = Some(if cause_mode == 0 {
                        Value::Null
                    } else {
                        Value::Ref(match receiver {
                            "original" => "causeOriginal",
                            "A" => "causeA",
                            "B" => "causeB",
                            "C" => "causeC",
                            _ => panic!("receiver identity"),
                        })
                    });
                }
                pc += 3;
            }
            0x0c => {
                regs[reg] = pending.take().unwrap();
                pc += 1;
            }
            0x38 => {
                pc = if matches!(regs[reg], Value::Null | Value::Int(0)) {
                    (pc as isize + code.instructions[pc + 1] as i16 as isize) as usize
                } else {
                    pc + 2
                }
            }
            0x27 => {
                let Value::Ref(value) = regs[reg] else {
                    panic!("throw");
                };
                thrown = Some(value);
            }
            0x0e => {
                trace.push("return".into());
                return trace.join("|");
            }
            opcode => panic!("opcode {opcode:#x}"),
        }
        if let Some(value) = thrown {
            let handler = code
                .try_regions
                .iter()
                .find(|region| region.start as usize <= at && at < region.end as usize);
            if let Some(region) = handler {
                caught = Some(value);
                pending = None;
                pc = region.catches[0].1 as usize;
            } else {
                trace.push(format!("throw:{value}"));
                return trace.join("|");
            }
        }
    }
    panic!("execution budget");
}
#[test]
fn dex_oracle_distinguishes_caught_parameter_and_updated_receiver_snapshots() {
    assert_eq!(
        dex_trace(&fixture(0), 2, 1, 0),
        "work:0|work:1|cause:original|throw:original"
    );
    assert_eq!(
        dex_trace(&fixture(1), 2, 1, 1),
        "work:0|work:1|cause:A|throw:causeA"
    );
    assert_eq!(
        dex_trace(&fixture(2), 2, 1, 1),
        "work:0|work:1|cause:B|throw:causeB"
    );
}

#[test]
#[ignore = "requires integrated renderer, javac and java"]
fn unchanged_source_jvm_checks_terminal_handler_cause_and_receiver_identity() {
    let dir = std::env::temp_dir().join(format!("rdx-detached-handler-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let mut methods = String::new();
    let mut cases = String::new();
    for kind in 0..6 {
        if kind == 3 {
            continue;
        } // Existing whole-method unreachable guard remains.
        let class = advanced_fixture(kind);
        methods.push_str(
            &native_java::render_method("sample.HandlerTail", &class, &class.methods[0])
                .unwrap()
                .source,
        );
        for limit in 0..6 {
            for fail in -1..6 {
                for mode in 0..3 {
                    cases.push_str(&format!(
                        "runCase({kind},{limit},{fail},{mode},\"{}\");\n",
                        dex_trace(&class, limit, fail, mode)
                    ));
                }
            }
        }
    }
    let java = format!(
        r#"package sample;
public class HandlerTail {{
 static final java.util.List<String> trace=new java.util.ArrayList<>(); static int failAt,mode;
 static final RuntimeException causeOriginal=new RuntimeException(),causeA=new RuntimeException(),causeB=new RuntimeException(),causeC=new RuntimeException(); static final Error causeFault=new Error();
 static class Traced extends RuntimeException {{ final String name; final Throwable cause; Traced(String n,Throwable c){{name=n;cause=c;}}
  public Throwable getCause(){{trace.add("cause:"+name);if(mode==2)throw causeFault;return mode==0?null:cause;}} }}
 static final Traced originalFault=new Traced("original",causeOriginal),a=new Traced("A",causeA),b=new Traced("B",causeB),c=new Traced("C",causeC);
 static String id(Throwable value){{if(value==originalFault)return "original";if(value==causeOriginal)return "causeOriginal";if(value==causeA)return "causeA";if(value==causeB)return "causeB";if(value==causeC)return "causeC";if(value==causeFault)return "causeFault";throw new AssertionError("identity");}}
 public static void work(int i){{trace.add("work:"+i);if(i==failAt)throw originalFault;}}
 {methods}
 static void runCase(int kind,int limit,int fail,int choice,String expected){{trace.clear();failAt=fail;mode=choice;
  try{{if(kind==0)original(limit,a);else if(kind==1)parameterReceiver(limit,a);else if(kind==2)updatedReceiverSnapshot(limit,a,b);else if(kind==4)priorThrowSnapshot(limit,a,b);else phiSnapshot(limit,a,b,c);trace.add("return");}}
  catch(Throwable failure){{trace.add("throw:"+id(failure));}}
  String actual=String.join("|",trace);if(!actual.equals(expected))throw new AssertionError(kind+":"+limit+":"+fail+":"+choice+":"+actual+" != "+expected);
 }}
 public static void main(String[] args){{{cases}}}
}}
"#
    );
    fs::write(dir.join("sample/HandlerTail.java"), &java).unwrap();
    for (program, argument) in [
        ("javac", "sample/HandlerTail.java"),
        ("java", "sample.HandlerTail"),
    ] {
        let output = Command::new(program)
            .arg(argument)
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
