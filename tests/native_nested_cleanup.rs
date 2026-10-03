use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_java,
};
use std::sync::Arc;
fn fixture() -> DexClass {
    let owner: Arc<str> = "Lsample/Cleanup;".into();
    let ty: Arc<str> = "Ljava/lang/RuntimeException;".into();
    DexClass {
        descriptor: owner.clone(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            types: vec![owner.clone()],
            strings: vec![
                "obtain".into(),
                "work".into(),
                "release".into(),
                "log".into(),
            ],
            protos: vec![
                ("Ljava/lang/Object;".into(), vec![]),
                ("Z".into(), vec!["Ljava/lang/Object;".into()]),
                ("V".into(), vec!["Ljava/lang/Object;".into()]),
                ("V".into(), vec![ty.clone()]),
            ],
            methods: vec![(0, 0, 0), (0, 1, 1), (0, 2, 2), (0, 3, 3)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: owner,
            name: "test".into(),
            return_type: "V".into(),
            parameters: vec![ty.clone()],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 3,
                ins: 1,
                outs: 1,
                tries: 5,
                offset: 0,
                instructions: vec![
                    0x0071, 0, 0, 0x010c, 0x1071, 1, 1, 0x000a, 0x0038, 6, 0x1071, 2, 1, 0x000e,
                    0x0227, 0x000d, 0x1071, 2, 1, 0x0027, 0x000d, 0x1071, 3, 0, 0x000e,
                ],
                try_regions: [
                    (0, 4, Some(ty.clone()), 20),
                    (4, 8, None, 15),
                    (10, 13, Some(ty.clone()), 20),
                    (14, 15, None, 15),
                    (16, 20, Some(ty), 20),
                ]
                .into_iter()
                .map(|(start, end, ty, h)| DexTryRegion {
                    start,
                    end,
                    catches: vec![(ty, h)].into(),
                })
                .collect(),
            }),
        }],
    }
}
fn render(c: &DexClass) -> anyhow::Result<rdx::engine::DecompiledCode> {
    native_java::render_method("sample.Cleanup", c, &c.methods[0])
}
#[test]
fn duplicate_cleanup_becomes_one_finally_with_original_links() {
    let code = render(&fixture()).unwrap();
    assert_eq!(
        code.source.matches("finally {").count(),
        1,
        "{}",
        code.source
    );
    assert_eq!(
        code.source.matches("sample.Cleanup.release(").count(),
        1,
        "{}",
        code.source
    );
    assert!(code.source.contains("catch (java.lang.RuntimeException"));
    for (label, token) in [
        ("sample.Cleanup.obtain()Ljava/lang/Object;", "obtain"),
        ("sample.Cleanup.work(Ljava/lang/Object;)Z", "work"),
        ("sample.Cleanup.release(Ljava/lang/Object;)V", "release"),
    ] {
        let links: Vec<_> = code.links.iter().filter(|l| l.label == label).collect();
        assert_eq!(links.len(), 1);
        assert_eq!(
            code.source
                .chars()
                .skip(links[0].start)
                .take(links[0].end - links[0].start)
                .collect::<String>(),
            token
        );
    }
}
#[test]
fn distinct_cleanup_arguments_and_rethrow_targets_use_generic_try_catch() {
    for (index, value) in [(18, 2), (19, 0x0227)] {
        let mut class = fixture();
        class.methods[0].code.as_mut().unwrap().instructions[index] = value;
        let code = render(&class).unwrap();
        assert!(!code.source.contains("finally {"), "{}", code.source);
        assert!(
            code.source.contains("catch (java.lang.Throwable"),
            "{}",
            code.source
        );
    }
}

#[test]
fn malformed_cleanup_dispatch_and_try_boundaries_are_rejected() {
    for (index, value) in [(9, 5), (7, 0x010a)] {
        let mut class = fixture();
        class.methods[0].code.as_mut().unwrap().instructions[index] = value;
        assert!(render(&class).is_err(), "accepted mutation {index}");
    }
    let mut class = fixture();
    class.methods[0].code.as_mut().unwrap().try_regions[1].start = 7;
    assert!(render(&class).is_err());
    let mut class = fixture();
    class.methods[0].code.as_mut().unwrap().try_regions[2].end = 12;
    assert!(
        render(&class).is_err(),
        "accepted split invocation boundary"
    );
}

