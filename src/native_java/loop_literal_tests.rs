use super::*;
use crate::native_dex::{DexCode, DexSymbols};
use std::sync::Arc;
fn fixture(
    words: Vec<u16>,
    registers: u16,
    ins: u16,
    parameters: Vec<Arc<str>>,
    ret: &str,
) -> (DexClass, DexMethod) {
    let class = DexClass {
        symbols: Arc::new(DexSymbols::default()),
        descriptor: "Lsample/Example;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![],
    };
    let method = DexMethod {
        declaring_type: class.descriptor.clone(),
        name: "run".into(),
        return_type: ret.into(),
        parameters,
        thrown_types: vec![],
        access_flags: 9,
        code: Some(DexCode {
            registers,
            ins,
            outs: 0,
            tries: 0,
            try_regions: vec![],
            instructions: words,
            offset: 0,
        }),
    };
    (class, method)
}
fn collect() -> serde_json::Value {
    let mut cases: Vec<(String, Vec<u16>, u16, bool)> = vec![];
    let continued = vec![
        0x0012,
        0x0114,
        0x2345,
        0x7fc1,
        0x4035,
        10,
        0x1201,
        0x0112,
        0x2101,
        0x5032,
        (-5i16) as u16,
        0x00d8,
        0x0100,
        0xf728,
        0x010f,
    ];
    let early_continue = vec![
        0x0012,
        0x0114,
        0x2345,
        0x7fc1,
        0x4035,
        10,
        0x1201,
        0x0112,
        0x5032,
        (-4i16) as u16,
        0x2101,
        0x00d8,
        0x0100,
        0xf728,
        0x010f,
    ];
    let broken = vec![
        0x0012, 0x0114, 0x2345, 0x7fc1, 0x4035, 10, 0x1201, 0x0112, 0x2101, 0x5032, 5, 0x00d8,
        0x0100, 0xf728, 0x010f,
    ];
    let early_break = vec![
        0x0012, 0x0114, 0x2345, 0x7fc1, 0x4035, 10, 0x1201, 0x0112, 0x5032, 6, 0x2101, 0x00d8,
        0x0100, 0xf728, 0x010f,
    ];
    for (name, words, legacy) in [
        ("continue_restored", continued, false),
        ("continue_bypass", early_continue, false),
        ("break_restored", broken.clone(), false),
        ("break_restored_legacy", broken, true),
        ("break_bypass", early_break, false),
    ] {
        cases.push((name.into(), words, 6, legacy));
    }
    for changed in [None, Some(0), Some(1), Some(2), Some(3)] {
        let mut words = vec![
            0x0012, 0x0114, 0x2345, 0x7fc1, 0x4035, 0, 0x1201, 0x0112, 0x2101,
        ];
        let mut edges = vec![];
        for index in 0..4 {
            if changed == Some(index) {
                words.push(0x0112);
            }
            let pc = words.len();
            words.extend([if index % 2 == 0 { 0x5032 } else { 0x6032 }, 0]);
            edges.push((pc, index % 2 == 0));
            if changed == Some(index) {
                words.push(0x2101);
            }
        }
        words.extend([0x00d8, 0x0100]);
        let latch = words.len();
        words.push(0x28 | (((4isize - latch as isize) as i8 as u8 as u16) << 8));
        let exit = words.len();
        words.push(0x010f);
        words[5] = (exit - 4) as u16;
        for (pc, continuing) in edges {
            words[pc + 1] =
                (if continuing { 4isize } else { exit as isize } - pc as isize) as i16 as u16;
        }
        cases.push((
            format!("composition_{}", changed.unwrap_or(9)),
            words,
            7,
            false,
        ));
    }
    for restored in [true, false] {
        let mut words = vec![
            0x0012, 0x0114, 0x2345, 0x7fc1, 0x4035, 18, 0x0312, 0x4335, 11, 0x1201, 0x0112,
        ];
        if restored {
            words.extend([0x2101, 0x5332, 6]);
        } else {
            words.extend([0x5332, 7, 0x2101]);
        }
        words.extend([
            0x03d8, 0x0103, 0x0029, 0xfff7, 0x00d8, 0x0100, 0x0029, 0xfff0, 0x010f,
        ]);
        cases.push((format!("nested_{restored}"), words, 6, false));
    }
    let mut rows = vec![];
    for (name, words, count, legacy) in cases {
        let (c, mut m) = fixture(words.clone(), count, 0, vec![], "F");
        let mut regs = vec![None; count as usize];
        for (r, name) in [(4, "limit"), (5, "stop"), (6, "breakAt")] {
            if r < regs.len() {
                assign(
                    &mut regs,
                    r,
                    Value {
                        text: name.into(),
                        ty: "I".into(),
                        literal: None,
                        wide_literal: None,
                        raw_bits32: false,
                    },
                )
                .unwrap();
            }
        }
        let mut g = if legacy {
            let mut g = Graph::new(&words).unwrap();
            g.live = crate::native_java::liveness::analyze(m.code.as_ref().unwrap());
            g
        } else {
            Graph::straight_line(&c, &m).unwrap().unwrap()
        };
        let mut out = Output::default();
        let result = render(
            &c,
            &m,
            &g,
            0,
            words.len(),
            regs.clone(),
            &mut out,
            0,
            true,
            None,
            None,
        );
        let err = result.as_ref().err().map(|e| format!("{e:#}"));
        let mut cache_equal = None;
        if !legacy && result.is_ok() {
            m.code.as_mut().unwrap().instructions.fill(u16::MAX);
            g.targets.fill(Some(usize::MAX));
            let mut poisoned = Output::default();
            let p = render(
                &c,
                &m,
                &g,
                0,
                words.len(),
                regs,
                &mut poisoned,
                0,
                true,
                None,
                None,
            );
            cache_equal = Some(match (p, result) {
                (Ok(p), Ok(result)) => {
                    p == result
                        && poisoned.text == out.text
                        && poisoned
                            .links
                            .iter()
                            .map(|l| (l.start, l.end, &l.label))
                            .eq(out.links.iter().map(|l| (l.start, l.end, &l.label)))
                }
                _ => false,
            });
        }
        rows.push(serde_json::json!({"name":name,"words":words,"registers":count,"body":out.text,"error":err,"cache_equal":cache_equal,"return":"F"}));
    }
    let words = vec![
        0x0438, 8, 0x0018, 0, 0, 0, 0x8000, 0x0628, 0x0018, 0, 0, 0, 0, 0x0010,
    ];
    let (c, m) = fixture(words.clone(), 5, 1, vec!["I".into()], "D");
    let out = reconstruct("sample.Example", &c, &m);
    rows.push(serde_json::json!({"name":"double_zero","words":words,"registers":5,"body":out.as_ref().ok().map(|o|o.text.clone()),"error":out.err().map(|e|e.to_string()),"cache_equal":null,"return":"D"}));
    serde_json::Value::Array(rows)
}
// Independent original DEX PC/register interpreter. A repeated complete machine
// state is nontermination; those inputs are never sent to the JVM.
fn dex_bits(row: &serde_json::Value, args: &[i32]) -> Option<u64> {
    use std::collections::HashSet;
    let words: Vec<u16> = row["words"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap() as u16)
        .collect();
    let mut regs = vec![0i32; row["registers"].as_u64().unwrap() as usize];
    if row["return"] == "D" {
        regs[4] = args[0];
    } else {
        for (&reg, &value) in [4, 5, 6].iter().zip(args) {
            if reg < regs.len() {
                regs[reg] = value;
            }
        }
    }
    let mut pc = 0usize;
    let mut seen = HashSet::new();
    for _ in 0..10_000 {
        if !seen.insert((pc, regs.clone())) {
            return None;
        }
        let word = words[pc];
        let a = (word >> 8) as usize;
        let jump = |offset: i16| usize::try_from(pc as isize + offset as isize).unwrap();
        match word as u8 {
            0x12 => {
                regs[a & 15] = ((a as i8) >> 4) as i32;
                pc += 1;
            }
            0x14 => {
                regs[a] = (words[pc + 1] as u32 | ((words[pc + 2] as u32) << 16)) as i32;
                pc += 3;
            }
            0x18 => {
                regs[a] = (words[pc + 1] as u32 | ((words[pc + 2] as u32) << 16)) as i32;
                regs[a + 1] = (words[pc + 3] as u32 | ((words[pc + 4] as u32) << 16)) as i32;
                pc += 5;
            }
            0x01 => {
                regs[a & 15] = regs[a >> 4];
                pc += 1;
            }
            0x32 | 0x35 => {
                let yes = if word as u8 == 0x32 {
                    regs[a & 15] == regs[a >> 4]
                } else {
                    regs[a & 15] >= regs[a >> 4]
                };
                pc = if yes {
                    jump(words[pc + 1] as i16)
                } else {
                    pc + 2
                };
            }
            0x38 => {
                pc = if regs[a] == 0 {
                    jump(words[pc + 1] as i16)
                } else {
                    pc + 2
                };
            }
            0x28 => pc = jump((word >> 8) as i8 as i16),
            0x29 => pc = jump(words[pc + 1] as i16),
            0xd8 => {
                let operand = words[pc + 1];
                regs[a] = regs[(operand & 255) as usize].wrapping_add((operand >> 8) as i8 as i32);
                pc += 2;
            }
            0x0f => return Some(regs[a] as u32 as u64),
            0x10 => return Some(regs[a] as u32 as u64 | ((regs[a + 1] as u32 as u64) << 32)),
            op => panic!("unsupported oracle opcode {op:x} at {pc}"),
        }
    }
    panic!("oracle step budget exceeded");
}
#[test]
fn original_loop_literal_paths_distinguish_nan_zero_and_signed_zero() {
    let rows = collect();
    let rows = rows.as_array().unwrap();
    for row in rows {
        assert!(row["error"].is_null(), "{}", row);
        if !row["cache_equal"].is_null() {
            assert_eq!(row["cache_equal"], true, "{}", row["name"]);
        }
    }
    let get = |name: &str| rows.iter().find(|r| r["name"] == name).unwrap();
    assert_eq!(dex_bits(get("continue_bypass"), &[3, 1, 2]), None);
    assert_eq!(
        dex_bits(get("break_restored"), &[3, 1, 2]),
        Some(0x7fc12345)
    );
    assert_eq!(dex_bits(get("break_bypass"), &[3, 1, 2]), Some(0));
    assert_eq!(dex_bits(get("break_bypass"), &[3, 7, 2]), Some(0x7fc12345));
    assert_eq!(dex_bits(get("composition_1"), &[3, 7, 1]), Some(0));
    assert_eq!(dex_bits(get("nested_false"), &[3, 1, 2]), Some(0));
    assert_eq!(dex_bits(get("nested_false"), &[3, 7, 2]), Some(0x7fc12345));
    assert_eq!(dex_bits(get("double_zero"), &[0]), Some(0));
    assert_eq!(dex_bits(get("double_zero"), &[1]), Some(0x8000000000000000));
}
#[test]
#[ignore = "requires RDX_JAVA25_HOME"]
fn unchanged_loop_literal_java_preserves_all_finite_path_raw_bits() {
    use std::{fs, process::Command};
    let home = std::env::var("RDX_JAVA25_HOME").unwrap();
    let rows = collect();
    let rows = rows.as_array().unwrap();
    let mut java = String::from("public class Run {\n");
    let mut cases = String::new();
    let mut count = 0;
    for row in rows {
        assert!(row["error"].is_null(), "{}", row);
        let name = row["name"].as_str().unwrap();
        let double = row["return"] == "D";
        let ret = if double { "double" } else { "float" };
        let params = if double {
            "int p0"
        } else {
            "int limit, int stop, int breakAt"
        };
        java.push_str(&format!(
            "public static {ret} {name}({params}) {{\n{}\n}}\n",
            row["body"].as_str().unwrap()
        ));
        let mut inputs = Vec::new();
        if double {
            inputs.extend([i32::MIN, -1, 0, 1, i32::MAX].map(|v| vec![v]));
        } else {
            for limit in -1..6 {
                for stop in -1..8 {
                    for break_at in -1..8 {
                        inputs.push(vec![limit, stop, break_at]);
                    }
                }
            }
        }
        for args in inputs {
            if let Some(bits) = dex_bits(row, &args) {
                let args = args
                    .iter()
                    .map(i32::to_string)
                    .collect::<Vec<_>>()
                    .join(",");
                cases.push_str(&format!(
                    "{name}\t{}\t{args}\t{bits:x}\n",
                    row["return"].as_str().unwrap()
                ));
                count += 1;
            }
        }
    }
    assert_eq!(count, 5964);
    java.push_str(r#"public static void main(String[] args) throws Exception {
      int count=0;
      for(String line:java.nio.file.Files.readAllLines(java.nio.file.Path.of(args[0]))) {
        String[] parts=line.split("\t"); String[] inputs=parts[2].split(",");
        Class<?>[] types=new Class<?>[inputs.length];Object[] vals=new Object[inputs.length];
        for(int i=0;i<inputs.length;i++){types[i]=int.class;vals[i]=Integer.valueOf(inputs[i]);}
        Object result=Run.class.getDeclaredMethod(parts[0],types).invoke(null,vals);
        long bits=parts[1].equals("D")?Double.doubleToRawLongBits((Double)result):Integer.toUnsignedLong(Float.floatToRawIntBits((Float)result));
        if(bits!=Long.parseUnsignedLong(parts[3],16))throw new AssertionError(line+" actual="+Long.toUnsignedString(bits,16));
        count++;
      }if(count!=5964)throw new AssertionError(count);
    }}"#);
    let dir = std::env::temp_dir().join(format!("rdx-loop-literal-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("Run.java"), java).unwrap();
    fs::write(dir.join("cases.tsv"), cases).unwrap();
    for (program, args) in [
        ("javac", vec!["Run.java"]),
        ("java", vec!["-Xverify:all", "-cp", ".", "Run", "cases.tsv"]),
    ] {
        let output = Command::new(format!("{home}/bin/{program}"))
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
