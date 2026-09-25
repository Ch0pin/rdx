//! Throw reconstruction tests use only native DEX fixtures and Rust assertions.
use rdx::{
    native_dex::{self, DexClass},
    native_java,
};

fn fixture(
    words: &[u16],
    parameters: &[&str],
    ret: &str,
    registers: u16,
    declared: &[&str],
) -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    let method = &mut class.methods[0];
    method.name = "throwing".into();
    method.access_flags = 9;
    method.parameters = parameters.iter().map(|ty| (*ty).into()).collect();
    method.return_type = ret.into();
    method.thrown_types = declared.iter().map(|ty| (*ty).into()).collect();
    let code = method.code.as_mut().unwrap();
    code.registers = registers;
    code.ins = parameters.len() as u16;
    code.instructions = words.to_vec();
    class
}
fn render(class: &DexClass) -> anyhow::Result<rdx::engine::DecompiledCode> {
    native_java::render_method("sample.Hello", class, &class.methods[0])
}
#[test]
fn unchecked_exception_and_null_are_terminal_throws() {
    for ty in [
        "Ljava/lang/RuntimeException;",
        "Ljava/lang/IllegalArgumentException;",
        "Ljava/lang/Error;",
    ] {
        let code = render(&fixture(&[0x0027], &[ty], "I", 1, &[])).unwrap();
        assert!(code.source.contains("throw p0;"), "{}", code.source);
        assert!(!code.source.contains("return "));
    }
    let code = render(&fixture(&[0x0012, 0x0027], &[], "V", 1, &[])).unwrap();
    assert!(code.source.contains("throw null;"));
}
#[test]
fn checked_throw_uses_exact_declared_or_body_inferred_type_and_preserves_link() {
    let ty = "Ljava/io/IOException;";
    let code = render(&fixture(&[0x0027], &[ty], "V", 1, &[ty])).unwrap();
    assert!(code.source.contains("throws java.io.IOException"));
    assert!(code.source.contains("throw p0;"));
    let link = code
        .links
        .iter()
        .find(|l| l.label == "java.io.IOException")
        .unwrap();
    let text: String = code
        .source
        .chars()
        .skip(link.start)
        .take(link.end - link.start)
        .collect();
    assert_eq!(text, "java.io.IOException");
    let inferred = render(&fixture(&[0x0027], &[ty], "V", 1, &[])).unwrap();
    assert!(inferred.source.contains("throws java.io.IOException"));
    assert!(inferred.source.contains("throw p0;"));
    let link = inferred
        .links
        .iter()
        .find(|link| link.label == "java.io.IOException")
        .unwrap();
    assert_eq!(
        inferred
            .source
            .chars()
            .skip(link.start)
            .take(link.end - link.start)
            .collect::<String>(),
        "java.io.IOException"
    );
    assert!(
        render(&fixture(
            &[0x0027],
            &[ty],
            "V",
            1,
            &["Ljava/lang/Exception;"]
        ))
        .is_err()
    );
}
#[test]
fn throw_branch_and_return_branch_keep_single_terminal_per_path() {
    let code = render(&fixture(
        &[0x0038, 3, 0x0127, 0x1012, 0x000f],
        &["I", "Ljava/lang/RuntimeException;"],
        "I",
        2,
        &[],
    ))
    .unwrap();
    assert!(code.source.contains("if ("));
    assert_eq!(code.source.matches("throw p1;").count(), 1);
    assert_eq!(code.source.matches("return 1;").count(), 1);
    let code = render(&fixture(
        &[0x0038, 3, 0x0127, 0x0027],
        &[
            "Ljava/lang/RuntimeException;",
            "Ljava/lang/RuntimeException;",
        ],
        "V",
        2,
        &[],
    ))
    .unwrap();
    assert_eq!(code.source.matches("throw ").count(), 2);
    assert!(!code.source.contains("return;"));
}
#[test]
fn nonthrowable_undefined_and_unreachable_throw_forms_fail_closed() {
    for (words, parameters, registers) in [
        (vec![0x0027], vec!["Ljava/lang/Object;"], 1),
        (vec![0x0027], vec!["I"], 1),
        (vec![0x1012, 0x0027], vec![], 1),
        (vec![0x0027], vec![], 1),
        (
            vec![0x0027, 0x000e],
            vec!["Ljava/lang/RuntimeException;"],
            1,
        ),
        (vec![0x0127], vec!["Ljava/lang/RuntimeException;"], 1),
    ] {
        assert!(
            render(&fixture(&words, &parameters, "V", registers, &[])).is_err(),
            "accepted {words:x?}"
        );
    }
}
#[test]
fn known_current_class_unchecked_subclass_is_accepted() {
    let mut class = fixture(&[0x0027], &["Lsample/Hello;"], "V", 1, &[]);
    class.superclass = Some("Ljava/lang/RuntimeException;".into());
    assert!(render(&class).unwrap().source.contains("throw p0;"));
    class.superclass = Some("Ljava/lang/Object;".into());
    assert!(render(&class).is_err());
}

