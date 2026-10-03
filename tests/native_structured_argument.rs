use rdx::{
    native_calls, native_cfg, native_constructors,
    native_dex::{self, DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_ir, native_java, native_method,
};
use std::{fs, process::Command, sync::Arc};
#[path = "../src/native_java/structured_argument_region.rs"]
mod proof;
fn fixture() -> DexClass {
    DexClass {
        symbols: Arc::new(DexSymbols {
            strings: vec!["value".into(), "<init>".into()],
            types: vec!["Lsample/A;".into(), "Lsample/Source;".into()],
            protos: vec![("I".into(), vec![]), ("V".into(), vec!["I".into()])],
            methods: vec![(1, 0, 0), (0, 1, 1)],
            ..Default::default()
        }),
        descriptor: "Lsample/Test;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![DexMethod {
            declaring_type: "Lsample/Test;".into(),
            name: "make".into(),
            return_type: "Lsample/A;".into(),
            parameters: vec!["Z".into()],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 3,
                ins: 1,
                outs: 2,
                tries: 0,
                try_regions: vec![],
                instructions: vec![
                    0x0022, 0, 0x0238, 7, 0x0071, 0, 0, 0x010a, 0x0228, 0x2112, 0x2070, 1, 0x0010,
                    0x0011,
                ],
                offset: 0,
            }),
        }],
    }
}
fn wide_fixture() -> DexClass {
    let mut c = fixture();
    let symbols = Arc::get_mut(&mut c.symbols).unwrap();
    symbols.protos[0] = ("J".into(), vec!["J".into(), "Ljava/lang/Object;".into()]);
    symbols.protos[1].1 = vec!["J".into(), "Ljava/lang/Object;".into()];
    let m = &mut c.methods[0];
    m.name = "makeWide".into();
    m.return_type = "J".into();
    m.parameters = vec!["Z".into(), "J".into(), "Ljava/lang/Object;".into()];
    let code = m.code.as_mut().unwrap();
    code.registers = 8;
    code.ins = 4;
    code.outs = 4;
    code.instructions = vec![
        0x0022, 0, 0x0438, 7, 0x3071, 0, 0x0765, 0x010b, 0x0328, 0x0116, 2, 0x4070, 1, 0x7210,
        0x0110,
    ];
    c
}
#[test]
fn constructor_liveouts_keep_wide_pairs_and_reference_inputs() {
    let c = wide_fixture();
    let plan = proof::prove(&c, &c.methods[0], 0, 11).unwrap();
    assert!(plan.definitely_written.contains(&1) && plan.definitely_written.contains(&2));
    assert!(
        plan.first_reads
            .iter()
            .any(|r| r.register == 5 && r.words == 2)
    );
    let code = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
    assert!(code.source.contains("switch (0)"), "{}", code.source);
    assert!(code.source.contains("long v"), "{}", code.source);
    let mut c = wide_fixture();
    c.methods[0].code.as_mut().unwrap().instructions[9] = 0x1113;
    assert!(proof::prove(&c, &c.methods[0], 0, 11).is_none());
    assert!(native_java::render_method("sample.Test", &c, &c.methods[0]).is_err());
}
fn protected_fixture() -> DexClass {
    let mut c = fixture();
    let sy = Arc::get_mut(&mut c.symbols).unwrap();
    sy.strings.push("handled".into());
    sy.protos
        .push(("V".into(), vec!["Ljava/lang/RuntimeException;".into()]));
    sy.methods.push((1, 2, 2));
    let m = &mut c.methods[0];
    m.name = "makeProtected".into();
    let code = m.code.as_mut().unwrap();
    code.instructions
        .extend([0x010d, 0x1071, 2, 1, 0x0012, 0x0011]);
    code.tries = 1;
    code.try_regions = vec![DexTryRegion {
        start: 0,
        end: 13,
        catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 14)].into(),
    }];
    c
}
fn store_fixture() -> DexClass {
    let mut c = fixture();
    let sy = Arc::get_mut(&mut c.symbols).unwrap();
    sy.types[1] = "Lsample/Store;".into();
    sy.types.push("I".into());
    sy.strings = vec!["number".into(), "<init>".into()];
    sy.fields = vec![(1, 2, 0)];
    sy.methods = vec![(0, 1, 1)];
    let m = &mut c.methods[0];
    m.name = "makeStore".into();
    m.parameters.clear();
    let code = m.code.as_mut().unwrap();
    code.ins = 0;
    code.instructions = vec![
        0x0022, 0, 0x7112, 0x0167, 0, 0x0160, 0, 0x2070, 0, 0x0010, 0x0011,
    ];
    c
}
#[test]
fn original_field_store_and_read_retain_metadata_and_allocation_position() {
    let c = store_fixture();
    let rendered = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
    assert!(
        rendered.source.find("new sample.A").unwrap()
            < rendered.source.find("sample.Store.number = 7").unwrap()
    );
    let links: Vec<_> = rendered
        .links
        .iter()
        .filter(|l| l.label == "sample.Store.number:I")
        .collect();
    assert_eq!(links.len(), 2);
    for link in links {
        assert_eq!(
            rendered
                .source
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect::<String>(),
            "number"
        );
    }
}
fn cast_fixture() -> DexClass {
    let mut c = fixture();
    let sy = Arc::get_mut(&mut c.symbols).unwrap();
    sy.types.push("Ljava/lang/String;".into());
    sy.protos[1].1 = vec!["Ljava/lang/String;".into()];
    let m = &mut c.methods[0];
    m.name = "makeCast".into();
    m.parameters = vec!["Z".into(), "Ljava/lang/Object;".into()];
    let code = m.code.as_mut().unwrap();
    code.registers = 4;
    code.ins = 2;
    code.instructions = vec![
        0x0022, 0, 0x0238, 6, 0x031f, 2, 0x3107, 0x0228, 0x0112, 0x2070, 1, 0x0010, 0x0011,
    ];
    c
}
#[test]
fn original_cast_and_complete_enclosing_dispatch_are_retained() {
    let cast = cast_fixture();
    assert!(proof::prove(&cast, &cast.methods[0], 0, 9).is_some());
    let rendered = native_java::render_method("sample.Test", &cast, &cast.methods[0]).unwrap();
    assert!(
        rendered.source.find("new sample.A").unwrap()
            < rendered.source.find("((java.lang.String) p1)").unwrap(),
        "{}",
        rendered.source
    );
    let c = protected_fixture();
    assert!(proof::prove(&c, &c.methods[0], 0, 10).is_some());
    let rendered = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
    assert_eq!(rendered.source.matches("catch (").count(), 1);
    assert!(
        rendered.source.contains("sample.Source.handled("),
        "{}",
        rendered.source
    );
    let mut c = fixture();
    c.methods[0]
        .code
        .as_mut()
        .unwrap()
        .instructions
        .resize(4097, 0);
    assert!(proof::prove(&c, &c.methods[0], 0, 10).is_none());
}
#[test]
fn complete_forward_region_and_raw_reproof() {
    let c = fixture();
    let m = &c.methods[0];
    let p = proof::prove(&c, m, 0, 10).unwrap();
    assert_eq!(p.owned, vec![2, 4, 7, 8, 9]);
    assert!(proof::validate(&c, m, &p));
    assert_eq!(p.definitely_written, vec![1]);
    assert_eq!(p.first_reads.len(), 1);
    for mutation in 0..10 {
        let mut b = p.clone();
        match mutation {
            0 => b.words[3] += 1,
            1 => b.registers += 1,
            2 => {
                b.owned.pop();
            }
            3 => {
                b.edges.pop();
            }
            4 => b.first_reads[0].register = 0,
            5 => b.definitely_written.clear(),
            6 => b.calls.clear(),
            7 => b.semantic_references.clear(),
            8 => b.constructor = 9,
            9 => b.ins = 0,
            _ => unreachable!(),
        };
        assert!(!proof::validate(&c, m, &b), "poison {mutation}");
    }
    let rendered = native_java::render_method("sample.Test", &c, m).unwrap();
    assert!(
        rendered.source.contains("new sample.A(switch (0)"),
        "{}",
        rendered.source
    );
    assert!(
        rendered.source.find("new sample.A").unwrap()
            < rendered.source.find("sample.Source.value()").unwrap()
    );
    for link in &rendered.links {
        assert!(link.start <= link.end && link.end <= rendered.source.chars().count());
    }
}
#[test]
fn raw_escaping_pending_alias_allocation_and_handler_edges_reject() {
    for mutation in 0..9 {
        let mut c = fixture();
        let code = c.methods[0].code.as_mut().unwrap();
        match mutation {
            0 => code.instructions[3] = 11, // escape past original constructor
            1 => code.instructions[3] = 5,  // branch into move-result
            2 => code.instructions[8] = 0xfc28, // backward goto
            3 => code.instructions[4] = 0x0022, // nested allocation
            4 => code.instructions[4] = 0x001a, // deferred string
            5 => code.instructions[2] = 0x0038, // read uninitialized receiver
            6 => code.instructions[9] = 0x0012, // destroys receiver on one arm
            7 => {
                code.tries = 1;
                code.try_regions.push(DexTryRegion {
                    start: 2,
                    end: 10,
                    catches: vec![(None, 13)].into(),
                });
            }
            8 => {
                code.tries = 1;
                code.try_regions.push(DexTryRegion {
                    start: 0,
                    end: 13,
                    catches: vec![(None, 7)].into(),
                });
            }
            _ => unreachable!(),
        }
        assert!(
            proof::prove(&c, &c.methods[0], 0, 10).is_none(),
            "negative {mutation}"
        );
    }
}
#[test]
fn every_original_reference_receiver_and_argument_requires_positive_domain() {
    for receiver in [true, false] {
        let mut c = fixture();
        let symbols = Arc::get_mut(&mut c.symbols).unwrap();
        symbols.strings = vec!["provider".into(), "value".into(), "<init>".into()];
        symbols.types.push("Ljava/lang/String;".into());
        symbols.protos[0] = ("Ljava/lang/Object;".into(), vec![]);
        symbols.protos.push((
            "I".into(),
            if receiver {
                vec![]
            } else {
                vec!["Ljava/lang/String;".into()]
            },
        ));
        symbols.methods = vec![(1, 0, 0), (1, 2, 1), (0, 1, 2)];
        let m = &mut c.methods[0];
        m.parameters.clear();
        let code = m.code.as_mut().unwrap();
        code.ins = 0;
        code.instructions = vec![
            0x0022,
            0,
            0x0071,
            0,
            0,
            0x010c,
            if receiver { 0x1072 } else { 0x1071 },
            1,
            1,
            0x010a,
            0x2070,
            2,
            0x0010,
            0x0011,
        ];
        assert!(proof::prove(&c, &c.methods[0], 0, 10).is_some());
        assert!(
            native_java::render_method("sample.Test", &c, &c.methods[0]).is_err(),
            "missing original cast: receiver={receiver}"
        );
        c.methods[0]
            .code
            .as_mut()
            .unwrap()
            .instructions
            .splice(6..6, [0x011f, if receiver { 1 } else { 2 }]);
        let rendered = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
        assert!(
            rendered.source.contains(if receiver {
                "sample.Source)"
            } else {
                "java.lang.String)"
            }),
            "{}",
            rendered.source
        );
    }
}
// Independent execution reads original encoded registers, offsets and method-pool identity.
fn oracle(c: &DexClass, branch: bool, mask: u8) -> String {
    let w = &c.methods[0].code.as_ref().unwrap().instructions;
    let mut r = [0i32; 3];
    r[2] = i32::from(branch);
    let mut pc = 0;
    let mut pending = 0;
    let mut trace = String::new();
    loop {
        let op = w[pc] as u8;
        let a = (w[pc] >> 8) as usize;
        match op {
            0x22 => {
                trace.push('N');
                if mask & 1 != 0 {
                    return format!("{trace}:N");
                }
                r[a] = -1;
                pc += 2;
            }
            0x38 => {
                pc = if r[a] == 0 {
                    (pc as isize + w[pc + 1] as i16 as isize) as usize
                } else {
                    pc + 2
                };
            }
            0x71 => {
                let id = w[pc + 1] as usize;
                let (_, proto, name) = c.symbols.methods[id];
                assert_eq!(c.symbols.strings[name as usize], "value");
                assert!(c.symbols.protos[proto as usize].1.is_empty());
                trace.push('S');
                if mask & 2 != 0 {
                    return format!("{trace}:S");
                }
                pending = 42;
                pc += 3;
            }
            0x0a => {
                r[a] = pending;
                pc += 1;
            }
            0x28 => pc = (pc as isize + (w[pc] >> 8) as u8 as i8 as isize) as usize,
            0x12 => {
                r[a & 15] = (a as u8 as i8 >> 4) as i32;
                pc += 1;
            }
            0x70 => {
                let packed = w[pc + 2];
                let recv = (packed & 15) as usize;
                let arg = ((packed >> 4) & 15) as usize;
                assert_eq!(r[recv], -1);
                trace.push('C');
                if mask & 4 != 0 {
                    return format!("{trace}:C");
                }
                r[recv] = r[arg];
                pc += 3;
            }
            0x11 => return format!("{trace}:{}", r[a]),
            _ => panic!("unsupported oracle opcode {op:x}@{pc}"),
        }
    }
}
fn oracle_wide(c: &DexClass, branch: bool, mask: u8, seed: i64) -> String {
    let w = &c.methods[0].code.as_ref().unwrap().instructions;
    let mut r = vec![0u32; 8];
    r[4] = u32::from(branch);
    r[5] = seed as u32;
    r[6] = (seed as u64 >> 32) as u32;
    r[7] = 0xdefa;
    let mut pc = 0;
    let mut pending = 0i64;
    let mut trace = String::new();
    let wide = |r: &[u32], p: usize| ((r[p] as u64) | ((r[p + 1] as u64) << 32)) as i64;
    loop {
        let op = w[pc] as u8;
        let a = (w[pc] >> 8) as usize;
        match op {
            0x22 => {
                trace.push('N');
                if mask & 1 != 0 {
                    return format!("{trace}:N");
                }
                r[a] = 0xffff_ffff;
                pc += 2;
            }
            0x38 => {
                pc = if r[a] == 0 {
                    (pc as isize + w[pc + 1] as i16 as isize) as usize
                } else {
                    pc + 2
                }
            }
            0x71 => {
                let packed = w[pc + 2];
                let regs = [
                    (packed & 15) as usize,
                    ((packed >> 4) & 15) as usize,
                    ((packed >> 8) & 15) as usize,
                ];
                assert_eq!(regs[1], regs[0] + 1);
                assert_eq!(r[regs[2]], 0xdefa);
                trace.push('S');
                if mask & 2 != 0 {
                    return format!("{trace}:S");
                }
                pending = wide(&r, regs[0]) ^ 0x1234_5678_9abc_def0;
                pc += 3;
            }
            0x0b => {
                r[a] = pending as u32;
                r[a + 1] = (pending as u64 >> 32) as u32;
                pc += 1;
            }
            0x28 => pc = (pc as isize + (w[pc] >> 8) as u8 as i8 as isize) as usize,
            0x16 => {
                let n = w[pc + 1] as i16 as i64;
                r[a] = n as u32;
                r[a + 1] = (n as u64 >> 32) as u32;
                pc += 2;
            }
            0x70 => {
                let packed = w[pc + 2];
                assert_eq!(r[(packed & 15) as usize], 0xffff_ffff);
                assert_eq!(r[((packed >> 12) & 15) as usize], 0xdefa);
                trace.push('C');
                if mask & 4 != 0 {
                    return format!("{trace}:C");
                }
                pc += 3;
            }
            0x10 => return format!("{trace}:{}", wide(&r, a)),
            _ => panic!("unsupported wide opcode {op:x}@{pc}"),
        }
    }
}
fn oracle_store(c: &DexClass, mask: u8) -> String {
    let w = &c.methods[0].code.as_ref().unwrap().instructions;
    let mut r = [0i32; 3];
    let mut pc = 0;
    let mut field = 0;
    let mut ready = false;
    let mut trace = String::new();
    loop {
        let op = w[pc] as u8;
        let a = (w[pc] >> 8) as usize;
        match op {
            0x22 => {
                trace.push('N');
                if mask & 1 != 0 {
                    return format!("{trace}:N");
                }
                r[a] = -1;
                pc += 2;
            }
            0x12 => {
                r[a & 15] = (a as u8 as i8 >> 4) as i32;
                pc += 1;
            }
            0x67 | 0x60 => {
                let index = w[pc + 1] as usize;
                assert_eq!(c.symbols.fields[index].0, 1);
                if !ready {
                    trace.push('F');
                    if mask & 16 != 0 {
                        return format!("{trace}:F");
                    }
                    ready = true;
                }
                if op == 0x67 {
                    field = r[a];
                } else {
                    r[a] = field;
                }
                pc += 2;
            }
            0x70 => {
                let packed = w[pc + 2];
                let recv = (packed & 15) as usize;
                let arg = ((packed >> 4) & 15) as usize;
                trace.push('C');
                if mask & 4 != 0 {
                    return format!("{trace}:C:{field}");
                }
                r[recv] = r[arg];
                pc += 3;
            }
            0x11 => return format!("{trace}:{}:{field}", r[a]),
            _ => panic!("unexpected store opcode {op:x}@{pc}"),
        }
    }
}
#[test]
#[ignore = "requires explicit JDK25"]
fn unchanged_emission_preserves_new_class_initialization_and_both_branch_faults() {
    let c = fixture();
    let code = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
    let sc = store_fixture();
    let storecode = native_java::render_method("sample.Test", &sc, &sc.methods[0]).unwrap();
    let pc = protected_fixture();
    let protectedcode = native_java::render_method("sample.Test", &pc, &pc.methods[0]).unwrap();
    let cc = cast_fixture();
    let castcode = native_java::render_method("sample.Test", &cc, &cc.methods[0]).unwrap();
    let wc = wide_fixture();
    let widecode = native_java::render_method("sample.Test", &wc, &wc.methods[0]).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-structured-argument-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let mut cases = vec![];
    for branch in [false, true] {
        for mask in 0..8 {
            cases.push(format!(
                "check({}, {}, \"{}\");",
                branch,
                mask,
                oracle(&c, branch, mask)
            ));
        }
    }
    for branch in [false, true] {
        for mask in 0..8 {
            for seed in [i64::MIN, i64::MAX, -1, 0, 7, 0x4567_89ab_cdef_1234] {
                let literal = if seed == i64::MIN {
                    "Long.MIN_VALUE".into()
                } else {
                    format!("{seed}L")
                };
                cases.push(format!(
                    "checkWide({}, {}, {}, \"{}\");",
                    branch,
                    mask,
                    literal,
                    oracle_wide(&wc, branch, mask, seed)
                ));
            }
        }
    }
    for branch in [false, true] {
        for mask in 0..16 {
            let mut expected = oracle(&pc, branch, mask);
            if expected.ends_with(":S") || expected.ends_with(":C") {
                expected.truncate(expected.len() - 2);
                expected.push_str(if mask & 8 != 0 { "H:H" } else { "H:null" });
            }
            cases.push(format!(
                "checkProtected({}, {}, \"{}\");",
                branch, mask, expected
            ));
        }
    }
    for branch in [false, true] {
        for mask in 0..8 {
            for kind in 0..3 {
                let expected = if mask & 1 != 0 {
                    "N:N"
                } else if branch && kind == 2 {
                    "N:cast"
                } else if mask & 4 != 0 {
                    "NC:C"
                } else {
                    "NC:0"
                };
                cases.push(format!(
                    "checkCast({}, {}, {}, \"{}\");",
                    branch, mask, kind, expected
                ));
            }
        }
    }
    for mask in [0, 1, 4, 5, 16, 17, 20, 21] {
        cases.push(format!(
            "checkStore({}, \"{}\");",
            mask,
            oracle_store(&sc, mask)
        ));
    }
    fs::write(dir.join("sample/Test.java"),format!(r#"package sample;
public class Test {{ {} {} {} {} {}
public static String runStore(int mask) {{State.mask=mask;try{{A a=makeStore();return State.trace+":"+a.value+":"+Store.number;}}catch(Throwable fault){{String result=outcome(fault);return fault==State.c?result+":"+Store.number:result;}}}}
public static String runCast(boolean branch,int mask,int kind) {{State.mask=mask;Object value=kind==0?null:kind==1?"ok":State.ref;try{{A a=makeCast(branch,value);return State.trace+":"+a.value;}}catch(ClassCastException fault){{return State.trace+":cast";}}catch(Throwable fault){{return outcome(fault);}}}}
public static String runProtected(boolean branch,int mask) {{State.mask=mask;try{{A a=makeProtected(branch);return State.trace+":"+(a==null?"null":a.value);}}catch(Throwable fault){{return outcome(fault);}}}}
public static String runWide(boolean branch,int mask,long seed) {{ State.mask=mask;State.seed=seed;try {{long result=makeWide(branch,seed,State.ref);return State.trace+":"+result;}}catch(Throwable fault){{return outcome(fault);}} }}
static String outcome(Throwable fault){{if(fault==State.n)return State.trace+":N";if(fault==State.s)return State.trace+":S";if(fault==State.c)return State.trace+":C";if(fault==State.h)return State.trace+":H";if(fault==State.f)return State.trace+":F";throw new AssertionError("unexpected fault identity",fault);}}
public static String run(boolean branch,int mask) {{ State.mask=mask;try {{ A a=make(branch);return State.trace+":"+a.value; }}catch(Throwable fault){{if(fault==State.n)return State.trace+":N";if(fault==State.s)return State.trace+":S";if(fault==State.c)return State.trace+":C";if(fault==State.h)return State.trace+":H";if(fault==State.f)return State.trace+":F";throw new AssertionError("unexpected fault identity",fault);}} }} }}
class State {{ static int mask;static long seed;static final Object ref=new Object();static String trace="";static final AssertionError n=new AssertionError("N"),f=new AssertionError("F");static final RuntimeException s=new RuntimeException("S"),c=new RuntimeException("C"),h=new RuntimeException("H"); }}
class Store {{static int number;static {{State.trace+="F";if((State.mask&16)!=0)throw State.f;}}}}
class Source {{static void handled(RuntimeException fault){{if(fault!=State.s&&fault!=State.c)throw new AssertionError("handler identity");State.trace+="H";if((State.mask&8)!=0)throw State.h;}}static long value(long seed,Object ref){{if(seed!=State.seed||ref!=State.ref)throw new AssertionError("input identity");State.trace+="S";if((State.mask&2)!=0)throw State.s;return seed^0x123456789abcdef0L;}} static int value() {{State.trace+="S";if((State.mask&2)!=0)throw State.s;return 42;}} }}
class A {{static {{State.trace+="N";if((State.mask&1)!=0)throw State.n;}}A(String value){{State.trace+="C";if((State.mask&4)!=0)throw State.c;this.value=0;}} A(long value,Object ref){{if(ref!=State.ref)throw new AssertionError("ctor reference");State.trace+="C";if((State.mask&4)!=0)throw State.c;this.value=0;}} final int value;A(int value){{State.trace+="C";if((State.mask&4)!=0)throw State.c;this.value=value;}} }}
"#,code.source,widecode.source,protectedcode.source,castcode.source,storecode.source)).unwrap();
    fs::write(dir.join("Runner.java"),format!(r#"import java.net.*;import java.nio.file.*;
public class Runner {{static int count;static void checkStore(int mask,String expected)throws Exception{{checkSpecial("runStore",new Class<?>[]{{int.class}},new Object[]{{mask}},expected);}}static void checkProtected(boolean branch,int mask,String expected)throws Exception{{checkSpecial("runProtected",new Class<?>[]{{boolean.class,int.class}},new Object[]{{branch,mask}},expected);}}static void checkCast(boolean branch,int mask,int kind,String expected)throws Exception{{checkSpecial("runCast",new Class<?>[]{{boolean.class,int.class,int.class}},new Object[]{{branch,mask,kind}},expected);}}static void checkSpecial(String name,Class<?>[] types,Object[] inputs,String expected)throws Exception{{try(URLClassLoader loader=new URLClassLoader(new URL[]{{Path.of(System.getProperty("classes")).toUri().toURL()}},null)){{Class<?> t=Class.forName("sample.Test",true,loader);String actual=(String)t.getMethod(name,types).invoke(null,inputs);if(!expected.equals(actual))throw new AssertionError(expected+" != "+actual);count++;}}}}static void checkWide(boolean branch,int mask,long seed,String expected)throws Exception{{try(URLClassLoader loader=new URLClassLoader(new URL[]{{Path.of(System.getProperty("classes")).toUri().toURL()}},null)){{Class<?> t=Class.forName("sample.Test",true,loader);String actual=(String)t.getMethod("runWide",boolean.class,int.class,long.class).invoke(null,branch,mask,seed);if(!expected.equals(actual))throw new AssertionError(expected+" != "+actual);count++;}}}}static void check(boolean branch,int mask,String expected)throws Exception{{try(URLClassLoader loader=new URLClassLoader(new URL[]{{Path.of(System.getProperty("classes")).toUri().toURL()}},null)){{Class<?> t=Class.forName("sample.Test",true,loader);String actual=(String)t.getMethod("run",boolean.class,int.class).invoke(null,branch,mask);if(!expected.equals(actual))throw new AssertionError(expected+" != "+actual);count++;}}}}public static void main(String[] args)throws Exception{{{}System.out.print("ok:"+count);}}}}
"#,cases.join("\n"))).unwrap();
    let home = std::env::var("RDX_JAVA25_HOME").expect("explicit JDK25 home");
    let javac = Command::new(format!("{home}/bin/javac"))
        .arg(dir.join("sample/Test.java"))
        .arg(dir.join("Runner.java"))
        .output()
        .unwrap();
    assert!(
        javac.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&javac.stderr),
        code.source
    );
    let java = Command::new(format!("{home}/bin/java"))
        .arg("-Xverify:all")
        .arg(format!("-Dclasses={}", dir.display()))
        .arg("-cp")
        .arg(&dir)
        .arg("Runner")
        .output()
        .unwrap();
    assert!(
        java.status.success(),
        "{}",
        String::from_utf8_lossy(&java.stderr)
    );
    assert_eq!(java.stdout, b"ok:200");
    fs::remove_dir_all(dir).unwrap();
}
