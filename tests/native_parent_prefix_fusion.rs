use rdx::{
    native_cfg,
    native_dex::{self, DexClass, DexSymbols},
    native_dominators, native_ir, native_java,
};
use std::{collections::BTreeMap, fs, process::Command, sync::Arc};
#[path = "../src/native_java/parent_prefix_fusion.rs"]
mod parent_prefix_fusion;
#[derive(Default)]
struct Assembler {
    words: Vec<u16>,
    labels: BTreeMap<&'static str, usize>,
    fixups: Vec<(usize, &'static str)>,
}
impl Assembler {
    fn label(&mut self, s: &'static str) {
        assert!(self.labels.insert(s, self.words.len()).is_none());
    }
    fn words(&mut self, w: &[u16]) {
        self.words.extend_from_slice(w);
    }
    fn branch(&mut self, w: u16, to: &'static str) {
        self.fixups.push((self.words.len(), to));
        self.words(&[w, 0]);
    }
    fn finish(mut self) -> (Vec<u16>, BTreeMap<&'static str, usize>) {
        for (pc, s) in self.fixups {
            self.words[pc + 1] = (self.labels[s] as isize - pc as isize) as i16 as u16;
        }
        (self.words, self.labels)
    }
}
fn fixture(reference: bool) -> (DexClass, BTreeMap<&'static str, usize>) {
    fixture_with_gap(reference, false)
}
fn fixture_with_gap(reference: bool, gap: bool) -> (DexClass, BTreeMap<&'static str, usize>) {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class.methods.retain(|m| m.name.as_ref() == "answer");
    class.symbols = Arc::new(DexSymbols {
        strings: vec!["prefix".into(), "touch".into(), "rotate".into()],
        types: vec!["Lsample/Effects;".into()],
        protos: vec![
            (
                "Ljava/lang/Object;".into(),
                vec!["I".into(), "I".into(), "Ljava/lang/Object;".into()],
            ),
            (
                "J".into(),
                vec!["I".into(), "I".into(), "Ljava/lang/Object;".into()],
            ),
            (
                "Ljava/lang/Object;".into(),
                vec!["Ljava/lang/Object;".into()],
            ),
        ],
        methods: vec![(0, 0, 0), (0, 1, 1), (0, 2, 2)],
        ..Default::default()
    });
    let m = &mut class.methods[0];
    m.name = if reference { "refs" } else { "wide" }.into();
    m.access_flags = 9;
    m.parameters = vec![
        "I".into(),
        "I".into(),
        "I".into(),
        "Ljava/lang/Object;".into(),
        "J".into(),
    ];
    m.return_type = if reference { "Ljava/lang/Object;" } else { "J" }.into();
    let c = m.code.as_mut().unwrap();
    c.registers = 14;
    c.ins = 6;
    c.outs = 3;
    c.tries = 0;
    c.try_regions.clear();
    let mut a = Assembler::default();
    a.words(&[0x0012, 0x0112, 0xc204, 0xb607]);
    if gap {
        a.branch(0x0a3a, "unownedEffectGap");
    }
    a.label("parent");
    a.words(&[0x3071, 0, 0x0610]);
    a.label("prefixResult");
    a.words(&[0x060c]);
    a.label("guard");
    a.branch(0x8035, "exit");
    a.words(&[0x00d8, 0x0100, 0x0112]);
    a.label("child");
    a.branch(0x9135, "parent");
    a.label("body");
    a.words(&[
        0x3071, 1, 0x0610, 0x040b, 0x029b, 0x0402, 0x1071, 2, 6, 0x060c, 0x01d8, 0x0101,
    ]);
    a.label("bodyContinue");
    a.branch(0xa032, "parent");
    a.branch(0xa132, "early");
    a.label("latch");
    a.branch(0x29, "child");
    a.label("early");
    if gap {
        a.branch(0x29, "exit");
        a.label("unownedEffectGap");
        a.words(&[0x3071, 1, 0x0610, if reference { 0x0611 } else { 0x0210 }]);
    } else {
        a.words(&[if reference { 0x0611 } else { 0x0210 }]);
    }
    a.label("exit");
    a.words(&[if reference { 0x0611 } else { 0x0210 }]);
    let (w, l) = a.finish();
    c.instructions = w;
    (class, l)
}
#[test]
fn exact_edges_bounded_growth_and_metadata_reproof() {
    for reference in [false, true] {
        let (c, l) = fixture(reference);
        let code = c.methods[0].code.as_ref().unwrap();
        let p = parent_prefix_fusion::select(code).unwrap();
        assert_eq!(p.parent_header, l["parent"]);
        assert_eq!(p.child_header, l["child"]);
        assert_eq!(p.reentries.len(), 2);
        assert!(parent_prefix_fusion::validate(code, &p));
        for n in 0..9 {
            let mut poison = p.clone();
            match n {
                0 => poison.words[0] ^= 0x100,
                1 => poison.registers += 1,
                2 => poison.ins -= 1,
                3 => poison.prefix.pop().map(|_| ()).unwrap(),
                4 => poison.edges.pop().map(|_| ()).unwrap(),
                5 => poison.widths[0].1 += 1,
                6 => poison.targets[0].1 = Some(l["child"]),
                7 => poison.reentries.clear(),
                _ => poison.terminal_edges.clear(),
            };
            assert!(!parent_prefix_fusion::validate(code, &poison), "poison{n}");
        }
    }
}
#[test]
fn middle_entries_cycles_pending_result_allocation_and_handlers_reject() {
    for kind in 0..5 {
        let (mut c, l) = fixture(false);
        let code = c.methods[0].code.as_mut().unwrap();
        match kind {
            0 => {
                let pc = l["bodyContinue"];
                code.instructions[pc + 1] =
                    (l["prefixResult"] as isize - pc as isize) as i16 as u16;
            }
            1 => {
                let pc = l["guard"];
                code.instructions[pc + 1] = (l["parent"] as isize - pc as isize) as i16 as u16;
            }
            2 => {
                let pc = l["bodyContinue"];
                code.instructions[pc + 1] = (l["guard"] as isize - pc as isize) as i16 as u16;
            }
            3 => {
                code.instructions[l["body"]] = 0x0622;
            }
            _ => code.tries = 1,
        };
        assert!(
            parent_prefix_fusion::select(code).is_none(),
            "negative{kind}"
        );
    }
}
#[test]
fn actual_source_has_one_loop_and_edge_specific_copies() {
    for r in [false, true] {
        let (c, _) = fixture(r);
        let s = native_java::render_method("sample.Effects", &c, &c.methods[0])
            .unwrap()
            .source;
        assert_eq!(s.matches("while (").count(), 1, "{s}");
        assert_eq!(s.matches("sample.Effects.prefix(").count(), 3, "{s}");
        assert!(!s.contains("switch ("), "{s}");
    }
}
// Independent DEX-word interpreter. Object identities are stable integer tokens;
// every invoke is driven by the encoded index and packed register operands.
fn oracle(reference: bool, args: [i64; 5], fault: usize, error: bool) -> String {
    let (c, _) = fixture(reference);
    let w = &c.methods[0].code.as_ref().unwrap().instructions;
    let mut r = [0i64; 14];
    r[8] = args[0];
    r[9] = args[1];
    r[10] = args[2];
    r[11] = args[3];
    r[12] = args[4];
    let mut pc = 0;
    let mut pending = 0;
    let mut trace = Vec::new();
    for _ in 0..10000 {
        let word = w[pc];
        let op = word as u8;
        let dst = (word >> 8) as usize;
        match op {
            0x12 => {
                r[(word >> 8 & 15) as usize] = ((word as i16) >> 12) as i64;
                pc += 1
            }
            0x04 | 0x07 => {
                r[(word >> 8 & 15) as usize] = r[(word >> 12) as usize];
                pc += 1
            }
            0x35 | 0x32 => {
                let a = r[(word >> 8 & 15) as usize];
                let b = r[(word >> 12) as usize];
                if if op == 0x35 { a >= b } else { a == b } {
                    pc = (pc as isize + w[pc + 1] as i16 as isize) as usize
                } else {
                    pc += 2
                }
            }
            0x29 => pc = (pc as isize + w[pc + 1] as i16 as isize) as usize,
            0x71 => {
                let operands = w[pc + 2];
                let a = r[(operands & 15) as usize];
                let b = r[(operands >> 4 & 15) as usize];
                let v = r[(operands >> 8 & 15) as usize];
                match w[pc + 1] {
                    0 => {
                        trace.push(format!("P{a}:{b}:{v}"));
                        pending = (v + 1) % 3;
                    }
                    1 => {
                        trace.push(format!("T{a}:{b}:{v}"));
                        pending = a * 100 + b * 10 + v;
                    }
                    2 => {
                        trace.push(format!("R{a}"));
                        pending = (a + 1) % 3;
                    }
                    _ => panic!(),
                };
                if fault != 0 && trace.len() == fault {
                    return format!("{}|{}", if error { "E" } else { "R" }, trace.join(","));
                }
                pc += 3
            }
            0x0b | 0x0c => {
                r[dst] = pending;
                pc += 1
            }
            0x9b => {
                r[dst] = r[(w[pc + 1] & 255) as usize].wrapping_add(r[(w[pc + 1] >> 8) as usize]);
                pc += 2
            }
            0xd8 => {
                r[dst] = (r[(w[pc + 1] & 255) as usize] as i32)
                    .wrapping_add((w[pc + 1] as i16 >> 8) as i32) as i64;
                pc += 2
            }
            0x10 | 0x11 => return format!("OK{}|{}", r[dst], trace.join(",")),
            _ => panic!("opcode {op:x}@{pc}"),
        }
    }
    panic!("oracle work bound")
}
#[test]
#[ignore = "requires JDK25 on PATH"]
fn unchanged_source_wide_reference_zero_iteration_and_competing_edges_match_dex() {
    let dir = std::env::temp_dir().join(format!("rdx-parent-prefix-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let mut cases = String::new();
    let mut count = 0;
    for outer in [-1, 0, 1, 3] {
        for inner in [-1, 0, 1, 3] {
            for stop in [-1, 1, 2] {
                for id in 0..3 {
                    for fault in [0, 1, 2, 3, 5, 8, 12] {
                        for error in [false, true] {
                            for reference in [false, true] {
                                let args = [outer, inner, stop, id, 5_000_000_000];
                                cases.push_str(&format!(
                                    "{}\t{outer}\t{inner}\t{stop}\t{id}\t{fault}\t{}\t{}\n",
                                    usize::from(reference),
                                    usize::from(error),
                                    oracle(reference, args, fault, error)
                                ));
                                count += 1
                            }
                        }
                    }
                }
            }
        }
    }
    fs::write(dir.join("cases.tsv"), cases).unwrap();
    let source = |reference| {
        let (c, _) = fixture(reference);
        native_java::render_method("sample.Effects", &c, &c.methods[0])
            .unwrap()
            .source
    };
    let java = format!(
        r#"package sample;
public class Effects {{
static Object[] tokens={{new Object(),new Object(),new Object()}};static java.util.List<String> trace=new java.util.ArrayList<>();static int fault;static boolean error;static RuntimeException re=new RuntimeException();static Error er=new Error();
static int id(Object o){{for(int i=0;i<tokens.length;i++)if(o==tokens[i])return i;throw new AssertionError("unknown identity");}}
static void check(){{if(fault!=0&&trace.size()==fault){{if(error)throw er;throw re;}}}}
static Object prefix(int i,int j,Object o){{int n=id(o);trace.add("P"+i+":"+j+":"+n);check();return tokens[(n+1)%3];}}
static long touch(int i,int j,Object o){{int n=id(o);trace.add("T"+i+":"+j+":"+n);check();return i*100+j*10+n;}}
static Object rotate(Object o){{int n=id(o);trace.add("R"+n);check();return tokens[(n+1)%3];}}
{} {}
public static void main(String[] args)throws Exception{{int count=0;for(String line:java.nio.file.Files.readAllLines(java.nio.file.Path.of("cases.tsv"))){{String[] f=line.split("\\t",-1);boolean reference=f[0].equals("1");int a=Integer.parseInt(f[1]),b=Integer.parseInt(f[2]),c=Integer.parseInt(f[3]),n=Integer.parseInt(f[4]);fault=Integer.parseInt(f[5]);error=f[6].equals("1");trace.clear();String actual;try{{actual="OK"+(reference?id(refs(a,b,c,tokens[n],5000000000L)):wide(a,b,c,tokens[n],5000000000L));}}catch(Throwable t){{if(t!=(error?er:re))throw new AssertionError("identity",t);actual=error?"E":"R";}}actual+="|"+String.join(",",trace);if(!actual.equals(f[7]))throw new AssertionError(line+" != "+actual);count++;}}System.out.println("DEX/JVM comparisons="+count);}}
}}
"#,
        source(false),
        source(true)
    );
    fs::write(dir.join("sample/Effects.java"), java).unwrap();
    let javac = Command::new("javac")
        .arg("sample/Effects.java")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        javac.status.success(),
        "{}",
        String::from_utf8_lossy(&javac.stderr)
    );
    let java = Command::new("java")
        .args(["-Xverify:all", "-cp", ".", "sample.Effects"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        java.status.success(),
        "{}",
        String::from_utf8_lossy(&java.stderr)
    );
    assert!(
        String::from_utf8_lossy(&java.stdout).contains(&format!("DEX/JVM comparisons={count}"))
    );
    println!("{}", String::from_utf8_lossy(&java.stdout));
}
fn remove_all_fixture() -> rdx::native_engine::NativeDexEngine {
    use rdx::{engine::DecompilerEngine, native_engine::NativeDexEngine};
    let mut e = NativeDexEngine::default();
    e.open(std::path::Path::new(
        &std::env::var("RDX_TEST_APK").expect("set RDX_TEST_APK to the corpus APK"),
    ))
    .unwrap();
    e
}
fn remove_oracle(
    c: &DexClass,
    len: usize,
    mask: usize,
    fault: usize,
    error: bool,
    null: bool,
) -> String {
    let m = c
        .methods
        .iter()
        .find(|m| m.name.as_ref() == "removeAll")
        .unwrap();
    let w = &m.code.as_ref().unwrap().instructions;
    let mut r = [0i64; 5];
    r[3] = 900;
    r[4] = if null { 0 } else { 901 };
    let mut pc = 0;
    let mut pending = 0;
    let mut next = 0;
    let mut trace = Vec::new();
    for _ in 0..1000 {
        let word = w[pc];
        let op = word as u8;
        match op {
            0x01 => {
                r[(word >> 8 & 15) as usize] = r[(word >> 12) as usize];
                pc += 1
            }
            0x12 => {
                r[(word >> 8 & 15) as usize] = ((word as i16) >> 12) as i64;
                pc += 1
            }
            0x0a | 0x0c => {
                r[(word >> 8) as usize] = pending;
                pc += 1
            }
            0x38 | 0x39 => {
                if (r[(word >> 8) as usize] == 0) == (op == 0x38) {
                    pc = (pc as isize + w[pc + 1] as i16 as isize) as usize
                } else {
                    pc += 2
                }
            }
            0x28 => pc = (pc as isize + (word >> 8) as i8 as isize) as usize,
            0x6e | 0x72 => {
                let idx = w[pc + 1] as usize;
                let name = &c.symbols.strings[c.symbols.methods[idx].2 as usize];
                let receiver = r[(w[pc + 2] & 15) as usize];
                if receiver == 0 {
                    return format!("N|{}", trace.join(","));
                }
                match name.as_ref() {
                    "iterator" => {
                        assert_eq!(receiver, 901);
                        trace.push("I".to_owned());
                        pending = 902
                    }
                    "hasNext" => {
                        assert_eq!(receiver, 902);
                        trace.push(format!("H{next}"));
                        pending = i64::from(next < len)
                    }
                    "next" => {
                        assert_eq!(receiver, 902);
                        trace.push(format!("N{next}"));
                        pending = next as i64;
                        next += 1
                    }
                    "remove" => {
                        assert_eq!(receiver, 900);
                        let item = r[(w[pc + 2] >> 4 & 15) as usize] as usize;
                        trace.push(format!("R{item}"));
                        pending = i64::from(mask & (1 << item) != 0)
                    }
                    other => panic!("unexpected invoke {other}"),
                };
                if fault != 0 && trace.len() == fault {
                    return format!("{}|{}", if error { "E" } else { "R" }, trace.join(","));
                }
                pc += 3
            }
            0x0f => return format!("OK{}|{}", r[(word >> 8) as usize], trace.join(",")),
            _ => panic!("remove oracle {op:x}@{pc}"),
        }
    }
    panic!("remove oracle bound")
}
#[test]
#[ignore = "requires JDK25 and pinned APK"]
fn actual_remove_all_matches_original_dex_sticky_result_and_iterator_faults() {
    let e = remove_all_fixture();
    let c = e.class("l3.d0").unwrap();
    let m = c
        .methods
        .iter()
        .find(|m| m.name.as_ref() == "removeAll")
        .unwrap();
    assert!(parent_prefix_fusion::select(m.code.as_ref().unwrap()).is_some());
    let source = native_java::render_method("l3.d0", c, m).unwrap().source;
    assert_eq!(source.matches("while (").count(), 1);
    let dir = std::env::temp_dir().join(format!("rdx-fusion-remove-all-{}", std::process::id()));
    fs::create_dir_all(dir.join("l3")).unwrap();
    let mut cases = String::new();
    let mut count = 0;
    for len in 0..=5 {
        for mask in 0..(1 << len) {
            for fault in [0, 1, 2, 3, 6, 12, 18] {
                for error in [false, true] {
                    for null in [false, true] {
                        let expected = remove_oracle(c, len, mask, fault, error, null);
                        cases.push_str(&format!(
                            "{len}\t{mask}\t{fault}\t{}\t{}\t{expected}\n",
                            usize::from(error),
                            usize::from(null)
                        ));
                        count += 1
                    }
                }
            }
        }
    }
    fs::write(dir.join("cases.tsv"), cases).unwrap();
    let java = format!(
        r#"package l3;
public class d0 {{static java.util.List<String> trace=new java.util.ArrayList<>();static int mask,fault;static boolean error;static RuntimeException re=new RuntimeException();static Error er=new Error();static void check(){{if(fault!=0&&trace.size()==fault){{if(error)throw er;throw re;}}}}
public boolean remove(Object o){{int n=(Integer)o;trace.add("R"+n);check();return (mask&(1<<n))!=0;}}
{}
static class Input extends java.util.AbstractCollection<Object> {{int length;Input(int n){{length=n;}}public int size(){{return length;}}public java.util.Iterator<Object> iterator(){{trace.add("I");check();return new java.util.Iterator<Object>(){{int next;public boolean hasNext(){{trace.add("H"+next);check();return next<length;}}public Object next(){{trace.add("N"+next);check();return next++;}}}};}}}}
public static void main(String[]args)throws Exception{{int count=0;for(String line:java.nio.file.Files.readAllLines(java.nio.file.Path.of("cases.tsv"))){{String[] f=line.split("\\t",-1);int len=Integer.parseInt(f[0]);mask=Integer.parseInt(f[1]);fault=Integer.parseInt(f[2]);error=f[3].equals("1");boolean nul=f[4].equals("1");trace.clear();String actual;try{{actual="OK"+(new d0().removeAll(nul?null:new Input(len))?1:0);}}catch(Throwable t){{if(nul){{if(!(t instanceof NullPointerException))throw new AssertionError(t);actual="N";}}else{{if(t!=(error?er:re))throw new AssertionError("identity",t);actual=error?"E":"R";}}}}actual+="|"+String.join(",",trace);if(!actual.equals(f[5]))throw new AssertionError(line+" != "+actual);count++;}}System.out.println("actual removeAll DEX/JVM comparisons="+count);}}
}}
"#,
        source
    );
    fs::write(dir.join("l3/d0.java"), java).unwrap();
    let javac = Command::new("javac")
        .arg("l3/d0.java")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        javac.status.success(),
        "{}",
        String::from_utf8_lossy(&javac.stderr)
    );
    let java = Command::new("java")
        .args(["-Xverify:all", "-cp", ".", "l3.d0"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        java.status.success(),
        "{}",
        String::from_utf8_lossy(&java.stderr)
    );
    assert!(
        String::from_utf8_lossy(&java.stdout)
            .contains(&format!("actual removeAll DEX/JVM comparisons={count}"))
    );
    println!("{}", String::from_utf8_lossy(&java.stdout));
}

#[test]
fn undefined_wide_head_and_result_type_changes_fail_typed_emission() {
    let (mut c, _) = fixture(false);
    c.methods[0].code.as_mut().unwrap().instructions[2] = 0xc201;
    assert!(native_java::render_method("sample.Effects", &c, &c.methods[0]).is_err());
    let (mut c, _) = fixture(false);
    Arc::get_mut(&mut c.symbols).unwrap().protos[1].0 = "I".into();
    assert!(native_java::render_method("sample.Effects", &c, &c.methods[0]).is_err());
}

#[test]
fn terminal_holes_do_not_absorb_unowned_effects() {
    for reference in [false, true] {
        let (c, l) = fixture_with_gap(reference, true);
        let code = c.methods[0].code.as_ref().unwrap();
        let cfg = native_cfg::ControlFlowGraph::build(code).unwrap();
        assert!(cfg.block_at.contains_key(&l["unownedEffectGap"]));
        assert_eq!(code.instructions[l["unownedEffectGap"]] as u8, 0x71);
        assert!(
            parent_prefix_fusion::select(code).is_none(),
            "a terminal goto cannot skip a physically unowned effectful gap"
        );
    }
}
