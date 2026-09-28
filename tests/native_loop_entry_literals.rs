use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_java,
};
use std::{fs, process::Command, sync::Arc};

fn fixture(words: Vec<u16>, ret: &str, symbols: DexSymbols) -> DexClass {
    let descriptor: Arc<str> = "Lsample/LoopEntry;".into();
    DexClass {
        symbols: Arc::new(symbols),
        descriptor: descriptor.clone(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![DexMethod {
            declaring_type: descriptor,
            name: "choose".into(),
            return_type: ret.into(),
            parameters: vec!["I".into()],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 3,
                ins: 1,
                outs: 1,
                tries: 0,
                try_regions: vec![],
                instructions: words,
                offset: 0,
            }),
        }],
    }
}

fn source(class: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.LoopEntry", class, &class.methods[0])?.source)
}

fn boolean_fixture() -> DexClass {
    fixture(
        vec![
            0x0012, // untyped zero in v0
            0x0238, 9, // if count == 0, return
            0x0071, 0, 0,      // flag()Z
            0x000a, // move-result v0
            0x02d8, 0xff02, // count--
            0xf828, // goto header
            0x000f, // return v0
        ],
        "Z",
        DexSymbols {
            types: vec!["Lsample/LoopEntry;".into()],
            strings: vec!["flag".into()],
            protos: vec![("Z".into(), vec![])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        },
    )
}

fn float_fixture() -> DexClass {
    fixture(
        vec![
            0x0012, // untyped +0 float bits in v0
            0x0238, 6,      // if count == 0, return
            0x2082, // int-to-float v0, v2
            0x02d8, 0xff02, // count--
            0xfb28, // goto header
            0x000f, // return v0
        ],
        "F",
        DexSymbols::default(),
    )
}

fn reference_fixture() -> DexClass {
    fixture(
        vec![
            0x0012, // untyped zero in v0
            0x0238, 9, // if count == 0, return
            0x0071, 0, 0,      // next()Ljava/lang/String;
            0x000c, // move-result-object v0
            0x02d8, 0xff02, // count--
            0xf828, // goto header
            0x0011, // return-object v0
        ],
        "Ljava/lang/String;",
        DexSymbols {
            types: vec!["Lsample/LoopEntry;".into()],
            strings: vec!["next".into()],
            protos: vec![("Ljava/lang/String;".into(), vec![])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        },
    )
}

fn reused_register_fixture() -> DexClass {
    fixture(
        vec![
            0x0012, // null v0 for the first loop
            0x0238, 12, // exit first loop when count is zero
            0x1071, 0, 0, // consume(v0) requires a String
            0x0071, 1, 0,      // next() returns a String
            0x000c, // move-result-object v0
            0x02d8, 0xff02, // count--
            0xf528, // goto first header
            0x1212, // count = 1
            0x0012, // reuse v0 as an integer zero
            0x0238, 7, // exit second loop when count is zero
            0x00d8, 0x0100, // v0++
            0x02d8, 0xff02, // count--
            0xfa28, // goto second header
            0x000f, // return integer v0
        ],
        "I",
        DexSymbols {
            types: vec!["Lsample/LoopEntry;".into(), "Ljava/lang/String;".into()],
            strings: vec!["consume".into(), "next".into()],
            protos: vec![
                ("V".into(), vec!["Ljava/lang/String;".into()]),
                ("Ljava/lang/String;".into(), vec![]),
            ],
            methods: vec![(0, 0, 0), (0, 1, 1)],
            ..Default::default()
        },
    )
}

fn exit_only_reference_fixture() -> DexClass {
    let mut class = fixture(
        vec![
            0x0012, // null v0
            0x1112, // one in v1
            0x0338, 12, // zero iterations return null
            0x1333, 7, // count != one: take the backedge path
            0x0071, 0, 0,      // next() only on the exit path
            0x000c, // move-result-object v0
            0x0428, // goto exit
            0x03d8, 0xff03, // count-- on the backedge path
            0xf528, // goto header
            0x0011, // return-object v0
        ],
        "Ljava/lang/String;",
        DexSymbols {
            types: vec!["Lsample/LoopEntry;".into()],
            strings: vec!["next".into()],
            protos: vec![("Ljava/lang/String;".into(), vec![])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        },
    );
    class.methods[0].code.as_mut().unwrap().registers = 4;
    class
}

#[test]
fn loop_entry_zero_promotes_to_proven_boolean_float_and_reference() {
    for (class, expected) in [
        (boolean_fixture(), "boolean"),
        (float_fixture(), "float"),
        (reference_fixture(), "java.lang.String"),
    ] {
        let java = source(&class).unwrap();
        assert!(java.contains("while (true)"), "{java}");
        assert!(java.contains(expected), "{java}");
    }
}

#[test]
fn loop_entry_wide_overlap_is_rejected() {
    let mut class = boolean_fixture();
    class.methods[0].code.as_mut().unwrap().instructions =
        vec![0x0012, 0x0238, 7, 0x0016, 1, 0x02d8, 0xff02, 0xfa28, 0x000f];
    assert!(source(&class).is_err());
}

#[test]
fn mixed_reference_and_boolean_carried_type_is_rejected() {
    let mut class = reference_fixture();
    class.methods[0].return_type = "Z".into();
    assert!(source(&class).is_err());
}

#[test]
fn later_integer_reuse_does_not_inherit_reference_type() {
    let java = source(&reused_register_fixture()).unwrap();
    assert!(java.contains("java.lang.String"), "{java}");
    assert!(java.contains("int"), "{java}");
}

#[test]
fn exit_only_reference_write_keeps_zero_iteration_null() {
    let java = source(&exit_only_reference_fixture()).unwrap();
    assert!(java.contains("java.lang.String"), "{java}");
    assert!(java.contains("null"), "{java}");
}

#[test]
fn second_instruction_old_value_read_prevents_lifetime_split() {
    let mut class = exit_only_reference_fixture();
    class.methods[0].code.as_mut().unwrap().instructions = vec![
        0x0012, 0x1112, 0x0338, 16, 0x1333, 7, 0x0071, 0, 0, 0x000c, 0x0828,
        0x0212, // first instruction in the latch block
        0x1071, 1, 0, // consume(v0) reads the entry value second
        0x03d8, 0xff03, 0xf128, 0x0011,
    ];
    let symbols = Arc::get_mut(&mut class.symbols).unwrap();
    symbols.strings.push("consume".into());
    symbols
        .protos
        .push(("V".into(), vec!["Ljava/lang/String;".into()]));
    symbols.methods.push((0, 1, 1));
    assert!(source(&class).is_err());
}

#[test]
fn exit_only_reference_proof_rejects_wide_backedge_overlap() {
    let mut class = exit_only_reference_fixture();
    class.methods[0].code.as_mut().unwrap().instructions = vec![
        0x0012, 0x1112, 0x0338, 14, 0x1333, 7, 0x0071, 0, 0, 0x000c, 0x0628, 0x0016,
        1, // wide write replaces v0/v1 on the backedge
        0x03d8, 0xff03, 0xf328, 0x0011,
    ];
    assert!(source(&class).is_err());
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn loop_entry_types_jvm_behavior() {
    let dir = std::env::temp_dir().join(format!("rdx-loop-entry-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let cases = [
        (
            boolean_fixture(),
            "static int calls; static boolean flag() { calls++; return (calls & 1) != 0; }",
            "for (int n=0;n<=3;n++) { calls=0; if (choose(n)!=(n%2!=0)||calls!=n) throw new AssertionError(n); }",
        ),
        (
            float_fixture(),
            "",
            "for (int n=0;n<=3;n++) { int bits=Float.floatToRawIntBits(choose(n)); int expected=n==0?0:Float.floatToRawIntBits(1.0f); if (bits!=expected) throw new AssertionError(n); }",
        ),
        (
            reference_fixture(),
            "static int calls; static String next() { return \"v\" + (++calls); }",
            "for (int n=0;n<=3;n++) { calls=0; String result=choose(n); if ((n==0?result!=null:!(\"v\"+n).equals(result))||calls!=n) throw new AssertionError(n); }",
        ),
        (
            reused_register_fixture(),
            "static int calls; static void consume(String value) { calls++; } static String next() { return \"v\" + calls; }",
            "for (int n=0;n<=3;n++) { calls=0; if (choose(n)!=1||calls!=n) throw new AssertionError(n); }",
        ),
        (
            exit_only_reference_fixture(),
            "static int calls; static String next() { calls++; return \"exit\"; }",
            "for (int n=0;n<=3;n++) { calls=0; String result=choose(n); if ((n==0?result!=null:!\"exit\".equals(result))||calls!=(n==0?0:1)) throw new AssertionError(n); }",
        ),
    ];
    for (index, (class, helpers, checks)) in cases.into_iter().enumerate() {
        let java = format!(
            "package sample; public class LoopEntry {{ {helpers}\n{}\npublic static void main(String[] args) {{ {checks} }} }}",
            source(&class).unwrap()
        );
        fs::write(dir.join("sample/LoopEntry.java"), java).unwrap();
        for (program, argument) in [
            ("javac", "sample/LoopEntry.java"),
            ("java", "sample.LoopEntry"),
        ] {
            let result = Command::new(program)
                .arg(argument)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "case {index} {program}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
    }
}
