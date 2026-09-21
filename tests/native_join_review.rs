//! Review regression for join discovery across an early bypass and long arm.
use rdx::{native_dex, native_java};

fn long_bypass_fixture(body_instructions: usize) -> rdx::native_dex::DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    let method = &mut class.methods[0];
    method.name = "longBypass".into();
    method.access_flags = 9;
    method.parameters = vec!["I".into()];
    method.return_type = "V".into();
    method.thrown_types.clear();

    // pc 0: if-eqz p0, pc 4
    // pc 2: if-eqz p0, tail; otherwise enter the same long arm at pc 4.
    // Each candidate in that arm is bypassed, but proving it rescans the
    // growing prefix before following the bypass edge.
    let tail = 4 + body_instructions;
    let offset = u16::try_from(tail - 2).unwrap();
    let mut words = vec![0x0038, 4, 0x0038, offset];
    words.resize(tail, 0x0000);
    words.push(0x000e);

    let code = method.code.as_mut().unwrap();
    code.registers = 1;
    code.ins = 1;
    code.instructions = words;
    code.tries = 0;
    code.try_regions.clear();
    class
}

#[test]
fn long_bypass_and_shared_tail_reconstruct_with_bounded_join_work() {
    let class = long_bypass_fixture(20_000);
    let rendered = native_java::render_method("sample.Hello", &class, &class.methods[0]);
    let source = rendered
        .unwrap_or_else(|error| panic!("valid long bypass fell back: {error:#}"))
        .source;
    assert!(source.contains("if ("), "{source}");
    assert_eq!(source.matches("return;").count(), 1, "{source}");
}
