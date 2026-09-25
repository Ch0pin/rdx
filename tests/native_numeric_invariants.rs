//! Narrow numeric proofs: loop invariants and closed boolean bitwise domains.
use rdx::{native_dex, native_java};
use std::sync::Arc;

fn render(
    name: &str,
    words: Vec<u16>,
    registers: u16,
    params: &[&str],
    ret: &str,
) -> anyhow::Result<String> {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))?
        .classes
        .remove(0);
    class.methods.retain(|m| m.name.as_ref() == "answer");
    let m = &mut class.methods[0];
    m.name = name.into();
    m.access_flags = 9;
    m.parameters = params.iter().map(|p| Arc::from(*p)).collect();
    m.return_type = ret.into();
    let code = m.code.as_mut().unwrap();
    code.registers = registers;
    code.ins = params.len() as u16;
    code.instructions = words;
    Ok(native_java::render_method("sample.Hello", &class, &class.methods[0])?.source)
}
fn float_loop(bits: u32) -> String {
    render(
        "invariant",
        vec![
            0x0014,
            bits as u16,
            (bits >> 16) as u16,
            0x0112,
            0x4132,
            8,
            0x0201,
            0x0012,
            0x2001,
            0x01d8,
            0x0101,
            0xf928,
            0x000f,
        ],
        5,
        &["I"],
        "F",
    )
    .unwrap()
}
fn float_branch(a: u32, b: u32) -> String {
    render(
        "selected",
        vec![
            0x0138,
            6,
            0x0014,
            a as u16,
            (a >> 16) as u16,
            0x0428,
            0x0014,
            b as u16,
            (b >> 16) as u16,
            0x000f,
        ],
        2,
        &["Z"],
        "F",
    )
    .unwrap()
}
#[test]
fn raw_constant_joins_support_float_but_typed_int_joins_do_not() {
    for (a, b) in [(0, 1), (0x80000000, 0), (0x3f800000, 0x7fc12345)] {
        let source = float_branch(a, b);
        assert!(source.contains("Float.intBitsToFloat"), "{source}");
    }
    assert!(
        render(
            "mixedTyped",
            vec![0x0138, 6, 0x0014, 0, 0x3f80, 0x0228, 0x2001, 0x000f],
            3,
            &["Z", "I"],
            "F"
        )
        .is_err()
    );
}
fn boolean_binary(op: u16) -> String {
    render("booleanOp", vec![op, 0x0201, 0x000f], 3, &["Z", "Z"], "Z").unwrap()
}
fn boolean_mask() -> String {
    render("masked", vec![0x00dd, 0x0101, 0x000f], 2, &["I"], "Z").unwrap()
}
fn numeric_boolean_join() -> String {
    render(
        "mixed",
        vec![0x0238, 5, 0x00dd, 0x0101, 0x0228, 0x7012, 0x000f],
        3,
        &["I", "Z"],
        "I",
    )
    .unwrap()
}
#[test]
fn restored_loop_float_literals_retain_raw_bits() {
    for bits in [0, 1, 0x80000000, 0x3fc00000, 0x7f800000, 0x7fc12345] {
        let source = float_loop(bits);
        assert!(source.contains("while (true)"), "{source}");
        assert!(source.contains("float invariant("), "{source}");
    }
}
#[test]
fn arbitrary_integer_computation_does_not_become_float_bits() {
    assert!(render("typed", vec![0x000f], 1, &["I"], "F").is_err());
    assert!(
        render(
            "changed",
            vec![
                0x0014, 0, 0x3f80, 0x0112, 0x2132, 7, 0x00d8, 0x0100, 0x01d8, 0x0101, 0xfa28,
                0x000f
            ],
            3,
            &["I"],
            "F"
        )
        .is_err()
    );
}
#[test]
fn bitwise_boolean_domains_and_mixed_integer_join_reconstruct() {
    for op in [0x0095, 0x0096, 0x0097] {
        boolean_binary(op);
    }
    assert!(boolean_mask().contains("!= 0"));
    assert!(numeric_boolean_join().contains("? 1 : 0"));
    assert!(render("notBoolean", vec![0x000f], 1, &["I"], "Z").is_err());
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn numeric_invariants_match_jvm_raw_bits_and_truth_tables() {
    use std::{fs, process::Command};
    let dir = std::env::temp_dir().join(format!("rdx-numeric-invariants-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let mut source = String::from("public class NumericInvariants {\n");
    let patterns = [0u32, 1, 0x80000000, 0x3fc00000, 0x7f800000, 0x7fc12345];
    for (i, bits) in patterns.iter().enumerate() {
        source.push_str(&float_loop(*bits).replace("invariant(", &format!("bits{i}(")));
    }
    for (name, op) in [("and", 0x0095), ("or", 0x0096), ("xor", 0x0097)] {
        source.push_str(&boolean_binary(op).replace("booleanOp(", &format!("{name}(")));
    }
    for (i, bits) in patterns.iter().enumerate() {
        source.push_str(&float_branch(*bits, 1).replace("selected(", &format!("selected{i}(")));
    }
    source.push_str(&boolean_mask());
    source.push_str(&numeric_boolean_join());
    source.push_str("public static void main(String[] args) {\n");
    for (i, bits) in patterns.iter().enumerate() {
        source.push_str(&format!("for(int n=0;n<8;n++) if(Float.floatToRawIntBits(bits{i}(n)) != {}) throw new AssertionError(\"bits{i}\");\n", *bits as i32));
    }
    for (i, bits) in patterns.iter().enumerate() {
        source.push_str(&format!("if(Float.floatToRawIntBits(selected{i}(true)) != {} || Float.floatToRawIntBits(selected{i}(false)) != 1) throw new AssertionError(\"select{i}\");\n", *bits as i32));
    }
    source.push_str("for(boolean a:new boolean[]{false,true}) for(boolean b:new boolean[]{false,true}) { if(and(a,b)!=(a&b)||or(a,b)!=(a|b)||xor(a,b)!=(a^b)) throw new AssertionError(\"boolean\"); }\n");
    source.push_str("for(int n:new int[]{Integer.MIN_VALUE,-3,-2,-1,0,1,2,3,Integer.MAX_VALUE}) { if(masked(n)!=((n&1)!=0)) throw new AssertionError(\"mask\"); if(mixed(n,true)!=(n&1)||mixed(n,false)!=7) throw new AssertionError(\"mixed\"); }\n}}\n");
    fs::write(dir.join("NumericInvariants.java"), &source).unwrap();
    for (program, arg) in [
        ("javac", "NumericInvariants.java"),
        ("java", "NumericInvariants"),
    ] {
        let output = Command::new(program)
            .arg(arg)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{program}: {}\n{source}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
