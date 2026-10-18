use rdx::{
    native_dex::{self, DexClass, DexSymbols},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;

fn fixture(input: &str, range: bool, hierarchy: bool) -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class.methods.retain(|m| m.name.as_ref() == "answer");
    let method = &mut class.methods[0];
    method.access_flags = 9;
    method.parameters = vec![input.into(), input.into()];
    method.return_type = "[Lsample/Parent;".into();
    let code = method.code.as_mut().unwrap();
    code.registers = 2;
    code.ins = 2;
    code.outs = 2;
    code.instructions = if range {
        vec![0x0225, 0, 0, 0x000c, 0x0011]
    } else {
        vec![0x2024, 0, 0x0010, 0x000c, 0x0011]
    };
    class.symbols = Arc::new(DexSymbols {
        types: vec!["[Lsample/Parent;".into()],
        ..Default::default()
    });
    if hierarchy {
        let declaration = |descriptor: &str, parent: &str| DexClass {
            descriptor: descriptor.into(),
            superclass: Some(parent.into()),
            interfaces: vec![],
            access_flags: 1,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            methods: vec![],
            symbols: Arc::new(DexSymbols::default()),
        };
        let parent = declaration("Lsample/Parent;", "Ljava/lang/Object;");
        let child = declaration("Lsample/Child;", "Lsample/Parent;");
        let sibling = declaration("Lsample/Sibling;", "Ljava/lang/Object;");
        class
            .symbols
            .hierarchy
            .set(Arc::new(
                TypeHierarchy::from_classes([&class, &parent, &child, &sibling]).unwrap(),
            ))
            .unwrap();
    }
    class
}

#[test]
fn proven_subtypes_fill_declared_component_without_casts_in_both_encodings() {
    for range in [false, true] {
        let class = fixture("Lsample/Child;", range, true);
        let out = native_java::render_method("sample.Hello", &class, &class.methods[0]).unwrap();
        assert!(
            out.source.contains("new sample.Parent[] {p0, p1}"),
            "{}",
            out.source
        );
        assert!(!out.source.contains("((sample.Parent)"), "{}", out.source);
    }
}

#[test]
fn absent_or_incompatible_subtype_proof_remains_rejected() {
    for (input, hierarchy) in [
        ("Lsample/Child;", false),
        ("Lsample/Sibling;", true),
        ("Lsample/Unknown;", true),
        ("I", true),
    ] {
        let class = fixture(input, false, hierarchy);
        assert!(
            native_java::render_method("sample.Hello", &class, &class.methods[0]).is_err(),
            "accepted {input} hierarchy={hierarchy}"
        );
    }
}

fn null_loop(nonzero: bool) -> DexClass {
    let mut class = fixture("Lsample/Child;", false, true);
    let method = &mut class.methods[0];
    method.parameters = vec!["I".into()];
    let code = method.code.as_mut().unwrap();
    code.registers = 3;
    code.ins = 1;
    code.instructions = vec![
        0x0012,
        0x0112, // null word and loop counter
        0x2135,
        6, // counter >= count -> filled array at8
        if nonzero { 0x1012 } else { 0x0012 },
        0x01d8,
        0x0101,
        0xfb28, // counter++, feedback to2
        0x1024,
        0,
        0,
        0x000c,
        0x0011,
    ];
    class
}

#[test]
fn filled_reference_array_accepts_complete_zero_loop_phi_and_rejects_nonzero() {
    let class = null_loop(false);
    let rendered = native_java::render_method("sample.Hello", &class, &class.methods[0]).unwrap();
    assert!(
        rendered.source.contains("new sample.Parent[] {null}"),
        "{}",
        rendered.source
    );
    let class = null_loop(true);
    assert!(native_java::render_method("sample.Hello", &class, &class.methods[0]).is_err());
}

#[test]
#[ignore = "requires javac/java; root runs serially"]
fn jvm_subtype_filled_array_preserves_component_identity_order_and_null() {
    use std::{fs, process::Command};
    let base = std::env::temp_dir().join(format!("rdx-filled-subtype-{}", std::process::id()));
    fs::create_dir_all(base.join("sample")).unwrap();
    fs::write(
        base.join("sample/Parent.java"),
        "package sample; class Parent {}",
    )
    .unwrap();
    fs::write(
        base.join("sample/Child.java"),
        "package sample; class Child extends Parent {}",
    )
    .unwrap();
    for range in [false, true] {
        let class = fixture("Lsample/Child;", range, true);
        let method = native_java::render_method("sample.Hello", &class, &class.methods[0])
            .unwrap()
            .source;
        let source = format!(
            "package sample; public class Hello {{ {method} public static void main(String[] args) {{ Child a=new Child(),b=new Child(); Parent[] values=answer(a,b); if(values.getClass()!=Parent[].class || values.length!=2 || values[0]!=a || values[1]!=b)throw new AssertionError(); values=answer(null,a); if(values[0]!=null || values[1]!=a)throw new AssertionError(); System.out.print(\"ok\"); }} }}"
        );
        fs::write(base.join("sample/Hello.java"), source).unwrap();
        let compile = Command::new("javac")
            .args([
                "sample/Parent.java",
                "sample/Child.java",
                "sample/Hello.java",
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
            .args(["-cp", ".", "sample.Hello"])
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
    let class = null_loop(false);
    let method = native_java::render_method("sample.Hello", &class, &class.methods[0])
        .unwrap()
        .source;
    fs::write(base.join("sample/Hello.java"), format!("package sample; public class Hello {{ {method} public static void main(String[] args) {{ for(int n:new int[]{{-1,0,1,5}}){{ Parent[] a=answer(n); if(a.getClass()!=Parent[].class || a.length!=1 || a[0]!=null)throw new AssertionError(); }} System.out.print(\"ok\"); }} }}")).unwrap();
    let compile = Command::new("javac")
        .args(["sample/Parent.java", "sample/Hello.java"])
        .current_dir(&base)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new("java")
        .args(["-cp", ".", "sample.Hello"])
        .current_dir(&base)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(run.stdout, b"ok");
    fs::remove_dir_all(base).unwrap();
}
