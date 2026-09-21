//! Renderer-level regressions for project-wide exception ancestry.
use rdx::{
    native_dex::{self, DexClass, DexSymbols, DexTryRegion},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;

fn definition(descriptor: &str, superclass: &str) -> DexClass {
    DexClass {
        symbols: Arc::new(DexSymbols::default()),
        descriptor: Arc::from(descriptor),
        superclass: Some(Arc::from(superclass)),
        interfaces: Vec::new(),
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: Vec::new(),
        fields: Vec::new(),
        methods: Vec::new(),
    }
}

fn method_fixture(parameter: &str, declared: &[&str], instructions: Vec<u16>) -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    let method = &mut class.methods[0];
    method.name = "hierarchyTest".into();
    method.access_flags = 9;
    method.parameters = vec![Arc::from(parameter)];
    method.return_type = "V".into();
    method.thrown_types = declared.iter().copied().map(Arc::from).collect();
    let code = method.code.as_mut().unwrap();
    code.registers = 1;
    code.ins = 1;
    code.instructions = instructions;
    code.tries = 0;
    code.try_regions.clear();
    class
}

fn attach(class: &DexClass, definitions: &[DexClass]) {
    let hierarchy = Arc::new(TypeHierarchy::from_classes(definitions.iter()).unwrap());
    class.symbols.hierarchy.set(hierarchy).unwrap();
}

fn render(class: &DexClass) -> anyhow::Result<rdx::engine::DecompiledCode> {
    native_java::render_method("sample.Hello", class, &class.methods[0])
}

fn with_two_catches(class: &mut DexClass, first: &str, second: &str) {
    let code = class.methods[0].code.as_mut().unwrap();
    code.instructions = vec![0x0027, 0x000d, 0x000e, 0x000d, 0x000e];
    code.tries = 1;
    code.try_regions = vec![DexTryRegion {
        start: 0,
        end: 1,
        catches: Arc::from(vec![
            (Some(Arc::from(first)), 1),
            (Some(Arc::from(second)), 3),
        ]),
    }];
}

#[test]
fn transitive_custom_runtime_exception_renders_throw_and_catch() {
    let base = definition("Lapp/BaseRuntime;", "Ljava/lang/RuntimeException;");
    let leaf = definition("Lapp/LeafRuntime;", "Lapp/BaseRuntime;");
    let mut class = method_fixture("Lapp/LeafRuntime;", &[], vec![0x0027, 0x000d, 0x000e]);
    let code = class.methods[0].code.as_mut().unwrap();
    code.tries = 1;
    code.try_regions = vec![DexTryRegion {
        start: 0,
        end: 1,
        catches: Arc::from(vec![(Some(Arc::from("Lapp/BaseRuntime;")), 1)]),
    }];
    attach(&class, &[base, leaf]);

    let source = render(&class).unwrap().source;
    assert!(source.contains("throw p0;"), "{source}");
    assert!(source.contains("catch (app.BaseRuntime "), "{source}");
}

#[test]
fn custom_io_exception_renders_declared_header_and_throw() {
    let disk = definition("Lapp/DiskFailure;", "Ljava/io/IOException;");
    let class = method_fixture("Lapp/DiskFailure;", &["Lapp/DiskFailure;"], vec![0x0027]);
    attach(&class, &[disk]);

    let source = render(&class).unwrap().source;
    assert!(source.contains("throws app.DiskFailure"), "{source}");
    assert!(source.contains("throw p0;"), "{source}");
}

#[test]
fn custom_nonthrowable_inheritance_still_falls_back() {
    let wrong = definition("Lapp/LooksExceptional;", "Ljava/lang/Object;");
    let class = method_fixture(
        "Lapp/LooksExceptional;",
        &["Lapp/LooksExceptional;"],
        vec![0x0027],
    );
    attach(&class, &[wrong]);

    assert!(render(&class).is_err());
}

#[test]
fn custom_catches_require_subclass_before_parent() {
    let base = definition("Lapp/BaseRuntime;", "Ljava/lang/RuntimeException;");
    let leaf = definition("Lapp/LeafRuntime;", "Lapp/BaseRuntime;");
    let mut ordered = method_fixture("Lapp/LeafRuntime;", &[], Vec::new());
    with_two_catches(&mut ordered, "Lapp/LeafRuntime;", "Lapp/BaseRuntime;");
    attach(&ordered, &[base, leaf]);
    assert!(render(&ordered).is_ok());

    let base = definition("Lapp/BaseRuntime;", "Ljava/lang/RuntimeException;");
    let leaf = definition("Lapp/LeafRuntime;", "Lapp/BaseRuntime;");
    let mut shadowed = method_fixture("Lapp/LeafRuntime;", &[], Vec::new());
    with_two_catches(&mut shadowed, "Lapp/BaseRuntime;", "Lapp/LeafRuntime;");
    attach(&shadowed, &[base, leaf]);
    assert!(render(&shadowed).is_err());
}