#[test]
fn cleanup_log_argument_requires_original_reference_type_proof() {
    let mut class = fixture();
    // obtain() is declared Object; changing release(Object) to
    // log(RuntimeException) adds no DEX check-cast or narrower definition.
    class.methods[0].code.as_mut().unwrap().instructions[17] = 3;
    assert!(
        render(&class).is_err(),
        "accepted an unproven Object-to-RuntimeException call argument"
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    None,
    Runtime,
    Other,
}
#[derive(Clone, Copy, Debug)]
struct Scenario {
    success: bool,
    faults: [Fault; 4],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reference {
    Resource,
    Parameter,
    Failure(u16, Fault),
}
impl Reference {
    fn name(self) -> String {
        match self {
            Self::Resource => "resource".into(),
            Self::Parameter => "param".into(),
            Self::Failure(method, kind) => format!(
                "{}.{}",
                ["obtain", "work", "release", "log"][usize::from(method)],
                if kind == Fault::Runtime {
                    "runtime"
                } else {
                    "other"
                }
            ),
        }
    }
    fn runtime(self) -> bool {
        matches!(self, Self::Parameter | Self::Failure(_, Fault::Runtime))
    }
}
#[derive(Clone, Copy, Debug)]
enum Register {
    Undefined,
    Reference(Reference),
    Boolean(bool),
}

// Execute raw DEX registers, instruction PCs, invocation operands and original
// ordered handler tables. This oracle has no knowledge of the emitted Java
// control structure, finally factoring, or mutation-specific intended behavior.
fn dex_trace(class: &DexClass, scenario: Scenario) -> Vec<String> {
    let code = class.methods[0].code.as_ref().unwrap();
    let mut regs = vec![Register::Undefined; usize::from(code.registers)];
    regs[usize::from(code.registers - code.ins)] = Register::Reference(Reference::Parameter);
    let mut pending = None;
    let mut caught = None;
    let mut trace = vec![];
    let mut pc = 0usize;
    for _ in 0..64 {
        let at = pc;
        let word = code.instructions[pc];
        let destination = usize::from(word >> 8);
        let mut thrown = None;
        match word as u8 {
            0x71 => {
                let method = code.instructions[pc + 1];
                let argument = if word >> 12 == 0 {
                    None
                } else {
                    assert_eq!(word >> 12, 1);
                    let register = usize::from(code.instructions[pc + 2] & 15);
                    match regs[register] {
                        Register::Reference(value) => Some(value),
                        _ => panic!("uninitialized reference argument at {pc}"),
                    }
                };
                let event = ["obtain", "work", "release", "log"][usize::from(method)];
                trace.push(
                    argument
                        .map_or_else(|| event.into(), |value| format!("{event}:{}", value.name())),
                );
                let fault = scenario.faults[usize::from(method)];
                pending = None;
                if fault != Fault::None {
                    thrown = Some(Reference::Failure(method, fault));
                } else {
                    pending = match method {
                        0 => Some(Register::Reference(Reference::Resource)),
                        1 => Some(Register::Boolean(scenario.success)),
                        2 | 3 => None,
                        _ => unreachable!(),
                    };
                }
                pc += 3;
            }
            0x0a | 0x0c => {
                regs[destination] = pending
                    .take()
                    .expect("move-result without original call result");
                pc += 1;
            }
            0x0d => {
                regs[destination] =
                    Register::Reference(caught.take().expect("handler without exception"));
                pc += 1;
            }
            0x38 => {
                let Register::Boolean(value) = regs[destination] else {
                    panic!("if-eqz without Boolean");
                };
                pc = if value {
                    pc + 2
                } else {
                    (pc as isize + code.instructions[pc + 1] as i16 as isize) as usize
                };
            }
            0x27 => {
                let Register::Reference(value) = regs[destination] else {
                    panic!("throw without exception");
                };
                assert_ne!(value, Reference::Resource);
                thrown = Some(value);
            }
            0x0e => {
                trace.push("return".into());
                return trace;
            }
            op => panic!("unsupported fixture opcode {op:#x}"),
        }
        if let Some(exception) = thrown {
            pending = None;
            let handler = code
                .try_regions
                .iter()
                .find(|region| (region.start as usize) <= at && at < (region.end as usize))
                .and_then(|region| {
                    region
                        .catches
                        .iter()
                        .find(|(ty, _)| ty.is_none() || exception.runtime())
                });
            if let Some((_, target)) = handler {
                caught = Some(exception);
                pc = *target as usize;
            } else {
                trace.push(format!("throw:{}", exception.name()));
                return trace;
            }
        }
    }
    panic!("fixture exceeded bounded DEX execution");
}
fn scenarios() -> Vec<Scenario> {
    let mut result = vec![];
    for success in [false, true] {
        for obtain in [Fault::None, Fault::Runtime, Fault::Other] {
            for work in [Fault::None, Fault::Runtime, Fault::Other] {
                for release in [Fault::None, Fault::Runtime, Fault::Other] {
                    for log in [Fault::None, Fault::Runtime, Fault::Other] {
                        result.push(Scenario {
                            success,
                            faults: [obtain, work, release, log],
                        });
                    }
                }
            }
        }
    }
    result
}

#[test]
fn dex_oracle_distinguishes_release_argument_and_original_rethrow_identity() {
    let scenario = Scenario {
        success: true,
        faults: [Fault::None, Fault::Other, Fault::None, Fault::None],
    };
    let baseline = fixture();
    assert_eq!(
        dex_trace(&baseline, scenario),
        [
            "obtain",
            "work:resource",
            "release:resource",
            "throw:work.other"
        ]
    );
    let mut distinct_argument = fixture();
    distinct_argument.methods[0]
        .code
        .as_mut()
        .unwrap()
        .instructions[18] = 2;
    assert_eq!(
        dex_trace(&distinct_argument, scenario),
        [
            "obtain",
            "work:resource",
            "release:param",
            "throw:work.other"
        ]
    );
    let mut distinct_throw = fixture();
    distinct_throw.methods[0]
        .code
        .as_mut()
        .unwrap()
        .instructions[19] = 0x0227;
    assert_eq!(
        dex_trace(&distinct_throw, scenario),
        [
            "obtain",
            "work:resource",
            "release:resource",
            "log:param",
            "return"
        ]
    );
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn unchanged_source_jvm_matches_original_dex_cleanup_argument_order_and_exception_identity() {
    use std::{fs, process::Command};
    let dir = std::env::temp_dir().join(format!("rdx-nested-cleanup-jvm-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let mut methods = String::new();
    let mut cases = String::new();
    for (variant, mutation) in [None, Some((18, 2)), Some((19, 0x0227))]
        .into_iter()
        .enumerate()
    {
        let mut class = fixture();
        if let Some((index, value)) = mutation {
            class.methods[0].code.as_mut().unwrap().instructions[index] = value;
        }
        class.methods[0].name = format!("test{variant}").into();
        methods.push_str(&render(&class).unwrap().source);
        for scenario in scenarios() {
            let expected = dex_trace(&class, scenario).join("|");
            let faults = scenario.faults.map(|fault| match fault {
                Fault::None => 0,
                Fault::Runtime => 1,
                Fault::Other => 2,
            });
            cases.push_str(&format!(
                "runCase({variant},{},{},{},{},{},\"{expected}\");\n",
                scenario.success, faults[0], faults[1], faults[2], faults[3]
            ));
        }
    }
    let java = format!(
        r#"package sample;
public class Cleanup {{
 static final Object resource = new Object();
 static final RuntimeException param = new RuntimeException("param");
 static final String[] names = {{"obtain","work","release","log"}};
 static final RuntimeException[] runtime = {{new RuntimeException("obtain"),new RuntimeException("work"),new RuntimeException("release"),new RuntimeException("log")}};
 static final Error[] other = {{new Error("obtain"),new Error("work"),new Error("release"),new Error("log")}};
 static final java.util.List<String> trace = new java.util.ArrayList<>();
 static final int[] faults = new int[4]; static boolean success;
 static String id(Object value) {{
   if(value==resource)return "resource";if(value==param)return "param";
   for(int i=0;i<4;i++){{if(value==runtime[i])return names[i]+".runtime";if(value==other[i])return names[i]+".other";}}
   throw new AssertionError("unrecognized object identity "+value);
 }}
 static void fail(int method) {{if(faults[method]==1)throw runtime[method];if(faults[method]==2)throw other[method];}}
 public static Object obtain() {{trace.add("obtain");fail(0);return resource;}}
 public static boolean work(Object value) {{trace.add("work:"+id(value));fail(1);return success;}}
 public static void release(Object value) {{trace.add("release:"+id(value));fail(2);}}
 public static void log(RuntimeException value) {{trace.add("log:"+id(value));fail(3);}}
 {methods}
 static void runCase(int variant,boolean ok,int f0,int f1,int f2,int f3,String expected) {{
   trace.clear();success=ok;faults[0]=f0;faults[1]=f1;faults[2]=f2;faults[3]=f3;
   try{{if(variant==0)test0(param);else if(variant==1)test1(param);else test2(param);trace.add("return");}}
   catch(Throwable failure){{trace.add("throw:"+id(failure));}}
   String actual=String.join("|",trace);
   if(!actual.equals(expected))throw new AssertionError("variant="+variant+",ok="+ok+",faults="+java.util.Arrays.toString(faults)+": "+actual+" != "+expected);
 }}
 public static void main(String[] args){{{cases}}}
}}
"#
    );
    fs::write(dir.join("sample/Cleanup.java"), &java).unwrap();
    for (program, argument) in [("javac", "sample/Cleanup.java"), ("java", "sample.Cleanup")] {
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
