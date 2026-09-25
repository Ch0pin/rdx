use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_java,
};
use std::sync::Arc;
fn fixture(flags: u32, superclass: &str, ty: &str) -> DexClass {
    let receiver = usize::from(flags & 8 == 0);
    DexClass {
        descriptor: "Lsample/Thrower;".into(),
        superclass: Some(superclass.into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols::default()),
        methods: vec![DexMethod {
            declaring_type: "Lsample/Thrower;".into(),
            name: "test".into(),
            return_type: "V".into(),
            parameters: vec![ty.into()],
            thrown_types: vec![],
            access_flags: flags,
            code: Some(DexCode {
                registers: (receiver + 1) as u16,
                ins: (receiver + 1) as u16,
                outs: 0,
                tries: 0,
                try_regions: vec![],
                instructions: vec![((receiver as u16) << 8) | 0x27],
                offset: 0,
            }),
        }],
    }
}
fn render(class: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.Thrower", class, &class.methods[0])?.source)
}
#[test]
fn checked_explicit_throws_are_inferred_only_for_unconstrained_declarations() {
    for flags in [9, 2] {
        let source = render(&fixture(
            flags,
            "Ljava/lang/Object;",
            "Ljava/io/IOException;",
        ))
        .unwrap();
        assert!(source.contains("throws java.io.IOException"), "{source}");
        assert!(source.contains("throw p0;"), "{source}");
    }
    assert!(
        render(&fixture(1, "Ljava/lang/Object;", "Ljava/io/IOException;")).is_err(),
        "instance override must not gain throws"
    );
    assert!(
        render(&fixture(9, "Lsample/Base;", "Ljava/io/IOException;")).is_err(),
        "unknown static hiding contract"
    );
    assert!(
        render(&fixture(2, "Lsample/Base;", "Ljava/io/IOException;")).is_err(),
        "unknown private method superclass signature conflict"
    );
    assert!(
        render(&fixture(9, "Ljava/lang/Object;", "Ljava/lang/Object;")).is_err(),
        "unproven throwable"
    );
    let unchecked = render(&fixture(
        9,
        "Ljava/lang/Object;",
        "Ljava/lang/RuntimeException;",
    ))
    .unwrap();
    assert!(!unchecked.contains(" throws "), "{unchecked}");
}
#[test]
fn locally_handled_checked_throw_does_not_change_override_contract() {
    let mut class = fixture(1, "Ljava/lang/Object;", "Ljava/io/IOException;");
    let code = class.methods[0].code.as_mut().unwrap();
    code.instructions = vec![0x0127, 0x000d, 0x000e];
    code.tries = 1;
    code.try_regions = vec![DexTryRegion {
        start: 0,
        end: 1,
        catches: vec![(Some("Ljava/io/IOException;".into()), 1)].into(),
    }];
    let source = render(&class).unwrap();
    assert!(!source.contains(" throws "), "{source}");
    assert!(source.contains("throw p0;"), "{source}");
}
#[test]
#[ignore = "requires javac and java"]
fn inferred_static_and_private_declarations_compile_and_preserve_exception_identity() {
    use std::process::Command;
    for flags in [9, 2] {
        let source = render(&fixture(
            flags,
            "Ljava/lang/Object;",
            "Ljava/io/IOException;",
        ))
        .unwrap();
        let dir = std::env::temp_dir().join(format!(
            "rdx-inferred-throws-{}-{flags}",
            std::process::id()
        ));
        std::fs::create_dir_all(dir.join("sample")).unwrap();
        let call = if flags & 8 != 0 {
            "test(expected)"
        } else {
            "new Thrower().test(expected)"
        };
        std::fs::write(dir.join("sample/Thrower.java"),format!(r#"package sample;
public class Thrower {{ {source}
 public static void main(String[] args){{java.io.IOException expected=new java.io.IOException("same");
  try{{{call};throw new AssertionError("not thrown");}}catch(java.io.IOException actual){{if(actual!=expected)throw new AssertionError("identity");}}
 }}
}}"#)).unwrap();
        for (command, args) in [
            ("javac", vec!["sample/Thrower.java"]),
            ("java", vec!["-cp", ".", "sample.Thrower"]),
        ] {
            let result = Command::new(command)
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{command}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
