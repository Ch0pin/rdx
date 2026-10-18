//! Allocation stays before an independent call; only unobservable Object initialization moves.
use rdx::{
    native_dex::{DexClass, DexCode, DexField, DexMethod, DexSymbols},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;
fn method(
    owner: &str,
    name: &str,
    ret: &str,
    registers: u16,
    ins: u16,
    words: Vec<u16>,
) -> DexMethod {
    DexMethod {
        declaring_type: owner.into(),
        name: name.into(),
        return_type: ret.into(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: if name == "<init>" { 0x10001 } else { 9 },
        code: Some(DexCode {
            registers,
            ins,
            outs: 1,
            tries: 0,
            try_regions: vec![],
            instructions: words,
            offset: 0,
        }),
    }
}
fn fixture() -> Vec<DexClass> {
    let symbols = Arc::new(DexSymbols {
        types: vec![
            "Ljava/lang/Object;".into(),
            "Lsample/Leaf;".into(),
            "I".into(),
            "Lsample/Source;".into(),
        ],
        strings: vec!["<init>".into(), "a".into(), "read".into()],
        protos: vec![("V".into(), vec![]), ("Ljava/lang/Object;".into(), vec![])],
        methods: vec![(0, 0, 0), (3, 1, 2)],
        fields: vec![(1, 0, 1)],
        ..Default::default()
    });
    let class = |name: &str, methods| DexClass {
        descriptor: name.into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods,
        symbols: symbols.clone(),
    };
    let mut caller = class(
        "Lsample/Caller;",
        vec![method(
            "Lsample/Caller;",
            "run",
            "Lsample/Leaf;",
            3,
            1,
            vec![0x0022, 1, 0x106e, 1, 2, 0x010c, 0x1070, 0, 0, 0x0011],
        )],
    );
    caller.methods[0].parameters = vec!["Lsample/Source;".into()];
    let mut leaf = class(
        "Lsample/Leaf;",
        vec![method(
            "Lsample/Leaf;",
            "<init>",
            "V",
            2,
            1,
            vec![0x1070, 0, 1, 0x0012, 0x105b, 0, 0x000e],
        )],
    );
    leaf.fields.push(DexField {
        declaring_type: leaf.descriptor.clone(),
        name: "a".into(),
        field_type: "Ljava/lang/Object;".into(),
        access_flags: 1,
        is_static: false,
    });
    vec![caller, leaf]
}
fn install(c: &[DexClass]) -> Arc<TypeHierarchy> {
    let h = Arc::new(TypeHierarchy::from_classes(c).unwrap());
    for k in c {
        let _ = k.symbols.hierarchy.set(h.clone());
    }
    h
}
fn render(c: &[DexClass]) -> anyhow::Result<rdx::engine::DecompiledCode> {
    install(c);
    native_java::render_method("sample.Caller", &c[0], &c[0].methods[0])
}

#[test]
fn fresh_default_initialization_preserves_allocation_then_call() {
    let source = render(&fixture()).unwrap().source;
    assert!(
        source.find("new sample.Leaf()").unwrap() < source.find(".read()").unwrap(),
        "{source}"
    );
    assert_eq!(source.matches("new sample.Leaf()").count(), 1);
    assert!(!source.contains("Object.<init>"));
}

#[test]
fn optimizer_erased_noarg_constructor_is_recovered_with_its_class() {
    let mut classes = fixture();
    let constructor = &mut classes[1].methods[0];
    constructor.parameters = vec!["I".into()];
    let code = constructor.code.as_mut().unwrap();
    code.ins = 2;
    code.instructions = vec![0x1070, 0, 0, 0x000e];
    let source = render(&classes).unwrap().source;
    assert!(source.find("new sample.Leaf()").unwrap() < source.find(".read()").unwrap());
    let leaf = native_java::render("sample.Leaf", &classes[1])
        .unwrap()
        .source;
    assert!(leaf.contains("public Leaf()"), "{leaf}");
    assert!(!leaf.contains(".method"), "{leaf}");
}

#[test]
fn observable_constructors_finalizers_and_identity_changes_decline() {
    for variant in 0..7 {
        let mut classes = fixture();
        match variant {
            0 => classes[1].methods.push(method(
                "Lsample/Leaf;",
                "finalize",
                "V",
                1,
                1,
                vec![0x000e],
            )),
            1 => classes[1].methods[0].code.as_mut().unwrap().instructions[3] = 0x1012,
            2 => classes[0].methods[0].code.as_mut().unwrap().instructions[4] = 0,
            3 => classes[0].methods[0].code.as_mut().unwrap().instructions[5] = 0x000c,
            4 => classes[0].methods[0].code.as_mut().unwrap().instructions[8] = 1,
            5 => classes[1].fields[0].access_flags |= 0x40,
            6 => classes[1].superclass = Some("Lsample/Unknown;".into()),
            _ => unreachable!(),
        }
        assert!(render(&classes).is_err(), "variant {variant}");
    }
}

#[test]
fn generated_source_preserves_success_and_exception_identity_on_jvm() {
    let source = render(&fixture()).unwrap().source;
    let dir = std::env::temp_dir().join(format!("rdx-early-noarg-{}", std::process::id()));
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    let files = [
        ("Caller", format!("package sample; public class Caller {{ {source} }}")),
        ("Leaf", "package sample; public class Leaf { static { Source.trace += \"L\"; } public Object a; public Leaf() { a=null; } }".into()),
        ("Source", "package sample; public class Source { static String trace=\"\"; static RuntimeException error; public Object read() { trace+=\"C\"; if(error!=null) throw error; return this; } }".into()),
        ("Main", "package sample; public class Main { public static void main(String[] args) { Source s=new Source(); Leaf leaf=Caller.run(s); if(leaf.a!=null || !Source.trace.equals(\"LC\")) throw new AssertionError(Source.trace); Source.error=new RuntimeException(); try { Caller.run(s); throw new AssertionError(); } catch(RuntimeException e) { if(e!=Source.error) throw new AssertionError(); } if(!Source.trace.equals(\"LCC\")) throw new AssertionError(); try { Caller.run(null); throw new AssertionError(); } catch(NullPointerException expected) {} System.out.println(\"three-cases-pass\"); } }".into()),
    ];
    let mut javac = std::process::Command::new("javac");
    javac.arg("-d").arg(&dir);
    for (name, text) in files {
        let path = dir.join(format!("sample/{name}.java"));
        std::fs::write(&path, text).unwrap();
        javac.arg(path);
    }
    let result = javac.output().unwrap();
    assert!(
        result.status.success(),
        "{}\n{source}",
        String::from_utf8_lossy(&result.stderr)
    );
    let result = std::process::Command::new("java")
        .arg("-cp")
        .arg(&dir)
        .arg("sample.Main")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("three-cases-pass"));
}
