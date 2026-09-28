use rdx::{
    native_dex::{self, DexClass, DexMethod, DexSymbols},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::{fs, process::Command, sync::Arc};

fn fixture(source: &str, name: &str, ret: &str) -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class.descriptor = "Lsample/NumberReceiver;".into();
    class
        .methods
        .retain(|method| method.name.as_ref() == "answer");
    class.symbols = Arc::new(DexSymbols {
        types: vec!["Ljava/lang/Number;".into()],
        strings: vec![name.into()],
        protos: vec![(ret.into(), vec![])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let method = &mut class.methods[0];
    method.declaring_type = class.descriptor.clone();
    method.name = "read".into();
    method.access_flags = 9;
    method.parameters = vec![source.into()];
    method.return_type = ret.into();
    let code = method.code.as_mut().unwrap();
    code.registers = 3;
    code.ins = 1;
    code.outs = 1;
    code.tries = 0;
    code.try_regions.clear();
    code.instructions = vec![
        0x106e,
        0,
        2,
        if matches!(ret, "J" | "D") {
            0x000b
        } else {
            0x000a
        },
        if matches!(ret, "J" | "D") {
            0x0010
        } else {
            0x000f
        },
    ];
    class
}

fn attach_hierarchy(class: &DexClass, shadow: Option<DexClass>) {
    let hierarchy = if let Some(ref shadow) = shadow {
        TypeHierarchy::from_classes([class, shadow]).unwrap()
    } else {
        TypeHierarchy::from_classes([class]).unwrap()
    };
    class.symbols.hierarchy.set(Arc::new(hierarchy)).unwrap();
}

fn shadow(descriptor: &str) -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class.descriptor = descriptor.into();
    class.superclass = Some("Ljava/lang/Number;".into());
    class.methods.clear();
    class.fields.clear();
    class
}

fn declaration(owner: &str, name: &str, parameters: &[&str], ret: &str, flags: u32) -> DexMethod {
    DexMethod {
        declaring_type: owner.into(),
        name: name.into(),
        return_type: ret.into(),
        parameters: parameters.iter().map(|ty| Arc::from(*ty)).collect(),
        thrown_types: vec![],
        access_flags: flags,
        code: None,
    }
}

fn declared_class(descriptor: &str, parent: &str, methods: Vec<DexMethod>) -> DexClass {
    DexClass {
        symbols: Arc::new(DexSymbols::default()),
        descriptor: descriptor.into(),
        superclass: Some(parent.into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods,
    }
}

fn generic_caller(is_virtual: bool, overloaded: bool) -> DexClass {
    let mut caller = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    caller.descriptor = "Lsample/Caller;".into();
    caller
        .methods
        .retain(|method| method.name.as_ref() == "answer");
    caller.symbols = Arc::new(DexSymbols {
        types: vec![if is_virtual {
            "Lsample/Base;".into()
        } else {
            "Lsample/Target;".into()
        }],
        strings: vec![if is_virtual { "done" } else { "accept" }.into()],
        protos: vec![if is_virtual {
            ("V".into(), vec![])
        } else {
            ("V".into(), vec!["Lsample/Base;".into()])
        }],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let method = &mut caller.methods[0];
    method.declaring_type = caller.descriptor.clone();
    method.name = if overloaded { "callOverloaded" } else { "call" }.into();
    method.access_flags = 9;
    method.parameters = vec!["Lsample/Sub;".into()];
    method.return_type = "V".into();
    let code = method.code.as_mut().unwrap();
    code.registers = 2;
    code.ins = 1;
    code.outs = 1;
    code.tries = 0;
    code.try_regions.clear();
    code.instructions = if is_virtual {
        vec![0x106e, 0, 1, 0x000e]
    } else {
        vec![0x1071, 0, 1, 0x0012, 0x1071, 0, 0, 0x000e]
    };
    caller
}

fn generic_caller_with_hierarchy(is_virtual: bool, overloaded: bool) -> DexClass {
    let caller = generic_caller(is_virtual, overloaded);
    let base_methods = if is_virtual {
        vec![declaration("Lsample/Base;", "done", &[], "V", 1)]
    } else {
        vec![]
    };
    let base = declared_class("Lsample/Base;", "Ljava/lang/Object;", base_methods);
    let sub = declared_class("Lsample/Sub;", "Lsample/Base;", vec![]);
    let target = declared_class(
        "Lsample/Target;",
        "Ljava/lang/Object;",
        if is_virtual {
            vec![]
        } else if overloaded {
            vec![
                declaration("Lsample/Target;", "accept", &["Lsample/Base;"], "V", 9),
                declaration("Lsample/Target;", "accept", &["Lsample/Sub;"], "V", 9),
            ]
        } else {
            vec![declaration(
                "Lsample/Target;",
                "accept",
                &["Lsample/Base;"],
                "V",
                9,
            )]
        },
    );
    caller
        .symbols
        .hierarchy
        .set(Arc::new(
            TypeHierarchy::from_classes([&caller, &base, &sub, &target]).unwrap(),
        ))
        .unwrap();
    caller
}

#[test]
fn six_sdk_wrappers_use_number_primitive_conversions_without_receiver_casts() {
    for wrapper in ["Byte", "Short", "Integer", "Long", "Float", "Double"] {
        for (name, ret) in [
            ("byteValue", "B"),
            ("shortValue", "S"),
            ("intValue", "I"),
            ("longValue", "J"),
            ("floatValue", "F"),
            ("doubleValue", "D"),
        ] {
            let class = fixture(&format!("Ljava/lang/{wrapper};"), name, ret);
            attach_hierarchy(&class, None);
            let body =
                native_java::render_method("sample.NumberReceiver", &class, &class.methods[0])
                    .unwrap();
            assert!(
                body.source.contains(&format!("p0.{name}()")),
                "{}",
                body.source
            );
            assert!(
                !body.source.contains("((java.lang.Number)"),
                "{}",
                body.source
            );
            let link = body
                .links
                .iter()
                .find(|link| link.label == format!("java.lang.Number.{name}(){ret}"))
                .unwrap();
            let linked_text: String = body
                .source
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect();
            assert_eq!(linked_text, name, "{}", body.source);
        }
    }
}

#[test]
fn unknown_or_downcast_receiver_keeps_explicit_number_cast() {
    let unknown = fixture("Ljava/lang/Integer;", "intValue", "I");
    let body =
        native_java::render_method("sample.NumberReceiver", &unknown, &unknown.methods[0]).unwrap();
    assert!(
        body.source.contains("((java.lang.Number) p0).intValue()"),
        "{}",
        body.source
    );

    let downcast = fixture("Ljava/lang/Object;", "intValue", "I");
    attach_hierarchy(&downcast, None);
    let body = native_java::render_method("sample.NumberReceiver", &downcast, &downcast.methods[0])
        .unwrap();
    assert!(
        body.source.contains("((java.lang.Number) p0).intValue()"),
        "{}",
        body.source
    );
}

#[test]
fn apk_shadow_of_wrapper_or_number_keeps_explicit_cast() {
    for descriptor in ["Ljava/lang/Integer;", "Ljava/lang/Number;"] {
        let class = fixture("Ljava/lang/Integer;", "intValue", "I");
        attach_hierarchy(&class, Some(shadow(descriptor)));
        let body =
            native_java::render_method("sample.NumberReceiver", &class, &class.methods[0]).unwrap();
        assert!(
            body.source.contains("((java.lang.Number) p0).intValue()"),
            "{}",
            body.source
        );
    }
}

#[test]
fn unique_loaded_call_omits_upcast_and_typed_null_with_exact_links() {
    let caller = generic_caller_with_hierarchy(false, false);
    let body = native_java::render_method("sample.Caller", &caller, &caller.methods[0]).unwrap();
    assert!(body.source.contains("Target.accept(p0)"), "{}", body.source);
    assert!(
        body.source.contains("Target.accept(null)"),
        "{}",
        body.source
    );
    assert!(!body.source.contains("((sample.Base)"), "{}", body.source);
    let links: Vec<_> = body
        .links
        .iter()
        .filter(|link| link.label == "sample.Target.accept(Lsample/Base;)V")
        .collect();
    assert_eq!(links.len(), 2, "{:?}", body.links);
    for link in links {
        let linked_text: String = body
            .source
            .chars()
            .skip(link.start)
            .take(link.end - link.start)
            .collect();
        assert_eq!(linked_text, "accept");
    }
}

#[test]
fn overloaded_loaded_call_retains_upcast_and_typed_null() {
    let caller = generic_caller_with_hierarchy(false, true);
    let body = native_java::render_method("sample.Caller", &caller, &caller.methods[0]).unwrap();
    assert!(
        body.source.contains("Target.accept(((sample.Base) p0))"),
        "{}",
        body.source
    );
    assert!(
        body.source.contains("Target.accept(((sample.Base) null))"),
        "{}",
        body.source
    );
}

#[test]
fn proven_void_receiver_uses_subtype_local() {
    let caller = generic_caller_with_hierarchy(true, false);
    let body = native_java::render_method("sample.Caller", &caller, &caller.methods[0]).unwrap();
    assert!(body.source.contains("p0.done()"), "{}", body.source);
    assert!(!body.source.contains("((sample.Base)"), "{}", body.source);
    let link = body
        .links
        .iter()
        .find(|link| link.label == "sample.Base.done()V")
        .unwrap();
    let linked_text: String = body
        .source
        .chars()
        .skip(link.start)
        .take(link.end - link.start)
        .collect();
    assert_eq!(linked_text, "done");
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn generic_cast_cleanup_jvm_preserves_dispatch_null_and_call_order() {
    let static_caller = generic_caller_with_hierarchy(false, false);
    let virtual_caller = generic_caller_with_hierarchy(true, false);
    let static_method =
        native_java::render_method("sample.Caller", &static_caller, &static_caller.methods[0])
            .unwrap()
            .source;
    let virtual_method =
        native_java::render_method("sample.Caller", &virtual_caller, &virtual_caller.methods[0])
            .unwrap()
            .source
            .replacen("void call(", "void callVirtual(", 1);
    let directory = std::env::temp_dir().join(format!("rdx-generic-casts-{}", std::process::id()));
    fs::create_dir_all(directory.join("sample")).unwrap();
    let java = format!(
        "package sample; class Base {{ void done() {{ Caller.log += \"B\"; }} }} class Sub extends Base {{ @Override void done() {{ Caller.log += \"S\"; }} }} class Target {{ static void accept(Base value) {{ Caller.log += value == null ? \"N\" : \"V\"; }} }} public class Caller {{ static String log = \"\"; {} {} public static void main(String[] args) {{ call(new Sub()); if (!log.equals(\"VN\")) throw new AssertionError(log); callVirtual(new Sub()); if (!log.equals(\"VNS\")) throw new AssertionError(log); try {{ callVirtual(null); throw new AssertionError(); }} catch (NullPointerException expected) {{}} }} }}",
        static_method, virtual_method
    );
    fs::write(directory.join("sample/Caller.java"), java).unwrap();
    for (program, args) in [
        ("javac", vec!["sample/Caller.java"]),
        ("java", vec!["sample.Caller"]),
    ] {
        let output = Command::new(program)
            .args(args)
            .current_dir(&directory)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{program}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn number_conversion_jvm_preserves_value_and_null_exception() {
    let mut methods = String::new();
    for (wrapper, name, ret, method_name) in [
        ("Byte", "byteValue", "B", "readByte"),
        ("Short", "shortValue", "S", "readShort"),
        ("Integer", "intValue", "I", "readInteger"),
        ("Long", "longValue", "J", "readLong"),
        ("Float", "floatValue", "F", "readFloat"),
        ("Double", "doubleValue", "D", "readDouble"),
        ("Integer", "byteValue", "B", "readIntegerByte"),
    ] {
        let mut class = fixture(&format!("Ljava/lang/{wrapper};"), name, ret);
        class.methods[0].name = method_name.into();
        attach_hierarchy(&class, None);
        methods.push_str(
            &native_java::render_method("sample.NumberReceiver", &class, &class.methods[0])
                .unwrap()
                .source,
        );
    }
    let directory =
        std::env::temp_dir().join(format!("rdx-number-receiver-{}", std::process::id()));
    fs::create_dir_all(directory.join("sample")).unwrap();
    let java = format!(
        "package sample; public class NumberReceiver {{\n{}\nprivate static void npe(Runnable action) {{ try {{ action.run(); throw new AssertionError(); }} catch (NullPointerException expected) {{}} }}\npublic static void main(String[] args) {{\nif (readByte(Byte.valueOf((byte) -7)) != -7) throw new AssertionError(\"byte\");\nif (readShort(Short.valueOf((short) -300)) != -300) throw new AssertionError(\"short\");\nif (readInteger(Integer.valueOf(7)) != 7) throw new AssertionError(\"int\");\nif (readIntegerByte(Integer.valueOf(0x1234)) != 0x34) throw new AssertionError(\"overflow\");\nif (readLong(Long.valueOf(0x123456789abcdefL)) != 0x123456789abcdefL) throw new AssertionError(\"long\");\nif (Float.floatToRawIntBits(readFloat(Float.valueOf(-0.0f))) != 0x80000000) throw new AssertionError(\"float zero\");\nif (Float.floatToRawIntBits(readFloat(Float.valueOf(Float.intBitsToFloat(0x7fc12345)))) != 0x7fc12345) throw new AssertionError(\"float nan\");\nif (Double.doubleToRawLongBits(readDouble(Double.valueOf(-0.0d))) != 0x8000000000000000L) throw new AssertionError(\"double zero\");\nif (Double.doubleToRawLongBits(readDouble(Double.valueOf(Double.longBitsToDouble(0x7ff8123412341234L)))) != 0x7ff8123412341234L) throw new AssertionError(\"double nan\");\nnpe(() -> readByte(null)); npe(() -> readShort(null)); npe(() -> readInteger(null)); npe(() -> readLong(null)); npe(() -> readFloat(null)); npe(() -> readDouble(null));\n}}\n}}\n",
        methods
    );
    fs::write(directory.join("sample/NumberReceiver.java"), java).unwrap();
    for (program, args) in [
        ("javac", vec!["sample/NumberReceiver.java"]),
        ("java", vec!["sample.NumberReceiver"]),
    ] {
        let output = Command::new(program)
            .args(args)
            .current_dir(&directory)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{program}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
