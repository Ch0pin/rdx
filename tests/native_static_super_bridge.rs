use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;

fn method(
    owner: &str,
    name: &str,
    flags: u32,
    parameters: Vec<Arc<str>>,
    words: Vec<u16>,
) -> DexMethod {
    DexMethod {
        declaring_type: owner.into(),
        name: name.into(),
        return_type: "I".into(),
        parameters,
        thrown_types: vec![],
        access_flags: flags,
        code: Some(DexCode {
            registers: 2,
            ins: 2,
            outs: 2,
            tries: 0,
            try_regions: vec![],
            instructions: words,
            offset: 0,
        }),
    }
}
fn fixture(bad: usize, range: bool) -> DexClass {
    let symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Parent;".into()],
        strings: vec!["ping".into()],
        protos: vec![("I".into(), vec!["I".into()])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let mut parent_method = method("Lsample/Parent;", "ping", 1, vec!["I".into()], vec![0x010f]);
    parent_method.thrown_types = vec!["Ljava/io/IOException;".into()];
    if bad == 2 {
        parent_method.access_flags = 2;
    }
    if bad == 3 {
        parent_method.access_flags = 0x401;
        parent_method.code = None;
    }
    let class = |descriptor: &str, superclass: &str, methods| DexClass {
        descriptor: descriptor.into(),
        superclass: Some(superclass.into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods,
        symbols: symbols.clone(),
    };
    let parent = class("Lsample/Parent;", "Ljava/lang/Object;", vec![parent_method]);
    let mut child = class(
        "Lsample/Child;",
        "Lsample/Parent;",
        vec![
            method(
                "Lsample/Child;",
                "bridge",
                9,
                vec![
                    if bad == 1 {
                        "Ljava/lang/Object;"
                    } else {
                        "Lsample/Child;"
                    }
                    .into(),
                    "I".into(),
                ],
                if range {
                    vec![0x0275, 0, 0, 0x000a, 0x000f]
                } else {
                    vec![0x206f, 0, 0x0010, 0x000a, 0x000f]
                },
            ),
            method(
                "Lsample/Child;",
                "ping",
                1,
                vec!["I".into()],
                vec![0x0013, 99, 0x000f],
            ),
        ],
    );
    if bad == 4 {
        child.methods[0].access_flags = 1;
        child.methods[0].parameters.remove(0);
    }
    symbols
        .hierarchy
        .set(Arc::new(
            TypeHierarchy::from_classes([&child, &parent]).unwrap(),
        ))
        .unwrap();
    child
}

#[test]
fn static_parameter_super_dispatch_uses_private_nonvirtual_source_bridge() {
    for range in [false, true] {
        let class = fixture(0, range);
        let source = native_java::render("sample.Child", &class).unwrap().source;
        assert!(source.contains("private <RdxSuperThrowable"), "{source}");
        assert!(source.contains("super.ping(p0)"), "{source}");
        assert!(source.contains("child._rdx_super_"), "{source}");
        assert!(!source.contains("child.ping("), "{source}");
    }
}
#[test]
fn wrong_receiver_and_private_or_abstract_super_targets_decline() {
    for bad in [1, 2, 3] {
        let class = fixture(bad, false);
        assert!(
            native_java::render("sample.Child", &class).is_err(),
            "accepted bad shape {bad}"
        );
    }
}

#[test]
#[ignore = "requires javac/java; root runs serially"]
fn jvm_static_bridge_preserves_super_override_null_and_checked_identity() {
    use std::{fs, process::Command};
    let base = std::env::temp_dir().join(format!("rdx-static-super-{}", std::process::id()));
    fs::create_dir_all(base.join("sample")).unwrap();
    fs::write(base.join("sample/Parent.java"),"package sample; public class Parent { static final java.io.IOException MARKER=new java.io.IOException(); static String events=\"\"; public int ping(int x) throws java.io.IOException { events+=\"P\"; if(x<0)throw MARKER; return x+10; } }").unwrap();
    fs::write(base.join("sample/Check.java"),"package sample; public class Check { public static void main(String[] args) throws Throwable { Child c=new Child(); if(Child.bridge(c,3)!=13 || c.ping(3)!=99 || !Parent.events.equals(\"P\"))throw new AssertionError(); Parent.events=\"\"; try{Child.bridge(c,-1);throw new AssertionError();}catch(Throwable error){if(error!=Parent.MARKER || !Parent.events.equals(\"P\"))throw new AssertionError(error);} Parent.events=\"\"; try{Child.bridge(null,1);throw new AssertionError();}catch(NullPointerException expected){if(!Parent.events.isEmpty())throw new AssertionError();} System.out.print(\"ok\"); } }").unwrap();
    for range in [false, true] {
        let class = fixture(0, range);
        fs::write(
            base.join("sample/Child.java"),
            native_java::render("sample.Child", &class).unwrap().source,
        )
        .unwrap();
        let compiled = Command::new("javac")
            .args([
                "-Xlint:all",
                "-Werror",
                "sample/Parent.java",
                "sample/Child.java",
                "sample/Check.java",
            ])
            .current_dir(&base)
            .output()
            .unwrap();
        assert!(
            compiled.status.success(),
            "{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        let executed = Command::new("java")
            .args(["-cp", ".", "sample.Check"])
            .current_dir(&base)
            .output()
            .unwrap();
        assert!(
            executed.status.success(),
            "{}",
            String::from_utf8_lossy(&executed.stderr)
        );
        assert_eq!(executed.stdout, b"ok");
    }
    fs::remove_dir_all(base).unwrap();
}

#[test]
fn instance_super_receiver_join_requires_exact_this_identity() {
    for other in [false, true] {
        let mut class = fixture(4, false);
        let method = &mut class.methods[0];
        method.parameters = vec!["Lsample/Child;".into(), "I".into()];
        method.thrown_types = vec!["Ljava/io/IOException;".into()];
        let code = method.code.as_mut().unwrap();
        code.registers = 4;
        code.ins = 3; // this=v1, other=v2, condition=v3
        code.instructions = vec![
            0x0338,
            4,      // if-eqz v3 ->5
            0x1007, // v0=this
            0x0328, // goto6
            0,      // unreachable nop
            if other { 0x2007 } else { 0x1007 },
            0x206f,
            0,
            0x0030, // invoke-super v0,v3
            0x000a,
            0x000f,
        ];
        let result = native_java::render_method("sample.Child", &class, &class.methods[0]);
        if other {
            assert!(result.is_err(), "other receiver must retain DEX");
        } else {
            let source = result.unwrap().source;
            assert!(source.contains("super.ping("), "{source}");
        }
    }
}
