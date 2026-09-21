//! Typed array/type instruction regressions, entirely native Rust.
use rdx::{
    native_dex::{self, DexClass, DexSymbols},
    native_java,
};
use std::sync::Arc;
fn fixture(words: &[u16], params: &[&str], ret: &str, registers: u16, types: &[&str]) -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class.methods.retain(|m| m.name.as_ref() == "answer");
    class.symbols = Arc::new(DexSymbols {
        types: types.iter().map(|s| Arc::from(*s)).collect(),
        ..Default::default()
    });
    let method = &mut class.methods[0];
    method.parameters = params.iter().map(|s| Arc::from(*s)).collect();
    method.return_type = Arc::from(ret);
    method.access_flags = 9;
    let code = method.code.as_mut().unwrap();
    code.registers = registers;
    code.ins = params.len() as u16;
    code.instructions = words.to_vec();
    class
}
fn source(class: &DexClass) -> String {
    native_java::render_method("sample.Hello", class, &class.methods[0])
        .unwrap()
        .source
}
#[test]
fn new_array_allocates_only_requested_dimension_and_reads_length() {
    for (ty, expected) in [
        ("[I", "new int[p0]"),
        ("[[I", "new int[p0][]"),
        ("[[Ljava/lang/String;", "new java.lang.String[p0][]"),
    ] {
        let class = fixture(&[0x1023, 0, 0x0021, 0x000f], &["I"], "I", 2, &[ty]);
        let s = source(&class);
        assert!(s.contains(expected), "{s}");
        assert_eq!(s.matches("new ").count(), 1);
        assert!(s.contains(".length"));
    }
}
#[test]
fn array_load_happens_before_store_even_when_source_register_is_reused() {
    // aget v0,v1,v2; aput v3,v1,v2; return v0
    let class = fixture(
        &[0x0044, 0x0201, 0x034b, 0x0201, 0x000f],
        &["[I", "I", "I"],
        "I",
        4,
        &[],
    );
    let s = source(&class);
    assert!(s.contains("int v0 = p0[p1];"), "{s}");
    assert!(s.contains("p0[p1] = p2;"), "{s}");
    assert!(s.find("int v0").unwrap() < s.find(" = p2;").unwrap());
    assert!(s.contains("return v0;"));
}
#[test]
fn reference_array_store_uses_runtime_store_check_without_component_downcast() {
    let class = fixture(
        &[0x024d, 0x0100, 0x000e],
        &["[Ljava/lang/String;", "I", "Ljava/lang/Object;"],
        "V",
        3,
        &[],
    );
    let s = source(&class);
    assert!(s.contains("((java.lang.Object[]) p0)[p1] = p2;"), "{s}");
    assert!(!s.contains("((java.lang.String) p2)"));
}
#[test]
fn primitive_array_operations_validate_component_and_narrow_stores() {
    for (ty, op, cast) in [
        ("[B", 0x4f, "byte"),
        ("[C", 0x50, "char"),
        ("[S", 0x51, "short"),
    ] {
        let class = fixture(&[0x0200 | op, 0x0100, 0x000e], &[ty, "I", "I"], "V", 3, &[]);
        let s = source(&class);
        assert!(s.contains(&format!("p0[p1] = ({cast}) (p2);")), "{s}");
    }
    for (ty, op) in [("[I", 0x4d), ("[J", 0x4c), ("[Z", 0x4b)] {
        let class = fixture(&[0x0200 | op, 0x0100, 0x000e], &[ty, "I", "I"], "V", 3, &[]);
        assert!(native_java::render_method("sample.Hello", &class, &class.methods[0]).is_err());
    }
}
#[test]
fn casts_are_materialized_before_register_overwrite_and_links_are_exact() {
    let class = fixture(
        &[0x011f, 0, 0x1007, 0x0112, 0x0011],
        &["Ljava/lang/Object;"],
        "Ljava/lang/String;",
        2,
        &["Ljava/lang/String;"],
    );
    let code = native_java::render_method("sample.Hello", &class, &class.methods[0]).unwrap();
    assert!(
        code.source.contains("((java.lang.String) p0)"),
        "{}",
        code.source
    );
    assert!(code.source.contains("return v0;"));
    for link in code.links.iter().filter(|l| l.label == "java.lang.String") {
        assert_eq!(
            code.source
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect::<String>(),
            "java.lang.String"
        );
    }
}
#[test]
fn instance_of_and_class_literals_preserve_reference_types() {
    let class = fixture(
        &[0x0020, 0, 0x000f],
        &["Ljava/lang/String;"],
        "Z",
        1,
        &["[I"],
    );
    assert!(source(&class).contains("((java.lang.Object) p0) instanceof int[]"));
    let class = fixture(&[0x001c, 0, 0x0011], &[], "Ljava/lang/Class;", 1, &["[[I"]);
    assert!(source(&class).contains("int[][].class"));
    for words in [
        vec![0x001c, 1, 0x0011],
        vec![0x001f, 0, 0x0011],
        vec![0x0023, 0, 0x0011],
    ] {
        let class = fixture(&words, &["I"], "Ljava/lang/Object;", 1, &["I"]);
        assert!(native_java::render_method("sample.Hello", &class, &class.methods[0]).is_err());
    }
}
#[test]
fn unknown_array_types_and_truncated_operands_fail_closed() {
    for words in [
        vec![0x0021, 0x000f],
        vec![0x0044],
        vec![0x0044, 0x0000, 0x000f],
    ] {
        let class = fixture(&words, &["Ljava/lang/Object;"], "I", 1, &[]);
        assert!(native_java::render_method("sample.Hello", &class, &class.methods[0]).is_err());
    }
}

