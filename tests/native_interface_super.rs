//! Exact direct-interface invokespecial dispatch, including conservative rejection gates.
use rdx::{
    native_dex::{
        DexAnnotation, DexAnnotationDirectory, DexClass, DexCode, DexMethod, DexSymbols, DexValue,
    },
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;

fn method(owner: &str, name: &str, flags: u32, words: Option<Vec<u16>>) -> DexMethod {
    DexMethod {
        declaring_type: owner.into(),
        name: name.into(),
        return_type: "I".into(),
        parameters: vec!["Ljava/lang/Object;".into()],
        thrown_types: vec![],
        access_flags: flags,
        code: words.map(|instructions| DexCode {
            registers: 2,
            ins: 2,
            outs: 2,
            tries: 0,
            try_regions: vec![],
            instructions,
            offset: 0,
        }),
    }
}

fn class(owner: &str, flags: u32, interfaces: &[&str], methods: Vec<DexMethod>) -> DexClass {
    DexClass {
        descriptor: owner.into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: interfaces.iter().map(|s| (*s).into()).collect(),
        access_flags: flags,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods,
        symbols: Arc::new(DexSymbols::default()),
    }
}

fn fixture() -> Vec<DexClass> {
    vec![
        class(
            "Lsample/Caller;",
            1,
            &["Lsample/Default;"],
            vec![
                method(
                    "Lsample/Caller;",
                    "call",
                    1,
                    Some(vec![0x206f, 0, 0x0010, 0x000a, 0x000f]),
                ),
                method("Lsample/Caller;", "value", 1, Some(vec![0x2012, 0x000f])),
            ],
        ),
        class(
            "Lsample/Default;",
            0x601,
            &[],
            vec![method(
                "Lsample/Default;",
                "value",
                1,
                Some(vec![0x1012, 0x000f]),
            )],
        ),
    ]
}

fn install(classes: &mut [DexClass]) {
    install_metadata(classes, false);
}

fn install_metadata(classes: &mut [DexClass], generic: bool) {
    let mut symbols = DexSymbols {
        types: vec!["Lsample/Default;".into()],
        strings: vec![
            classes
                .get(1)
                .and_then(|c| c.methods.first())
                .map(|m| m.name.to_string())
                .unwrap_or_else(|| "value".into()),
        ],
        protos: vec![("I".into(), vec!["Ljava/lang/Object;".into()])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    };
    if generic {
        symbols.types.push("Ldalvik/annotation/Signature;".into());
        let value_name = symbols.strings.len() as u32;
        symbols.strings.push("value".into());
        for (index, class) in classes.iter_mut().enumerate() {
            let signature = if class.descriptor.as_ref() == "Lsample/Default;" {
                "<T:Ljava/lang/Object;>Ljava/lang/Object;".to_string()
            } else {
                format!(
                    "Ljava/lang/Object;{}",
                    class
                        .interfaces
                        .iter()
                        .map(|interface| {
                            if interface.as_ref() == "Lsample/Default;" {
                                "Lsample/Default<Ljava/lang/String;>;".to_string()
                            } else {
                                interface.to_string()
                            }
                        })
                        .collect::<String>()
                )
            };
            let signature_index = symbols.strings.len() as u32;
            symbols.strings.push(signature);
            class.annotations_offset = (index + 1) as u32;
            symbols.annotations.insert(
                class.annotations_offset,
                Arc::new(DexAnnotationDirectory {
                    class: Some(Arc::from(vec![Arc::new(DexAnnotation {
                        visibility: 2,
                        type_idx: 1,
                        elements: vec![(
                            value_name,
                            DexValue::Array(vec![DexValue::String(signature_index)]),
                        )],
                    })])),
                    fields: vec![],
                    methods: vec![],
                    parameters: vec![],
                }),
            );
        }
    }
    let symbols = Arc::new(symbols);
    for class in classes.iter_mut() {
        class.symbols = symbols.clone();
    }
    symbols
        .hierarchy
        .set(Arc::new(
            TypeHierarchy::from_classes(classes.iter()).unwrap(),
        ))
        .unwrap();
}

#[test]
fn direct_default_super_preserves_nonvirtual_dispatch_and_method_link() {
    for range in [false, true] {
        let mut classes = fixture();
        if range {
            classes[0].methods[0].code.as_mut().unwrap().instructions =
                vec![0x0275, 0, 0, 0x000a, 0x000f];
        }
        install(&mut classes);
        let output =
            native_java::render_method("sample.Caller", &classes[0], &classes[0].methods[0])
                .unwrap();
        assert!(
            output.source.contains("Default.super.value("),
            "{}",
            output.source
        );
        let link = output
            .links
            .iter()
            .find(|link| link.label == "sample.Default.value(Ljava/lang/Object;)I")
            .unwrap();
        assert_eq!(
            output
                .source
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect::<String>(),
            "value"
        );
    }
}

#[test]
fn invalid_or_unproved_interface_super_shapes_fail_closed() {
    for shape in 0..15 {
        let mut classes = fixture();
        match shape {
            // A reference argument of the caller's type is still an ordinary receiver.
            0 => {
                classes[0].methods[0].parameters[0] = "Lsample/Caller;".into();
                classes[0].methods[0].code.as_mut().unwrap().instructions[2] = 0x0011;
            }
            1 => {
                classes[0].interfaces.clear();
                classes[0].superclass = Some("Lsample/Parent;".into());
                classes.push(class("Lsample/Parent;", 1, &["Lsample/Default;"], vec![]));
            }
            2 => {
                classes[0].interfaces.push("Lsample/Specific;".into());
                classes.push(class(
                    "Lsample/Specific;",
                    0x601,
                    &["Lsample/Default;"],
                    vec![method(
                        "Lsample/Specific;",
                        "value",
                        1,
                        Some(vec![0x3012, 0x000f]),
                    )],
                ));
            }
            3 => classes[1].methods[0].access_flags = 9,
            4 => classes[1].methods[0].access_flags = 2,
            5 => {
                classes[1].methods[0].access_flags = 0x401;
                classes[1].methods[0].code = None;
            }
            6 => {
                classes.pop();
            }
            7 => {
                let mut overload =
                    method("Lsample/Default;", "value", 1, Some(vec![0x1012, 0x000f]));
                overload.parameters[0] = "Ljava/lang/String;".into();
                classes[1].methods.push(overload);
            }
            8 => classes[1].access_flags = 1,
            9 => classes[1].methods[0].access_flags |= 0x10,
            10 => classes[1].methods[0].access_flags |= 0x20,
            11 => classes[1].methods[0].access_flags |= 0x20000,
            12 => classes[1].methods[0]
                .thrown_types
                .push("Ljava/lang/Exception;".into()),
            13 => classes[1].methods[0].code = None,
            14 => {
                let mut varargs = method(
                    "Lsample/Default;",
                    "value",
                    0x81,
                    Some(vec![0x1012, 0x000f]),
                );
                varargs.parameters.push("[Ljava/lang/Object;".into());
                classes[1].methods.push(varargs);
            }
            _ => unreachable!(),
        }
        install(&mut classes);
        assert!(
            native_java::render_method("sample.Caller", &classes[0], &classes[0].methods[0])
                .is_err(),
            "accepted shape {shape}"
        );
    }
}

#[test]
fn inherited_default_uses_direct_interface_qualifier_and_preserves_raw_owner_link() {
    let mut classes = fixture();
    classes[0].interfaces = vec!["Lsample/Direct;".into()];
    classes.push(class(
        "Lsample/Direct;",
        0x601,
        &["Lsample/Default;"],
        vec![],
    ));
    install(&mut classes);
    let output =
        native_java::render_method("sample.Caller", &classes[0], &classes[0].methods[0]).unwrap();
    assert!(
        output.source.contains("Direct.super.value("),
        "{}",
        output.source
    );
    assert!(
        output
            .links
            .iter()
            .any(|link| link.label == "sample.Default.value(Ljava/lang/Object;)I")
    );
}

#[test]
fn interface_default_can_qualify_its_own_direct_parent_default() {
    let mut classes = fixture();
    classes[0].access_flags = 0x601;
    install(&mut classes);
    let output =
        native_java::render_method("sample.Caller", &classes[0], &classes[0].methods[0]).unwrap();
    assert!(
        output.source.contains("Default.super.value("),
        "{}",
        output.source
    );
}

#[test]
fn interface_super_argument_widening_keeps_one_descriptor_cast() {
    let mut classes = fixture();
    classes[0].methods[0].parameters[0] = "Ljava/lang/String;".into();
    install(&mut classes);
    let output =
        native_java::render_method("sample.Caller", &classes[0], &classes[0].methods[0]).unwrap();
    let normalized = output.source.replace("java.lang.Object", "Object");
    assert_eq!(normalized.matches("((Object)").count(), 1, "{normalized}");
    assert!(normalized.contains("Default.super.value("), "{normalized}");
}

#[test]
fn system_class_signatures_preserve_current_raw_interface_source_contract() {
    let mut classes = fixture();
    install_metadata(&mut classes, true);
    let caller = native_java::render("sample.Caller", &classes[0]).unwrap();
    let default = native_java::render("sample.Default", &classes[1]).unwrap();
    assert!(
        caller.source.contains("Default.super.value("),
        "{}",
        caller.source
    );
    assert!(
        !default.source.contains("interface Default<"),
        "{}",
        default.source
    );
    assert!(
        !caller.source.contains("Default<String>"),
        "{}",
        caller.source
    );
    assert!(!caller.source.contains("@Signature"));
}

#[test]
fn most_specific_direct_interface_can_inherit_exact_raw_default() {
    let mut classes = fixture();
    classes[0].interfaces.push("Lsample/Specific;".into());
    classes.push(class(
        "Lsample/Specific;",
        0x601,
        &["Lsample/Default;"],
        vec![],
    ));
    install(&mut classes);
    let output =
        native_java::render_method("sample.Caller", &classes[0], &classes[0].methods[0]).unwrap();
    assert!(
        output.source.contains("Specific.super.value("),
        "{}",
        output.source
    );
}

#[test]
fn cyclic_interface_graphs_and_object_default_overrides_fail_closed() {
    for above_owner in [false, true] {
        let mut classes = fixture();
        classes[0].interfaces = vec!["Lsample/Direct;".into()];
        classes.push(class(
            "Lsample/Direct;",
            0x601,
            &["Lsample/Default;", "Lsample/Loop;"],
            vec![],
        ));
        classes.push(class("Lsample/Loop;", 0x601, &["Lsample/Direct;"], vec![]));
        if above_owner {
            classes[2].interfaces = vec!["Lsample/Default;".into()];
            classes[1].interfaces.push("Lsample/Direct;".into());
        }
        install(&mut classes);
        assert!(
            native_java::render_method("sample.Caller", &classes[0], &classes[0].methods[0])
                .is_err()
        );
    }
    let mut classes = fixture();
    classes[1].methods[0].name = "equals".into();
    install(&mut classes);
    assert!(
        native_java::render_method("sample.Caller", &classes[0], &classes[0].methods[0]).is_err()
    );
}

#[test]
fn intervening_default_or_abstract_redeclaration_rejects_ancestor_retargeting() {
    for flags in [1, 0x401] {
        let mut classes = fixture();
        classes[0].interfaces = vec!["Lsample/Direct;".into()];
        let body = (flags == 1).then(|| vec![0x3012, 0x000f]);
        classes.push(class(
            "Lsample/Direct;",
            0x601,
            &["Lsample/Default;"],
            vec![method("Lsample/Direct;", "value", flags, body)],
        ));
        install(&mut classes);
        assert!(
            native_java::render_method("sample.Caller", &classes[0], &classes[0].methods[0])
                .is_err()
        );
    }
}

#[test]
fn competing_inherited_branch_and_unresolved_branch_reject_retargeting() {
    for resolved in [false, true] {
        let mut classes = fixture();
        classes[0].interfaces = vec!["Lsample/Direct;".into()];
        classes.push(class(
            "Lsample/Direct;",
            0x601,
            &["Lsample/Default;", "Lsample/Other;"],
            vec![],
        ));
        if resolved {
            classes.push(class(
                "Lsample/Other;",
                0x601,
                &[],
                vec![method(
                    "Lsample/Other;",
                    "value",
                    1,
                    Some(vec![0x3012, 0x000f]),
                )],
            ));
        }
        install(&mut classes);
        assert!(
            native_java::render_method("sample.Caller", &classes[0], &classes[0].methods[0])
                .is_err()
        );
    }
}

#[test]
#[ignore = "requires javac and java; parent runs serially"]
fn jvm_default_super_chooses_default_over_receiver_override() {
    jvm_check(false, false, false, false, false);
    jvm_check(true, false, false, false, false);
    jvm_check(false, true, false, false, false);
    jvm_check(false, false, true, false, false);
    jvm_check(false, false, false, true, false);
    jvm_check(true, false, false, false, true);
}

fn jvm_check(
    inherited: bool,
    caller_interface: bool,
    redundant_parent: bool,
    ancestor_overload: bool,
    generic_metadata: bool,
) {
    use std::{fs, process::Command};
    let mut classes = fixture();
    if caller_interface {
        classes[0].access_flags = 0x601;
    }
    if ancestor_overload {
        classes[0].methods[0].parameters[0] = "Ljava/lang/String;".into();
        classes[1].interfaces.push("Lsample/Parent;".into());
        let mut overload = method("Lsample/Parent;", "value", 1, Some(vec![0x3012, 0x000f]));
        overload.parameters[0] = "Ljava/lang/String;".into();
        classes.push(class("Lsample/Parent;", 0x601, &[], vec![overload]));
    }
    if inherited || redundant_parent {
        if redundant_parent {
            classes[0].interfaces.push("Lsample/Direct;".into());
        } else {
            classes[0].interfaces = vec!["Lsample/Direct;".into()];
        }
        classes.push(class(
            "Lsample/Direct;",
            0x601,
            &["Lsample/Default;"],
            vec![],
        ));
    }
    install_metadata(&mut classes, generic_metadata);
    let dir = std::env::temp_dir().join(format!("rdx-interface-super-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    for (name, class) in [("Caller", &classes[0]), ("Default", &classes[1])] {
        let emitted = native_java::render(&format!("sample.{name}"), class).unwrap();
        fs::write(dir.join(format!("sample/{name}.java")), emitted.source).unwrap();
    }
    for class in classes.iter().skip(2) {
        let name = class
            .descriptor
            .strip_prefix("Lsample/")
            .unwrap()
            .strip_suffix(';')
            .unwrap();
        let emitted = native_java::render(&format!("sample.{name}"), class).unwrap();
        fs::write(dir.join(format!("sample/{name}.java")), emitted.source).unwrap();
    }
    let constructor = if caller_interface {
        "new Caller() {}"
    } else {
        "new Caller()"
    };
    fs::write(dir.join("sample/Check.java"), format!("package sample; public class Check {{ public static void main(String[] args) {{ Caller c={constructor}; if(c.call(null)!=1 || c.call(\"subtype\")!=1 || c.value((Object)null)!=2) throw new AssertionError(); System.out.print(\"ok\"); }} }}")).unwrap();
    let javac = Command::new("javac")
        .args([
            "sample/Default.java",
            "sample/Caller.java",
            "sample/Check.java",
        ])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        javac.status.success(),
        "{}",
        String::from_utf8_lossy(&javac.stderr)
    );
    let java = Command::new("java")
        .args(["-cp", ".", "sample.Check"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        java.status.success(),
        "{}",
        String::from_utf8_lossy(&java.stderr)
    );
    assert_eq!(java.stdout, b"ok");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "requires RDX_TEST_APK with pinned Airbnb build; parent runs serially"]
fn airbnb_router_default_bridges_render_and_resolve_original_owner_links() {
    use rdx::engine::{DecompilerEngine, NativeEngine};
    let path = std::env::var_os("RDX_TEST_APK").expect("RDX_TEST_APK required");
    let name =
        "com.airbnb.android.feat.a11ysettings.nav.A11ysettingsRouters$A11ySettingsScreenRouter";
    let owner = "com.airbnb.android.base.trio.airbnb.navigation.InfraTrioRouter";
    let mut engine = NativeEngine::start().unwrap();
    engine.open(std::path::Path::new(&path)).unwrap();
    let output = engine.decompile_with_metadata(name).unwrap();
    assert!(!output.source.contains(".method "), "{}", output.source);
    assert_eq!(
        output.source.matches("TrioRouter.super.").count(),
        2,
        "{}",
        output.source
    );
    assert!(
        !output.source.contains("((Parcelable) ((Parcelable)"),
        "{}",
        output.source
    );
    for signature in [
        "ȷ(Lem7;Landroid/os/Parcelable;Lcom/airbnb/android/base/trio/navigation/TrioPresentation;Lcom/airbnb/android/base/trio/TrioUUID;)Lxmx0;",
        "ɍ(Landroid/os/Parcelable;Lcom/airbnb/android/base/trio/TrioUUID;Lcom/airbnb/android/base/trio/navigation/TrioPresentation;)Lxmx0;",
    ] {
        let label = format!("{owner}.{signature}");
        let link = output
            .links
            .iter()
            .find(|link| link.label == label)
            .expect("raw default owner link");
        let target = engine
            .resolve_usage_target(name, link.start, &output.source_hash)
            .unwrap();
        assert_eq!(target.id, label);
    }
}

#[test]
fn loaded_default_can_shadow_complete_platform_leaf_ancestor() {
    let mut classes = fixture();
    classes[1].interfaces.push("Landroid/os/Parcelable;".into());
    install(&mut classes);
    let source = native_java::render_method("sample.Caller", &classes[0], &classes[0].methods[0])
        .unwrap()
        .source;
    assert!(source.contains("Default.super.value("), "{source}");
}

#[test]
fn unresolved_platform_ancestor_still_rejects_interface_super() {
    let mut classes = fixture();
    classes[1]
        .interfaces
        .push("Landroid/os/UnresolvedInterface;".into());
    install(&mut classes);
    assert!(
        native_java::render_method("sample.Caller", &classes[0], &classes[0].methods[0]).is_err()
    );
}
