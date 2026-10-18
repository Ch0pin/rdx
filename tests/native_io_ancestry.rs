use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;
const IO: &str = "Ljava/io/IOException;";
fn class(owner: &str, parent: &str) -> DexClass {
    DexClass {
        symbols: Arc::new(DexSymbols::default()),
        descriptor: owner.into(),
        superclass: Some(parent.into()),
        interfaces: vec![],
        access_flags: 0x11,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![],
    }
}
fn fixture(name: &str, ret: &str, params: &[&str], flags: u32, parent: &str) -> DexClass {
    let mut c = class("Lsample/Stream;", parent);
    c.symbols = Arc::new(DexSymbols {
        types: vec![IO.into(), "Lsample/Effects;".into()],
        strings: vec!["<init>".into(), "before".into()],
        protos: vec![("V".into(), vec![])],
        methods: vec![(0, 0, 0), (1, 0, 1)],
        ..Default::default()
    });
    let ins = params
        .iter()
        .map(|p| if matches!(*p, "J" | "D") { 2 } else { 1 })
        .sum::<u16>()
        + u16::from(flags & 8 == 0);
    c.methods.push(DexMethod {
        declaring_type: c.descriptor.clone(),
        name: name.into(),
        return_type: ret.into(),
        parameters: params.iter().map(|p| Arc::from(*p)).collect(),
        thrown_types: vec![],
        access_flags: flags,
        code: Some(DexCode {
            registers: ins + 1,
            ins,
            outs: 1,
            tries: 0,
            try_regions: vec![],
            instructions: vec![0x0071, 1, 0, 0x0022, 0, 0x1070, 0, 0, 0x0027],
            offset: 0,
        }),
    });
    c
}
fn declaration(m: &DexMethod) -> DexMethod {
    DexMethod {
        declaring_type: m.declaring_type.clone(),
        name: m.name.clone(),
        return_type: m.return_type.clone(),
        parameters: m.parameters.clone(),
        thrown_types: m.thrown_types.clone(),
        access_flags: m.access_flags,
        code: None,
    }
}
fn permits(c: &DexClass, extra: &[&DexClass]) -> bool {
    let h = TypeHierarchy::from_classes(std::iter::once(c).chain(extra.iter().copied())).unwrap();
    h.permits_inferred_checked_throw(c, &c.methods[0], IO)
}
#[test]
fn local_absence_walks_to_matching_inherited_method() {
    let c = fixture("readNBytes", "[B", &["I"], 1, "Ljava/io/FilterInputStream;");
    assert!(permits(&c, &[]));
    let h = TypeHierarchy::from_classes([&c]).unwrap();
    assert!(h.inherited_io_exception(&c, &c.methods[0]));
    c.symbols.hierarchy.set(Arc::new(h)).unwrap();
    let src = native_java::render_method("sample.Stream", &c, &c.methods[0])
        .unwrap()
        .source;
    assert!(src.contains("throws java.io.IOException"), "{src}");
    assert!(
        src.find("Effects.before()").unwrap() < src.find("new java.io.IOException").unwrap(),
        "{src}"
    );
}
#[test]
fn absent_everywhere_allows_unique_contract_with_safe_callers() {
    let c = fixture("unique", "V", &[], 1, "Ljava/io/FilterInputStream;");
    assert!(permits(&c, &[]));
    let h = TypeHierarchy::from_classes([&c]).unwrap();
    assert!(!h.inherited_io_exception(&c, &c.methods[0]));
    c.symbols.hierarchy.set(Arc::new(h)).unwrap();
    assert!(
        native_java::render_method("sample.Stream", &c, &c.methods[0])
            .unwrap()
            .source
            .contains("throws java.io.IOException")
    );
}
#[test]
fn unknown_ancestor_and_cycles_remain_unproved() {
    let mut c = fixture("unique", "V", &[], 1, "Ljava/io/FilterInputStream;");
    c.interfaces.push("Lvendor/Unknown;".into());
    assert!(!permits(&c, &[]));
    let mut a = class("Lsample/A;", "Lsample/B;");
    let b = class("Lsample/B;", "Lsample/A;");
    a.interfaces.push("Ljava/io/Closeable;".into());
    c.superclass = Some(a.descriptor.clone());
    c.interfaces.clear();
    assert!(!permits(&c, &[&a, &b]));
}
#[test]
fn inherited_incompatible_interface_and_duplicate_signatures_reject() {
    let mut c = fixture("unique", "V", &[], 1, "Ljava/io/FilterInputStream;");
    let mut i = class("Lsample/Contract;", "Ljava/lang/Object;");
    i.access_flags = 0x601;
    let mut m = declaration(&c.methods[0]);
    m.declaring_type = i.descriptor.clone();
    m.code = None;
    m.access_flags = 0x401;
    i.methods.push(declaration(&m));
    c.interfaces.push(i.descriptor.clone());
    assert!(!permits(&c, &[&i]));
    i.methods[0].thrown_types = vec![IO.into()];
    assert!(permits(&c, &[&i]));
    i.methods.push(m);
    assert!(!permits(&c, &[&i]));
}
#[test]
fn loaded_sdk_parent_shadow_is_examined_after_matching_local_contract() {
    let c = fixture("read", "I", &[], 1, "Ljava/io/FilterInputStream;");
    let mut shadow = class("Ljava/io/InputStream;", "Ljava/lang/Object;");
    let mut m = declaration(&c.methods[0]);
    m.declaring_type = shadow.descriptor.clone();
    m.code = None;
    shadow.methods.push(m);
    assert!(!permits(&c, &[&shadow]));
}
#[test]
fn return_static_object_and_constructor_rules_stay_exact() {
    for (name, ret, params, flags) in [
        ("readNBytes", "I", vec!["I"], 1),
        ("readNBytes", "[B", vec!["I"], 9),
        ("nullInputStream", "Ljava/io/InputStream;", vec![], 9),
        ("getClass", "Ljava/lang/Class;", vec![], 1),
        ("mark", "V", vec!["I"], 1),
    ] {
        let c = fixture(name, ret, &params, flags, "Ljava/io/FilterInputStream;");
        assert!(!permits(&c, &[]), "{name} {ret} {flags}");
    }
    let c = fixture("<init>", "V", &[], 1, "Ljava/io/FilterInputStream;");
    assert!(permits(&c, &[]));
    assert!(
        !TypeHierarchy::from_classes([&c])
            .unwrap()
            .inherited_io_exception(&c, &c.methods[0])
    );
}
#[test]
fn uncaught_loaded_caller_keeps_unique_declaration_blocked() {
    let c = fixture("unique", "V", &[], 1, "Ljava/io/FilterInputStream;");
    let mut caller = class("Lsample/Caller;", "Ljava/lang/Object;");
    caller.symbols = Arc::new(DexSymbols {
        types: vec![c.descriptor.clone()],
        strings: vec!["unique".into()],
        protos: vec![("V".into(), vec![])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    caller.methods.push(DexMethod {
        declaring_type: caller.descriptor.clone(),
        name: "run".into(),
        return_type: "V".into(),
        parameters: vec![c.descriptor.clone()],
        thrown_types: vec![],
        access_flags: 9,
        code: Some(DexCode {
            registers: 1,
            ins: 1,
            outs: 1,
            tries: 0,
            try_regions: vec![],
            instructions: vec![0x106e, 0, 0, 0x000e],
            offset: 0,
        }),
    });
    assert!(!permits(&c, &[&caller]));
}
#[test]
#[ignore = "requires javac and java"]
fn jvm_exact_unique_throw_keeps_identity_and_preceding_effect_order() {
    use std::{fs, process::Command};
    struct Temp(std::path::PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let mut c = fixture("unique", "V", &[IO], 1, "Ljava/io/FilterInputStream;");
    c.methods[0].code = Some(DexCode {
        registers: 2,
        ins: 2,
        outs: 0,
        tries: 0,
        try_regions: vec![],
        instructions: vec![0x0071, 1, 0, 0x0127],
        offset: 0,
    });
    let h = TypeHierarchy::from_classes([&c]).unwrap();
    c.symbols.hierarchy.set(Arc::new(h)).unwrap();
    let emitted = native_java::render_method("sample.Stream", &c, &c.methods[0])
        .unwrap()
        .source;
    let inherited = fixture("readNBytes", "[B", &["I"], 1, "Ljava/io/FilterInputStream;");
    let inherited_h = TypeHierarchy::from_classes([&inherited]).unwrap();
    inherited
        .symbols
        .hierarchy
        .set(Arc::new(inherited_h))
        .unwrap();
    let inherited_source =
        native_java::render_method("sample.Stream", &inherited, &inherited.methods[0])
            .unwrap()
            .source;
    let t = Temp(std::env::temp_dir().join(format!(
            "rdx-io-ancestry-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )));
    fs::create_dir_all(t.0.join("sample")).unwrap();
    let java = format!(
        r#"package sample;
import java.io.*;
final class Stream extends FilterInputStream {{
 Stream() {{super(new ByteArrayInputStream(new byte[0]));}}
 {emitted}
 {inherited_source}
}}
final class Effects {{static Throwable failure;static int calls;static void before() {{calls++;if(failure instanceof RuntimeException)throw (RuntimeException)failure;if(failure instanceof Error)throw (Error)failure;}}}}
public class Verify {{public static void main(String[] args) throws Exception {{
 IOException supplied=new IOException("original");Throwable[] faults={{null,new IllegalStateException("before"),new AssertionError("before")}};
 for(Throwable fault:faults) {{Effects.calls=0;Effects.failure=fault;Throwable seen=null;try {{new Stream().unique(supplied);}}catch(Throwable e){{seen=e;}}if(seen!=(fault==null?supplied:fault)||Effects.calls!=1)throw new AssertionError("identity/order");}}
 Effects.calls=0;Effects.failure=null;Throwable inheritedSeen=null;try{{new Stream().readNBytes(2);}}catch(Throwable e){{inheritedSeen=e;}}if(!(inheritedSeen instanceof IOException)||Effects.calls!=1)throw new AssertionError("inherited IO contract");
 System.out.println("3 identity/order cases passed");
}}}}
"#
    );
    let src = t.0.join("sample/Verify.java");
    fs::write(&src, java).unwrap();
    let compile = Command::new("javac")
        .arg("-d")
        .arg(&t.0)
        .arg(&src)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}\n{emitted}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new("java")
        .arg("-cp")
        .arg(&t.0)
        .arg("sample.Verify")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(String::from_utf8_lossy(&run.stdout).contains("3 identity/order cases passed"));
}

#[test]
fn repeated_object_and_marker_leaves_form_valid_diamonds() {
    let mut c = fixture("unique", "V", &[], 1, "Ljava/lang/Object;");
    let mut a = class("Lsample/Left;", "Ljava/lang/Object;");
    let mut b = class("Lsample/Right;", "Ljava/lang/Object;");
    a.interfaces = vec![
        "Ljava/io/Serializable;".into(),
        "Ljava/lang/Cloneable;".into(),
    ];
    b.interfaces = a.interfaces.clone();
    c.interfaces = vec![a.descriptor.clone(), b.descriptor.clone()];
    assert!(permits(&c, &[&a, &b]));
    c.interfaces.reverse();
    assert!(permits(&c, &[&a, &b]));
}
#[test]
fn incompatible_branch_rejects_both_traversal_orders() {
    let mut c = fixture("unique", "V", &[], 1, "Ljava/io/FilterInputStream;");
    let mut a = class("Lsample/Allowed;", "Ljava/lang/Object;");
    let mut b = class("Lsample/Forbidden;", "Ljava/lang/Object;");
    a.access_flags = 0x601;
    b.access_flags = 0x601;
    let mut yes = declaration(&c.methods[0]);
    yes.declaring_type = a.descriptor.clone();
    yes.thrown_types = vec![IO.into()];
    yes.access_flags = 0x401;
    a.methods.push(yes);
    let mut no = declaration(&c.methods[0]);
    no.declaring_type = b.descriptor.clone();
    no.access_flags = 0x401;
    b.methods.push(no);
    c.interfaces = vec![a.descriptor.clone(), b.descriptor.clone()];
    assert!(!permits(&c, &[&a, &b]));
    c.interfaces.reverse();
    assert!(!permits(&c, &[&a, &b]));
}