#[test]
fn varargs_and_bridge_methods_preserve_declarations_and_symbol_identity() {
    let mut class = fixture(&[0x0021, 0x000f], &["[I"], "I", 1, &[]);
    class.methods[0].access_flags |= 0x80;
    let s = source(&class);
    assert!(s.contains("answer(int... p0)"), "{s}");
    class.methods[0].parameters = vec![Arc::from("I")];
    assert!(native_java::render_method("sample.Hello", &class, &class.methods[0]).is_err());
    let mut class = fixture(&[0x000f], &["I"], "I", 1, &[]);
    class.methods[0].access_flags |= 0x40 | 0x1000;
    let code = native_java::render_method("sample.Hello", &class, &class.methods[0]).unwrap();
    assert!(code.source.contains("// DEX bridge method"));
    assert!(
        code.links
            .iter()
            .any(|l| l.label == "sample.Hello.answer(I)I")
    );
}

#[test]
fn filled_arrays_preserve_element_order_and_result_register() {
    for words in [
        vec![0x2024, 0, 0x0010, 0x000c, 0x0011],
        vec![0x0225, 0, 0, 0x000c, 0x0011],
    ] {
        let class = fixture(&words, &["I", "I"], "[I", 2, &["[I"]);
        let s = source(&class);
        assert!(s.contains("new int[] {p0, p1}"), "{s}");
        assert!(s.contains("return v0;"));
    }
    let class = fixture(
        &[0x2024, 0, 0x0010, 0x000c, 0x0011],
        &["Ljava/lang/String;", "Ljava/lang/Object;"],
        "[Ljava/lang/Object;",
        2,
        &["[Ljava/lang/Object;"],
    );
    assert!(source(&class).contains("new java.lang.Object[] {p0, p1}"));
    for words in [
        vec![0x6024, 0, 0],
        vec![0x0225, 0, 10],
        vec![0x2024, 1, 0x0010],
        vec![0x2024, 0, 0x0010, 0x000a, 0x000f],
    ] {
        let class = fixture(&words, &["I", "I"], "I", 2, &["[I"]);
        assert!(native_java::render_method("sample.Hello", &class, &class.methods[0]).is_err());
    }
}

#[test]
fn narrowing_conversions_have_java_signed_and_unsigned_semantics() {
    for (op, ty, cast) in [
        (0x8d, "B", "byte"),
        (0x8e, "C", "char"),
        (0x8f, "S", "short"),
    ] {
        let class = fixture(&[op, 0x000f], &["I"], ty, 1, &[]);
        let s = source(&class);
        assert!(s.contains(&format!("{cast} v0 = ({cast}) (p0);")), "{s}");
        assert!(s.contains("return v0;"));
    }
}
