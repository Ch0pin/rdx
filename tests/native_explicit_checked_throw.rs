use rdx::{
    native_dex::{DexClass, DexCode, DexField, DexMethod, DexSymbols},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;
fn fixture(words: Vec<u16>) -> DexClass {
    fixture_with_message(words, false)
}
fn fixture_with_message(words: Vec<u16>, message: bool) -> DexClass {
    let symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Checked;".into()],
        strings: vec!["<init>".into(), "original message".into()],
        protos: vec![(
            "V".into(),
            if message {
                vec!["Ljava/lang/String;".into()]
            } else {
                vec![]
            },
        )],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let make = |owner: &str, parent: &str, methods| DexClass {
        descriptor: owner.into(),
        superclass: Some(parent.into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods,
        symbols: symbols.clone(),
    };
    let root = make(
        "Lsample/Caller;",
        "Ljava/lang/Object;",
        vec![DexMethod {
            declaring_type: "Lsample/Caller;".into(),
            name: "run".into(),
            return_type: "V".into(),
            parameters: vec![],
            thrown_types: vec![],
            access_flags: 1,
            code: Some(DexCode {
                registers: 3,
                ins: 1,
                outs: 2,
                tries: 0,
                try_regions: vec![],
                instructions: words,
                offset: 0,
            }),
        }],
    );
    let checked = make("Lsample/Checked;", "Ljava/lang/Exception;", vec![]);
    symbols
        .hierarchy
        .set(Arc::new(
            TypeHierarchy::from_classes([&root, &checked]).unwrap(),
        ))
        .unwrap();
    root
}

fn field_fixture(words: Vec<u16>) -> DexClass {
    let mut class = fixture(words);
    let symbols = Arc::get_mut(&mut class.symbols).unwrap();
    symbols.types.push("Lsample/Caller;".into());
    symbols.strings.push("failure".into());
    symbols.fields.push((1, 0, 2));
    class.fields.push(DexField {
        declaring_type: class.descriptor.clone(),
        name: "failure".into(),
        field_type: "Lsample/Checked;".into(),
        access_flags: 1,
        is_static: false,
    });
    class
}

#[test]
fn nonnull_checked_field_throw_keeps_original_header() {
    for words in [
        vec![0x2054, 0, 0x0039, 3, 0x000e, 0x0027],
        vec![0x2054, 0, 0x0038, 3, 0x0027, 0x000e],
    ] {
        let class = field_fixture(words);
        let source = native_java::render("sample.Caller", &class).unwrap().source;
        assert!(source.contains("throw _rdx_checked_throw_"), "{source}");
        assert!(
            !source
                .lines()
                .find(|line| line.contains(" run("))
                .unwrap()
                .contains("throws"),
            "{source}"
        );
        assert!(class.methods[0].thrown_types.is_empty());
    }
    let mut class = field_fixture(vec![0x0062, 0, 0x0039, 3, 0x000e, 0x0027]);
    class.fields[0].is_static = true;
    class.fields[0].access_flags = 9;
    let source = native_java::render("sample.Caller", &class).unwrap().source;
    assert!(source.contains("throw _rdx_checked_throw_"), "{source}");
}

#[test]
fn checked_field_certificate_rejects_missing_guard_null_arm_and_redefinition() {
    for words in [
        vec![0x2054, 0, 0x0027],
        vec![0x2054, 0, 0x0038, 3, 0x000e, 0x0027],
        vec![0x2054, 0, 0x0039, 2, 0x0027],
        vec![0x2054, 0, 0x0039, 3, 0x000e, 0x0012, 0x0027],
    ] {
        let class = field_fixture(words);
        let source = native_java::render("sample.Caller", &class);
        // Redefining the operand to zero remains the ordinary throw-null path.
        if let Ok(source) = source {
            assert!(source.source.contains("throw null;"), "{}", source.source);
            assert!(
                !source.source.contains("throw _rdx_checked_throw_"),
                "{}",
                source.source
            );
        }
    }
    let mut class = field_fixture(vec![0x2054, 0, 0x0039, 3, 0x000e, 0x0027]);
    class.fields[0].field_type = "Ljava/lang/Object;".into();
    assert!(native_java::render("sample.Caller", &class).is_err());

    // One branch reaches the throw without visiting the field's null guard.
    // Seed the other path with a valid checked reference so refusal specifically
    // depends on the guard's authority rather than an undefined operand.
    let mut class = field_fixture(vec![
        0x1007, // move-object v0, p0 (checked parameter in v1)
        0x0238, 7, // if-eqz p1 (int v2), throw at 8
        0x0062, 0, // sget-object v0, failure
        0x0039, 3,      // if-nez v0, throw at 8
        0x000e, // null path returns
        0x0027,
    ]);
    class.methods[0].parameters = vec!["Lsample/Checked;".into(), "I".into()];
    let code = class.methods[0].code.as_mut().unwrap();
    code.registers = 4;
    code.ins = 3;
    // With four registers, instance receiver is v1 and parameters are v2/v3.
    code.instructions[0] = 0x2007;
    code.instructions[1] = 0x0338;
    class.fields[0].is_static = true;
    class.fields[0].access_flags = 9;
    assert!(native_java::render("sample.Caller", &class).is_err());
}
#[test]
fn fresh_string_checked_throw_keeps_message_and_header() {
    for words in [
        vec![0x0022, 0, 0x011a, 1, 0x2070, 0, 0x0010, 0x0027],
        vec![0x0022, 0, 0x011b, 1, 0, 0x2070, 0, 0x0010, 0x0027],
        vec![0x0022, 0, 0x011a, 1, 0x0276, 0, 0, 0x0027],
    ] {
        let class = fixture_with_message(words, true);
        let source = native_java::render("sample.Caller", &class).unwrap().source;
        assert!(source.contains("original message"), "{source}");
        assert!(source.contains("throw _rdx_checked_throw_"), "{source}");
        assert!(
            !source
                .lines()
                .find(|line| line.contains(" run("))
                .unwrap()
                .contains("throws"),
            "{source}"
        );
        assert!(class.methods[0].thrown_types.is_empty());
    }
}
#[test]
fn string_checked_shape_requires_exact_arguments_and_throw_receiver() {
    for words in [
        vec![0x0022, 0, 0x011a, 1, 0x2070, 0, 0x0020, 0x0027],
        vec![0x0022, 0, 0x011a, 1, 0x2070, 0, 0x0010, 0x0127],
        vec![0x0022, 0, 0x011a, 1, 0x0276, 0, 1, 0x0027],
    ] {
        let class = fixture_with_message(words, true);
        assert!(native_java::render("sample.Caller", &class).is_err());
    }
    let class = fixture(vec![0x0022, 0, 0x011a, 1, 0x2070, 0, 0x0010, 0x0027]);
    assert!(native_java::render("sample.Caller", &class).is_err());

    let mut class =
        fixture_with_message(vec![0x0022, 0, 0x011a, 1, 0x2070, 0, 0x0010, 0x0027], true);
    let symbols = Arc::get_mut(&mut class.symbols).unwrap();
    symbols.types.push("Ljava/lang/Exception;".into());
    symbols.methods[0].0 = 1;
    assert!(native_java::render("sample.Caller", &class).is_err());
}
#[test]
fn fresh_checked_throw_keeps_original_header() {
    let class = fixture(vec![0x0022, 0, 0x1070, 0, 0, 0x0027]);
    let source = native_java::render("sample.Caller", &class).unwrap().source;
    assert!(source.contains("throw _rdx_checked_throw_"), "{source}");
    assert!(
        !source
            .lines()
            .find(|l| l.contains(" run("))
            .unwrap()
            .contains("throws"),
        "{source}"
    );
    assert!(source.contains("throw (E) failure"), "{source}");
}
#[test]
fn primitive_and_undefined_throws_remain_rejected_and_null_keeps_npe() {
    for words in [vec![0x1012, 0x0027], vec![0x0027]] {
        let class = fixture(words);
        assert!(native_java::render_method("sample.Caller", &class, &class.methods[0]).is_err());
    }
    let class = fixture(vec![0x0012, 0x0027]);
    let source = native_java::render("sample.Caller", &class).unwrap().source;
    assert!(source.contains("throw null;"), "{source}");
    assert!(!source.contains("_rdx_checked_throw_"), "{source}");
}
#[test]
#[ignore = "requires javac/java; root runs serially"]
fn jvm_explicit_checked_bridge_preserves_constructor_order_and_identity() {
    use std::{fs, process::Command};
    let base = std::env::temp_dir().join(format!("rdx-explicit-checked-{}", std::process::id()));
    fs::create_dir_all(base.join("sample")).unwrap();
    let class = fixture(vec![0x0022, 0, 0x1070, 0, 0, 0x0027]);
    fs::write(
        base.join("sample/Caller.java"),
        native_java::render("sample.Caller", &class).unwrap().source,
    )
    .unwrap();
    fs::write(base.join("sample/Checked.java"),"package sample; public class Checked extends Exception { private static final long serialVersionUID=1L; static Checked last; static String trace=\"\"; public Checked(){trace+=\"C\";last=this;} }").unwrap();
    fs::write(base.join("sample/Check.java"),"package sample; public class Check { public static void main(String[] args) { for(int i=0;i<3;i++){Checked.trace=\"B\";try{new Caller().run();throw new AssertionError();}catch(Throwable e){if(e!=Checked.last||!Checked.trace.equals(\"BC\"))throw new AssertionError(e);}Checked.trace+=\"A\";if(!Checked.trace.equals(\"BCA\"))throw new AssertionError();}System.out.print(\"ok\");} }").unwrap();
    let output = Command::new("javac")
        .args([
            "-Xlint:all",
            "-Werror",
            "sample/Caller.java",
            "sample/Checked.java",
            "sample/Check.java",
        ])
        .current_dir(&base)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new("java")
        .args(["-cp", ".", "sample.Check"])
        .current_dir(&base)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"ok");
}

#[test]
#[ignore = "requires javac/java; root runs serially"]
fn jvm_string_checked_bridge_preserves_message_identity_and_constructor_order() {
    use std::{fs, process::Command};
    let base = std::env::temp_dir().join(format!("rdx-string-checked-{}", std::process::id()));
    fs::create_dir_all(base.join("sample")).unwrap();
    fs::write(base.join("sample/Checked.java"), "package sample; public class Checked extends Exception { private static final long serialVersionUID=1L; static Checked last; static String trace=\"\"; public Checked(String message){super(message);trace+=\"C\";last=this;} }").unwrap();
    fs::write(base.join("sample/Check.java"), "package sample; public class Check { public static void main(String[] args) { Checked.trace=\"B\";try{new Caller().run();throw new AssertionError();}catch(Throwable e){if(e!=Checked.last||!Checked.trace.equals(\"BC\")||!\"original message\".equals(e.getMessage()))throw new AssertionError(e);}Checked.trace+=\"A\";if(!Checked.trace.equals(\"BCA\"))throw new AssertionError();System.out.print(\"ok\");} }").unwrap();
    for words in [
        vec![0x0022, 0, 0x011a, 1, 0x2070, 0, 0x0010, 0x0027],
        vec![0x0022, 0, 0x011b, 1, 0, 0x2070, 0, 0x0010, 0x0027],
        vec![0x0022, 0, 0x011a, 1, 0x0276, 0, 0, 0x0027],
    ] {
        let class = fixture_with_message(words, true);
        fs::write(
            base.join("sample/Caller.java"),
            native_java::render("sample.Caller", &class).unwrap().source,
        )
        .unwrap();
        let compile = Command::new("javac")
            .args([
                "-Xlint:all",
                "-Werror",
                "sample/Caller.java",
                "sample/Checked.java",
                "sample/Check.java",
            ])
            .current_dir(&base)
            .output()
            .unwrap();
        assert!(
            compile.status.success(),
            "{}",
            String::from_utf8_lossy(&compile.stderr)
        );
        let run = Command::new("java")
            .args(["-cp", ".", "sample.Check"])
            .current_dir(&base)
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "{}",
            String::from_utf8_lossy(&run.stderr)
        );
        assert_eq!(run.stdout, b"ok");
    }
}

