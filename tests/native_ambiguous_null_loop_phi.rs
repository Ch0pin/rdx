//! A shared DEX zero has reference-only uses in two distinct loop phis.
use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_hierarchy::TypeHierarchy,
    native_java,
    native_method::MethodAnalysis,
    native_types::{AssignmentBound, TypeResolution},
};
use std::{fs, process::Command, sync::Arc};

fn fixture() -> DexClass {
    let descriptor: Arc<str> = "Lsample/AmbiguousNullLoop;".into();
    let class = DexClass {
        descriptor: descriptor.clone(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            types: vec![descriptor.clone(), "[Ljava/lang/Object;".into()],
            strings: vec!["nextA".into(), "nextB".into(), "inspect".into()],
            protos: vec![
                (
                    "Ljava/lang/String;".into(),
                    vec!["Ljava/lang/Object;".into()],
                ),
                (
                    "Ljava/lang/StringBuilder;".into(),
                    vec!["Ljava/lang/Object;".into()],
                ),
                (
                    "V".into(),
                    vec![
                        "Ljava/lang/String;".into(),
                        "Ljava/lang/StringBuilder;".into(),
                    ],
                ),
            ],
            methods: vec![(0, 0, 0), (0, 1, 1), (0, 2, 2)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: descriptor,
            name: "choose".into(),
            return_type: "[Ljava/lang/Object;".into(),
            parameters: vec!["I".into()],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 5,
                ins: 1,
                outs: 2,
                tries: 0,
                try_regions: vec![],
                offset: 0,
                instructions: vec![
                    0x0012, // @0: shared untyped zero in v0
                    0x0107, // @1: move-object v1, v0
                    0x0438, 13, // @2: zero iterations -> exit @15
                    0x1071, 0, 0,      // @4: nextA(v0) -> String
                    0x000c, // @7: v0 = result
                    0x1071, 1, 1,      // @8: nextB(v1) -> StringBuilder
                    0x010c, // @11: v1 = result
                    0x04d8, 0xff04, // @12: count--
                    0xf428, // @14: backedge -> @2
                    0x2071, 2, 0x0010, // @15: inspect(String, StringBuilder)
                    0x2024, 1, 0x0010, // @18: Object[] {v0, v1}
                    0x020c, // @21: array result
                    0x0211, // @22: return-object v2
                ],
            }),
        }],
    };
    let hierarchy = Arc::new(TypeHierarchy::from_classes([&class]).unwrap());
    class.symbols.hierarchy.set(hierarchy).unwrap();
    class
}

fn source(class: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.AmbiguousNullLoop", class, &class.methods[0])?.source)
}

#[test]
fn shared_null_keeps_ambiguous_input_and_resolves_each_reference_phi() {
    let class = fixture();
    let analysis = MethodAnalysis::build(&class, &class.methods[0]).unwrap();
    let types = analysis.infer_types().unwrap();
    for (register, descriptor) in [(0, "Ljava/lang/String;"), (1, "Ljava/lang/StringBuilder;")] {
        let phi = analysis
            .ssa()
            .phis
            .iter()
            .find(|phi| {
                phi.register == register && analysis.ssa().graph.blocks[phi.block].start == 2
            })
            .unwrap();
        assert_eq!(
            types.values[phi.result].resolution,
            TypeResolution::Resolved(descriptor.into())
        );
        assert!(phi.incoming.iter().any(|(_, id)| {
            let value = &types.values[*id];
            value.assignment
                == [AssignmentBound::Literal {
                    bits: 0,
                    wide: false,
                }]
                && value
                    .required_types
                    .iter()
                    .all(|ty| ty.starts_with('L') || ty.starts_with('['))
                && matches!(
                    value.resolution,
                    TypeResolution::Unresolved("literal has multiple use types")
                )
        }));
    }
}

#[test]
fn shared_null_reconstructs_two_independently_typed_loop_locals() {
    let java = source(&fixture()).unwrap();
    assert!(
        java.contains("String") && java.contains("StringBuilder"),
        "{java}"
    );
    assert!(java.contains("while (") && java.contains("null"), "{java}");
}

#[test]
fn nonzero_undefined_wide_and_integer_inputs_are_never_promoted_to_null() {
    for (name, prefix) in [
        ("nonzero", vec![0x1012]),
        ("undefined", vec![0x0000]),
        ("wide", vec![0x0016, 0]),
        ("integer parameter", vec![0x00d8, 0x0004]),
    ] {
        let mut class = fixture();
        class.methods[0]
            .code
            .as_mut()
            .unwrap()
            .instructions
            .splice(0..1, prefix);
        // Every branch and target follows the changed prefix, so all relative
        // offsets remain valid even when the prefix grows by one code unit.
        MethodAnalysis::build(&class, &class.methods[0]).unwrap_or_else(|error| {
            panic!("{name}: valid structural analysis required: {error:#}")
        });
        assert!(source(&class).is_err(), "{name} must retain fallback");
    }
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn shared_null_loop_jvm_preserves_zero_iterations_identity_and_call_order() {
    let directory =
        std::env::temp_dir().join(format!("rdx-ambiguous-null-loop-{}", std::process::id()));
    fs::create_dir_all(directory.join("sample")).unwrap();
    let java = format!(
        r#"package sample;
public class AmbiguousNullLoop {{
    static Object previousA, previousB;
    static StringBuilder trace;
    static String nextA(Object old) {{
        if (old != previousA) throw new AssertionError("A identity");
        trace.append('A');
        String result = new String("a" + trace.length());
        previousA = result;
        return result;
    }}
    static StringBuilder nextB(Object old) {{
        if (old != previousB) throw new AssertionError("B identity");
        trace.append('B');
        StringBuilder result = new StringBuilder("b" + trace.length());
        previousB = result;
        return result;
    }}
    static void inspect(String a, StringBuilder b) {{
        if (a != previousA || b != previousB) throw new AssertionError("exit identity");
        trace.append('I');
    }}
    {}
    public static void main(String[] args) {{
        for (int n = 0; n <= 4; n++) {{
            previousA = previousB = null;
            trace = new StringBuilder();
            Object[] result = choose(n);
            if (result.length != 2 || result[0] != previousA || result[1] != previousB)
                throw new AssertionError("returned identities " + n);
            if (!trace.toString().equals("AB".repeat(n) + "I"))
                throw new AssertionError("order " + trace);
        }}
    }}
}}"#,
        source(&fixture()).unwrap()
    );
    fs::write(directory.join("sample/AmbiguousNullLoop.java"), java).unwrap();
    for (program, argument) in [
        ("javac", "sample/AmbiguousNullLoop.java"),
        ("java", "sample.AmbiguousNullLoop"),
    ] {
        let result = Command::new(program)
            .arg(argument)
            .current_dir(&directory)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{program}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
