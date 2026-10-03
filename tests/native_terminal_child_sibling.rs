//! Disjoint ordinary loops retain classification on either side of a selected pair.
use rdx::{
    native_cfg,
    native_dex::{self, DexClass, DexSymbols},
    native_dominators, native_ir, native_java,
};
use std::{collections::BTreeMap, fs, process::Command, sync::Arc};
#[path = "../src/native_java/terminal_child_loop.rs"]
mod terminal_child_loop;

#[derive(Default)]
struct Assembler {
    words: Vec<u16>,
    labels: BTreeMap<&'static str, usize>,
    fixups: Vec<(usize, &'static str)>,
}
impl Assembler {
    fn label(&mut self, name: &'static str) {
        assert!(self.labels.insert(name, self.words.len()).is_none());
    }
    fn words(&mut self, words: &[u16]) {
        self.words.extend_from_slice(words);
    }
    fn branch(&mut self, word: u16, target: &'static str) {
        self.fixups.push((self.words.len(), target));
        self.words(&[word, 0]);
    }
    fn go(&mut self, target: &'static str) {
        self.branch(0x29, target);
    }
    fn finish(mut self) -> (Vec<u16>, BTreeMap<&'static str, usize>) {
        for (pc, target) in self.fixups {
            self.words[pc + 1] = (self.labels[target] as isize - pc as isize) as i16 as u16;
        }
        (self.words, self.labels)
    }
}
fn allocate(a: &mut Assembler) {
    a.words(&[0x0722, 1, 0x1070, 1, 7]);
}
fn touch(a: &mut Assembler, wide: bool) {
    a.words(&[0x3071, 0, 0x0106, 0x040a]);
    if wide {
        a.words(&[0x4481, 0x029b, 0x0402]);
    } else {
        a.words(&[0x0290, 0x0402]);
    }
}
fn fixture(kind: usize) -> (DexClass, BTreeMap<&'static str, usize>) {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class.methods.retain(|m| m.name.as_ref() == "answer");
    class.symbols = Arc::new(DexSymbols {
        strings: vec!["touch".into(), "<init>".into()],
        types: vec!["Lsample/Effects;".into(), "Lsample/Token;".into()],
        protos: vec![("I".into(), vec!["I".into(); 3]), ("V".into(), vec![])],
        methods: vec![(0, 0, 0), (1, 1, 1)],
        ..Default::default()
    });
    let m = &mut class.methods[0];
    m.name = format!("sibling{kind}").into();
    m.access_flags = 9;
    m.parameters = vec!["I".into(); 4];
    m.return_type = if kind == 4 { "J" } else { "I" }.into();
    let c = m.code.as_mut().unwrap();
    c.registers = 12;
    c.ins = 4;
    c.outs = 3;
    c.tries = 0;
    c.try_regions.clear();
    let mut a = Assembler::default();
    if kind == 4 {
        let value = 5_000_000_000u64;
        a.words(&[
            0x0218,
            value as u16,
            (value >> 16) as u16,
            (value >> 32) as u16,
            (value >> 48) as u16,
        ]);
    } else {
        a.words(&[0x0212]);
    }
    a.words(&[0x0012, 0x0112, 0x0612]);
    if kind == 1 {
        allocate(&mut a);
    }
    a.label("before");
    a.branch(0x8035, "beforeExit");
    if kind == 3 {
        a.label("beforeAllocation");
        allocate(&mut a);
    }
    a.label("beforeTouch");
    touch(&mut a, kind == 4);
    a.words(&[0x00d8, 0x0100]);
    a.go("before");
    a.label("beforeExit");
    a.words(&[0x0012, 0x1612]);
    a.label("parent");
    a.branch(0x9035, "parentExit");
    a.words(&[0x00d8, 0x0100, 0x0112]);
    a.label("child");
    a.branch(0xa135, "parent");
    a.label("childTouch");
    touch(&mut a, kind == 4);
    a.words(&[0x01d8, 0x0101]);
    a.go("child");
    a.label("parentExit");
    if kind == 2 {
        allocate(&mut a);
    }
    a.words(&[0x0012, 0x0112, 0x2612]);
    a.label("after");
    a.branch(0xb035, "afterExit");
    a.label("afterTouch");
    touch(&mut a, kind == 4);
    a.label("afterIncrement");
    a.words(&[0x00d8, 0x0100]);
    a.go("after");
    a.label("afterExit");
    a.words(&[if kind == 4 { 0x0210 } else { 0x020f }]);
    let (words, labels) = a.finish();
    c.instructions = words;
    (class, labels)
}
fn source(kind: usize) -> String {
    let (c, _) = fixture(kind);
    native_java::render_method("sample.Hello", &c, &c.methods[0])
        .unwrap()
        .source
}
#[test]
fn exact_one_pair_and_each_ordinary_header_remains_owned() {
    for kind in 0..5 {
        let (c, l) = fixture(kind);
        let code = c.methods[0].code.as_ref().unwrap();
        let p = terminal_child_loop::select(code).unwrap();
        assert_eq!(p.parent_header, l["parent"]);
        assert_eq!(p.child_header, l["child"]);
        assert_eq!(p.parent_exit, l["parentExit"]);
        assert!(terminal_child_loop::validate(code, &p));
        assert!(!p.parent_members.contains(&l["before"]));
        assert!(!p.parent_members.contains(&l["after"]));
        assert_eq!(
            p.prefix_allocation_pairs.len(),
            usize::from((1..=3).contains(&kind))
        );
        for mutation in 0..6 {
            let mut poison = p.clone();
            match mutation {
                0 => poison.parent_members.push(l["before"]),
                1 => poison.parent_header = l["before"],
                2 => poison.child_latch -= 1,
                3 => poison.parent_exit = l["afterIncrement"],
                4 => poison.edges.pop().map(|_| ()).unwrap(),
                _ => poison.original_words[0] ^= 0x100,
            };
            assert!(!terminal_child_loop::validate(code, &poison));
        }
    }
}
#[test]
fn crossing_overlap_handler_allocation_and_constructor_interior_decline() {
    for target in ["childTouch", "beforeTouch"] {
        let (mut c, l) = fixture(0);
        let code = c.methods[0].code.as_mut().unwrap();
        let pc = if target == "childTouch" {
            l["before"]
        } else {
            l["child"]
        };
        code.instructions[pc + 1] = (l[target] as isize - pc as isize) as i16 as u16;
        assert!(
            terminal_child_loop::select(code).is_none(),
            "cross {target}"
        );
    }
    let (mut c, l) = fixture(0);
    let code = c.methods[0].code.as_mut().unwrap();
    let pc = code.instructions.len() - 3;
    code.instructions[pc + 1] = (l["parent"] as isize - pc as isize) as i16 as u16;
    assert!(
        terminal_child_loop::select(code).is_none(),
        "ancestor overlap"
    );
    let (mut c, _) = fixture(0);
    c.methods[0].code.as_mut().unwrap().tries = 1;
    assert!(terminal_child_loop::select(c.methods[0].code.as_ref().unwrap()).is_none());
    let (mut c, l) = fixture(0);
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions[l["childTouch"]] = 0x0722;
    assert!(
        terminal_child_loop::select(code).is_none(),
        "body allocation"
    );
    let (mut c, l) = fixture(3);
    let code = c.methods[0].code.as_mut().unwrap();
    let pc = l["parent"];
    code.instructions[pc + 1] = (l["beforeAllocation"] as isize + 2 - pc as isize) as i16 as u16;
    assert!(
        terminal_child_loop::select(code).is_none(),
        "constructor interior"
    );
    let (mut c, l) = fixture(3);
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions[l["beforeAllocation"] + 4] = 6;
    assert!(
        terminal_child_loop::select(code).is_none(),
        "constructor receiver mismatch"
    );
}
#[test]
fn completed_constructor_pairs_do_not_straddle_entries_and_keep_exact_symbol_guards() {
    let (mut c, l) = fixture(0);
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions[l["beforeExit"]] = 0x0722;
    code.instructions[l["beforeExit"] + 1] = 1;
    assert!(
        terminal_child_loop::select(code).is_none(),
        "new receiver cannot straddle parent entry"
    );
    let (mut c, l) = fixture(3);
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions[l["beforeAllocation"] + 2] = 0;
    assert!(
        terminal_child_loop::select(code).is_none(),
        "interrupted pair"
    );
    for mutation in 0..3 {
        let (mut c, _) = fixture(3);
        let symbols = Arc::get_mut(&mut c.symbols).unwrap();
        match mutation {
            0 => symbols.methods[1].0 = 0,
            1 => symbols.protos[1].0 = "I".into(),
            _ => symbols.protos[1].1 = vec!["I".into()],
        }
        assert!(
            terminal_child_loop::select(c.methods[0].code.as_ref().unwrap()).is_some(),
            "shape stays closed; actual symbols remain the renderer's authority"
        );
        assert!(
            native_java::render_method("sample.Hello", &c, &c.methods[0]).is_err(),
            "constructor owner/proto mutation {mutation}"
        );
    }
}
#[test]
fn ordinary_external_entry_guard_is_not_bypassed_by_selected_pair() {
    let (mut c, l) = fixture(0);
    let code = c.methods[0].code.as_mut().unwrap();
    let pc = l["parent"];
    code.instructions[pc + 1] = (l["afterIncrement"] as isize - pc as isize) as i16 as u16;
    // Keep the ordinary post-pair setup reachable via an independent bypass.
    // The parent guard nevertheless enters the later sibling's interior.
    let bypass = l["beforeExit"];
    code.instructions[bypass] = 0x0638;
    code.instructions[bypass + 1] = (l["parentExit"] as isize - bypass as isize) as i16 as u16;
    let p = terminal_child_loop::select(code).expect("selected exact pair remains provable");
    assert_eq!(p.parent_exit, l["afterIncrement"]);
    let error = native_java::render_method("sample.Hello", &c, &c.methods[0]).unwrap_err();
    assert!(
        format!("{error:#}").contains("loop has an interior entry"),
        "{error:#}"
    );
}
#[test]
fn every_header_emits_once_and_constructor_guard_remains_exact() {
    for kind in 0..5 {
        let (c, _) = fixture(kind);
        {
            let java = native_java::render_method("sample.Hello", &c, &c.methods[0])
                .unwrap()
                .source;
            assert_eq!(java.matches("while (").count(), 4, "{java}");
            assert_eq!(java.matches("sample.Effects.touch(").count(), 3, "{java}");
            assert!(!java.contains("switch ("));
        }
    }
    let (mut c, l) = fixture(3);
    c.methods[0].code.as_mut().unwrap().instructions[l["beforeAllocation"] + 3] = 0;
    assert!(
        native_java::render_method("sample.Hello", &c, &c.methods[0]).is_err(),
        "nonconstructor method index must not initialize allocation"
    );
}
// Independent instruction-driven oracle. Branch offsets, operands and original
// allocation/invoke order come from DEX words; no reconstructed source is read.
fn oracle(kind: usize, inputs: [i64; 4], fail_at: usize, is_error: bool) -> String {
    let (c, _) = fixture(kind);
    let words = &c.methods[0].code.as_ref().unwrap().instructions;
    let mut regs = [0i64; 12];
    regs[8..].copy_from_slice(&inputs);
    let mut pc = 0usize;
    let mut pending = 0;
    let mut trace = Vec::new();
    for _ in 0..10000 {
        let word = words[pc];
        let op = word as u8;
        let dest = (word >> 8) as usize;
        match op {
            0x12 => {
                regs[(word >> 8 & 15) as usize] = ((word as i16) >> 12) as i64;
                pc += 1;
            }
            0x18 => {
                regs[dest] = (u64::from(words[pc + 1])
                    | (u64::from(words[pc + 2]) << 16)
                    | (u64::from(words[pc + 3]) << 32)
                    | (u64::from(words[pc + 4]) << 48)) as i64;
                pc += 5;
            }
            0x22 => {
                regs[dest] = 777;
                pc += 2;
            }
            0x35 => {
                let a = (word >> 8 & 15) as usize;
                let b = (word >> 12) as usize;
                if regs[a] >= regs[b] {
                    pc = (pc as isize + words[pc + 1] as i16 as isize) as usize;
                } else {
                    pc += 2;
                }
            }
            0x29 => pc = (pc as isize + words[pc + 1] as i16 as isize) as usize,
            0x70 | 0x71 => {
                if words[pc + 1] == 1 {
                    assert_eq!(regs[(words[pc + 2] & 15) as usize], 777);
                    trace.push("C".into());
                } else {
                    let packed = words[pc + 2];
                    let stage = regs[(packed & 15) as usize];
                    let i = regs[(packed >> 4 & 15) as usize];
                    let j = regs[(packed >> 8 & 15) as usize];
                    trace.push(format!("{stage}:{i}:{j}"));
                    pending = stage * 100 + i * 10 + j;
                }
                if fail_at != 0 && trace.len() == fail_at {
                    return format!("{}|{}", if is_error { "E" } else { "R" }, trace.join(","));
                }
                pc += 3;
            }
            0x0a => {
                regs[dest] = pending;
                pc += 1;
            }
            0x81 => {
                let to = (word >> 8 & 15) as usize;
                let from = (word >> 12) as usize;
                regs[to] = regs[from] as i32 as i64;
                pc += 1;
            }
            0x90 | 0x9b => {
                let a = (words[pc + 1] & 255) as usize;
                let b = (words[pc + 1] >> 8) as usize;
                regs[dest] = regs[a] + regs[b];
                if op == 0x90 {
                    regs[dest] = regs[dest] as i32 as i64;
                }
                pc += 2;
            }
            0xd8 => {
                regs[dest] =
                    regs[(words[pc + 1] & 255) as usize] + (words[pc + 1] as i16 >> 8) as i64;
                pc += 2;
            }
            0x0f | 0x10 => return format!("OK{}|{}", regs[dest], trace.join(",")),
            _ => panic!("oracle opcode {op:x} @{pc}"),
        }
    }
    panic!("oracle exceeded work bound")
}
#[test]
#[ignore = "requires JDK25 javac/java on PATH"]
fn unchanged_source_matches_dex_order_carries_wide_and_constructor_faults() {
    let root = std::env::temp_dir().join(format!("rdx-child-sibling-{}", std::process::id()));
    fs::create_dir_all(root.join("sample")).unwrap();
    let mut cases = String::new();
    let mut count = 0;
    for a in [-1, 0, 2] {
        for b in [-1, 0, 2] {
            for c in [-1, 0, 2] {
                for d in [-1, 0, 2] {
                    for kind in 0..5 {
                        for fail in [0, 1, 3, 8] {
                            for is_error in [false, true] {
                                let inputs = [a, b, c, d];
                                let expected = oracle(kind, inputs, fail, is_error);
                                cases.push_str(&format!(
                                    "{kind}\t{a}\t{b}\t{c}\t{d}\t{fail}\t{}\t{expected}\n",
                                    usize::from(is_error)
                                ));
                                count += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    fs::write(root.join("cases.tsv"), cases).unwrap();
    let java = format!(
        r#"package sample;
class Token {{ Token() {{ Effects.construct(); }} }}
public class Effects {{
 static java.util.List<String> trace=new java.util.ArrayList<>(); static int failAt; static boolean error;
 static final RuntimeException sentinelR=new RuntimeException("sentinel"); static final Error sentinelE=new Error("sentinel");
 static void check() {{ if(failAt!=0&&trace.size()==failAt) {{ if(error) throw sentinelE; throw sentinelR; }} }}
 static void construct() {{trace.add("C");check();}}
 public static int touch(int stage,int i,int j) {{trace.add(stage+":"+i+":"+j);check();return stage*100+i*10+j;}}
 {} {} {} {} {}
 public static void main(String[] args) throws Exception {{int count=0;
 for(String line:java.nio.file.Files.readAllLines(java.nio.file.Path.of("cases.tsv"))) {{String[] f=line.split("\\t",-1);int kind=Integer.parseInt(f[0]),a=Integer.parseInt(f[1]),b=Integer.parseInt(f[2]),c=Integer.parseInt(f[3]),d=Integer.parseInt(f[4]);failAt=Integer.parseInt(f[5]);error=f[6].equals("1");trace.clear();String actual;
 try {{long result=switch(kind) {{case 0->sibling0(a,b,c,d);case 1->sibling1(a,b,c,d);case 2->sibling2(a,b,c,d);case 3->sibling3(a,b,c,d);case 4->sibling4(a,b,c,d);default->throw new AssertionError();}};actual="OK"+result;}}
 catch(Throwable t) {{if(t!=(error?sentinelE:sentinelR))throw new AssertionError("wrong exception identity",t);actual=error?"E":"R";}}
 actual+="|"+String.join(",",trace);if(!actual.equals(f[7]))throw new AssertionError(line+" != "+actual);count++; }}
 System.out.println("DEX/JVM comparisons="+count);}}
}}
"#,
        source(0),
        source(1),
        source(2),
        source(3),
        source(4)
    );
    fs::write(root.join("sample/Effects.java"), &java).unwrap();
    for (program, args) in [
        ("javac", vec!["sample/Effects.java"]),
        ("java", vec!["-Xverify:all", "sample.Effects"]),
    ] {
        let out = Command::new(program)
            .args(args)
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{program}: {}\n{java}",
            String::from_utf8_lossy(&out.stderr)
        );
        println!("{}", String::from_utf8_lossy(&out.stdout));
    }
    assert_eq!(count, 3240);
}