#[test]
#[ignore = "requires javac/java; root runs serially"]
fn jvm_guarded_field_preserves_object_and_null_return_path() {
    use std::{fs, process::Command};
    let base = std::env::temp_dir().join(format!("rdx-field-checked-{}", std::process::id()));
    fs::create_dir_all(base.join("sample")).unwrap();
    fs::write(base.join("sample/Checked.java"), "package sample; public class Checked extends Exception { private static final long serialVersionUID=1L; public Checked(String message){super(message);} }").unwrap();
    fs::write(base.join("sample/Check.java"), "package sample; public class Check { public static void main(String[] args) { Caller caller=new Caller();caller.run();Checked original=new Checked(\"field message\");caller.failure=original;try{caller.run();throw new AssertionError();}catch(Throwable e){if(e!=original||!\"field message\".equals(e.getMessage()))throw new AssertionError(e);}caller.failure=null;caller.run();System.out.print(\"ok\");} }").unwrap();
    for words in [
        vec![0x2054, 0, 0x0039, 3, 0x000e, 0x0027],
        vec![0x2054, 0, 0x0038, 3, 0x0027, 0x000e],
    ] {
        let class = field_fixture(words);
        fs::write(
            base.join("sample/Caller.java"),
            native_java::render("sample.Caller", &class).unwrap().source,
        )
        .unwrap();
        let compile = Command::new("javac")
            .args([
                "-Xlint:all",
                "-Werror",
                "sample/Caller.java",
                "sample/Checked.java",
                "sample/Check.java",
            ])
            .current_dir(&base)
            .output()
            .unwrap();
        assert!(
            compile.status.success(),
            "{}",
            String::from_utf8_lossy(&compile.stderr)
        );
        let run = Command::new("java")
            .args(["-cp", ".", "sample.Check"])
            .current_dir(&base)
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "{}",
            String::from_utf8_lossy(&run.stderr)
        );
        assert_eq!(run.stdout, b"ok");
    }
}