#[test]
fn allocated_exception_is_initialized_once_before_throw() {
    use rdx::native_dex::DexSymbols;
    use std::sync::Arc;
    let mut class = fixture(&[0x0022, 0, 0x1070, 0, 0, 0x0027], &[], "V", 1, &[]);
    class.symbols = Arc::new(DexSymbols {
        types: vec!["Ljava/lang/RuntimeException;".into()],
        strings: vec!["<init>".into()],
        methods: vec![(0, 0, 0)],
        protos: vec![("V".into(), vec![])],
        ..Default::default()
    });
    class.methods[0].code.as_mut().unwrap().outs = 1;
    let code = render(&class).unwrap();
    assert_eq!(
        code.source
            .matches("new java.lang.RuntimeException()")
            .count(),
        1
    );
    assert!(
        code.source
            .find("new java.lang.RuntimeException()")
            .unwrap()
            < code.source.find("throw v0;").unwrap()
    );
    assert_eq!(
        code.links
            .iter()
            .filter(|l| l.label == "java.lang.RuntimeException.<init>()V")
            .count(),
        1
    );
    class.methods[0].code.as_mut().unwrap().instructions = vec![0x0022, 0, 0x0027];
    assert!(render(&class).is_err());
}

#[test]
fn malformed_declared_nonthrowable_or_unknown_hierarchy_is_not_trusted() {
    for ty in [
        "Ljava/lang/String;",
        "Ljava/lang/Object;",
        "[Ljava/lang/Exception;",
        "Lunknown/MaybeException;",
    ] {
        assert!(
            render(&fixture(&[0x0027], &[ty], "V", 1, &[ty])).is_err(),
            "accepted declared {ty}"
        );
    }
    let mut class = fixture(&[0x0027], &["Lsample/Hello;"], "V", 1, &["Lsample/Hello;"]);
    class.superclass = Some("Ljava/lang/Exception;".into());
    assert!(render(&class).is_ok());
    class.superclass = Some("Lunknown/Base;".into());
    assert!(render(&class).is_err());
}
#[test]
fn static_initializer_does_not_acquire_checked_throws_clause() {
    let mut class = fixture(&[0x000e], &[], "V", 0, &["Ljava/io/IOException;"]);
    class.methods[0].name = "<clinit>".into();
    class.methods[0].access_flags = 0x10008;
    assert!(render(&class).is_err());
}

#[test]
fn explicit_throw_inside_try_keeps_typed_handler() {
    let mut class = fixture(
        &[0x0027, 0x000d, 0x000e],
        &["Ljava/lang/RuntimeException;"],
        "V",
        1,
        &[],
    );
    let code = class.methods[0].code.as_mut().unwrap();
    code.tries = 1;
    code.try_regions = vec![rdx::native_dex::DexTryRegion {
        start: 0,
        end: 1,
        catches: std::sync::Arc::from(vec![(
            Some(std::sync::Arc::<str>::from("Ljava/lang/RuntimeException;")),
            1,
        )]),
    }];
    let code = render(&class).unwrap();
    assert!(code.source.contains("try {"));
    assert_eq!(code.source.matches("throw p0;").count(), 1);
    assert!(code.source.contains("catch (java.lang.RuntimeException "));
}
