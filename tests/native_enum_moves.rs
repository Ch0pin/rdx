use rdx::{
    native_dex::{DexClass, DexCode, DexField, DexMethod, DexSymbols},
    native_java,
};
use std::sync::Arc;
fn base_fixture() -> DexClass {
    let mut class = DexClass {
        descriptor: "Lsample/Mode;".into(),
        superclass: Some("Ljava/lang/Enum;".into()),
        interfaces: vec![],
        access_flags: 0x4011,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![],
        symbols: Arc::new(DexSymbols {
            strings: vec![
                "ONE".into(),
                "TWO".into(),
                "a".into(),
                "b".into(),
                "all".into(),
                "<init>".into(),
                "clone".into(),
            ],
            types: vec![
                "Lsample/Mode;".into(),
                "Ljava/lang/Enum;".into(),
                "[Lsample/Mode;".into(),
            ],
            protos: vec![
                ("V".into(), vec!["Ljava/lang/String;".into(), "I".into()]),
                ("Ljava/lang/Object;".into(), vec![]),
            ],
            methods: vec![(1, 0, 5), (2, 1, 6)],
            fields: vec![(0, 0, 2), (0, 0, 3), (0, 2, 4)],
            ..Default::default()
        }),
    };
    for (name, ty, flags) in [
        ("a", "Lsample/Mode;", 0x4019),
        ("b", "Lsample/Mode;", 0x4019),
        ("all", "[Lsample/Mode;", 0x1a),
    ] {
        class.fields.push(DexField {
            declaring_type: class.descriptor.clone(),
            name: name.into(),
            field_type: ty.into(),
            access_flags: flags,
            is_static: true,
        });
    }
    class.methods.push(DexMethod {
        declaring_type: class.descriptor.clone(),
        name: "<clinit>".into(),
        return_type: "V".into(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 0x10008,
        code: Some(DexCode {
            registers: 4,
            ins: 0,
            outs: 3,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: vec![
                0x0022, 0, 0x011a, 0, 0x0212, 0x3070, 0, 0x0210, 0x0069, 0, 0x0122, 0, 0x021a, 1,
                0x1312, 0x3070, 0, 0x0321, 0x0169, 1, 0x2024, 2, 0x0010, 0x000c, 0x0069, 2, 0x000e,
            ],
        }),
    });
    class.methods.push(DexMethod {
        declaring_type: class.descriptor.clone(),
        name: "values".into(),
        return_type: "[Lsample/Mode;".into(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 9,
        code: Some(DexCode {
            registers: 1,
            ins: 0,
            outs: 1,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: vec![0x0062, 2, 0x106e, 1, 0, 0x000c, 0x001f, 2, 0x0011],
        }),
    });
    class
}

// Constants remain in the original array order after a register permutation.
// Each form crosses a different DEX operand-width boundary.
fn fixture(form: u8) -> DexClass {
    let mut class = base_fixture();
    let code = class.methods[0].code.as_mut().unwrap();
    code.registers = 300;
    let mut words = code.instructions[..20].to_vec();
    match form {
        0 => words.extend([0x0307, 0x1007, 0x3107]),
        1 => words.extend([0x0308, 0, 0x0008, 1, 0x0108, 3]),
        2 => words.extend([0x0009, 260, 0, 0x1007, 0x0108, 260]),
        _ => unreachable!(),
    }
    words.extend([0x2024, 2, 0x0001, 0x000c, 0x0069, 2, 0x000e]);
    code.instructions = words;
    class
}

#[test]
fn initialized_enum_object_moves_preserve_array_order_and_navigation() {
    for form in 0..3 {
        let class = fixture(form);
        let code = native_java::render("sample.Mode", &class).unwrap();
        assert!(code.source.contains("ONE,"), "{}", code.source);
        assert!(code.source.contains("Mode a = ONE"), "{}", code.source);
        assert!(code.source.contains("Mode b = TWO"), "{}", code.source);
        for field in ["a", "b"] {
            assert!(
                code.links
                    .iter()
                    .any(|link| link.label == format!("sample.Mode.{field}:Lsample/Mode;"))
            );
            assert!(
                code.definitions
                    .iter()
                    .any(|d| d.name == field && d.kind == "field")
            );
        }
        native_java::render_method("sample.Mode", &class, &class.methods[0]).unwrap();
    }
}

#[test]
fn enum_scalar_and_text_moves_preserve_constructor_arguments() {
    for moves in [
        vec![0x2301, 0x3201],
        vec![0x0302, 2, 0x0202, 3],
        vec![0x0003, 260, 2, 0x0202, 260],
        vec![0x1307, 0x3107],
    ] {
        let mut class = base_fixture();
        let code = class.methods[0].code.as_mut().unwrap();
        code.registers = 300;
        code.instructions.splice(5..5, moves);
        native_java::render("sample.Mode", &class).unwrap();
    }
}

#[test]
fn enum_array_moves_decline_without_shared_mutation_identity() {
    let mut class = base_fixture();
    let code = class.methods[0].code.as_mut().unwrap();
    code.instructions.insert(24, 0x0307); // v3 = initialized array in v0
    code.instructions[25] = 0x0369; // publish the same array through its alias
    assert!(native_java::render("sample.Mode", &class).is_err());

    // The DEX alias sees this mutation: both arrays would be [TWO,TWO].
    // Deep-copying Atom::Array at the move would leave the published v3
    // incorrectly modeled as [ONE,TWO], allowing an unsound enum recovery.
    let mut mutated = base_fixture();
    let words = &mut mutated.methods[0].code.as_mut().unwrap().instructions;
    words.truncate(24); // initialized original array in v0
    words.extend([
        0x0307, // v3 = v0, same mutable array
        0x0212, // v2 = index zero
        0x014d, 0x0200, // v0[v2] = v1 (TWO)
        0x0369, 2, // publish v3
        0x000e,
    ]);
    assert!(native_java::render("sample.Mode", &mutated).is_err());
}

#[test]
fn enum_moves_decline_wrong_kinds_unknown_inputs_and_uninitialized_aliases() {
    for (at, moves) in [
        (20, vec![0x0301]),         // reference read through a scalar move
        (5, vec![0x2307]),          // integer read through an object move
        (5, vec![0x0307]),          // uninitialized enum allocation
        (5, vec![0x3307]),          // undefined source
        (20, vec![0x0308, 300]),    // source past frame
        (20, vec![0x0009, 300, 0]), // destination past frame
        (5, vec![0x2304]),          // wide words have no supported Atom representation
    ] {
        let mut class = base_fixture();
        class.methods[0]
            .code
            .as_mut()
            .unwrap()
            .instructions
            .splice(at..at, moves);
        assert!(native_java::render("sample.Mode", &class).is_err());
    }
    let mut wrong_order = fixture(2);
    let words = &mut wrong_order.methods[0].code.as_mut().unwrap().instructions;
    let array = words.iter().position(|&w| w == 0x2024).unwrap();
    words[array + 2] = 0x0010;
    assert!(native_java::render("sample.Mode", &wrong_order).is_err());
    let mut public = fixture(2);
    public.fields[2].access_flags = 0x1019;
    assert!(native_java::render("sample.Mode", &public).is_err());
}

#[test]
#[ignore = "requires javac/java; root runs serially with RDX_ENUM_MOVE_JVM_DIR"]
fn jvm_enum_moves_preserve_names_ordinals_identity_and_clone_isolation() {
    use std::{fs, process::Command};
    let root = std::env::var_os("RDX_ENUM_MOVE_JVM_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("rdx-enum-moves-{}", std::process::id()))
        });
    for form in 0..3 {
        let dir = root.join(format!("form{form}"));
        fs::create_dir_all(dir.join("sample")).unwrap();
        let class = fixture(form);
        fs::write(
            dir.join("sample/Mode.java"),
            native_java::render("sample.Mode", &class).unwrap().source,
        )
        .unwrap();
        // Independent expected DEX trace: allocate ONE/0, publish a; allocate
        // TWO/1, publish b; shuffle registers; array operands select ONE,TWO.
        fs::write(dir.join("sample/Check.java"), r#"package sample;
public class Check {
 public static void main(String[] args) {
  Mode[] aliases={Mode.a,Mode.b}; String[] names={"ONE","TWO"};
  for(int repeat=0;repeat<20;repeat++) {
   Mode[] values=Mode.values();
   if(values.length!=2) throw new AssertionError("length");
   for(int i=0;i<2;i++) {
    if(values[i]!=aliases[i] || values[i].ordinal()!=i || !values[i].name().equals(names[i])) throw new AssertionError("identity/order");
    if(Mode.valueOf(names[i])!=aliases[i]) throw new AssertionError("lookup");
   }
   values[0]=Mode.b; values[1]=null;
   if(Mode.values()[0]!=Mode.a || Mode.values()[1]!=Mode.b) throw new AssertionError("clone");
  }
 }
}"#).unwrap();
        for (program, args) in [
            ("javac", vec!["sample/Mode.java", "sample/Check.java"]),
            ("java", vec!["-cp", ".", "sample.Check"]),
        ] {
            let output = Command::new(program)
                .current_dir(&dir)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{program}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
