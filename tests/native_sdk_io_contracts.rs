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
fn exact_channel_reader_and_flushable_headers_are_shared_with_lookup() {
    for (parent, interfaces, name, ret, params) in [
        (
            "Ljava/lang/Object;",
            vec!["Ljava/nio/channels/ByteChannel;"],
            "close",
            "V",
            vec![],
        ),
        (
            "Ljava/lang/Object;",
            vec!["Ljava/nio/channels/ReadableByteChannel;"],
            "read",
            "I",
            vec!["Ljava/nio/ByteBuffer;"],
        ),
        (
            "Ljava/lang/Object;",
            vec!["Ljava/nio/channels/WritableByteChannel;"],
            "write",
            "I",
            vec!["Ljava/nio/ByteBuffer;"],
        ),
        (
            "Ljava/lang/Object;",
            vec!["Ljava/io/Flushable;"],
            "flush",
            "V",
            vec![],
        ),
        (
            "Ljava/io/Reader;",
            vec![],
            "read",
            "I",
            vec!["[C", "I", "I"],
        ),
        (
            "Ljava/lang/Object;",
            vec!["Ljava/lang/Readable;"],
            "read",
            "I",
            vec!["Ljava/nio/CharBuffer;"],
        ),
    ] {
        let mut c = fixture(name, ret, &params, 1, parent);
        c.interfaces = interfaces.into_iter().map(Arc::from).collect();
        let h = TypeHierarchy::from_classes([&c]).unwrap();
        assert!(
            h.inherited_io_exception(&c, &c.methods[0]),
            "{name} {parent}"
        );
        assert_eq!(
            h.call_checked_effects(&c.descriptor, name, &c.methods[0].parameters, ret, false)
                .unwrap(),
            vec![Arc::<str>::from("Ljava/lang/Throwable;")],
            "body inference remains incomplete"
        );
        c.symbols.hierarchy.set(Arc::new(h)).unwrap();
        let source = native_java::render_method("sample.Stream", &c, &c.methods[0])
            .unwrap()
            .source;
        assert!(source.contains("throws java.io.IOException"), "{source}");
    }
}
#[test]
fn local_absence_cannot_skip_inherited_no_throws_or_wrong_dispatch() {
    for (parent, interface, name, ret, params, flags) in [
        (
            "Ljava/lang/Object;",
            Some("Ljava/nio/channels/ByteChannel;"),
            "isOpen",
            "Z",
            vec![],
            1,
        ),
        (
            "Ljava/lang/Object;",
            Some("Ljava/nio/channels/ByteChannel;"),
            "close",
            "I",
            vec![],
            1,
        ),
        ("Ljava/io/Reader;", None, "close", "V", vec![], 9),
        ("Ljava/io/Reader;", None, "markSupported", "Z", vec![], 1),
        (
            "Ljava/io/Reader;",
            None,
            "nullReader",
            "Ljava/io/Reader;",
            vec![],
            9,
        ),
        (
            "Ljava/io/Reader;",
            None,
            "read",
            "J",
            vec!["[C", "I", "I"],
            1,
        ),
    ] {
        let mut c = fixture(name, ret, &params, flags, parent);
        c.interfaces = interface.into_iter().map(Arc::from).collect();
        assert!(!permits(&c, &[]), "{name} {parent} {flags}");
    }
}
#[test]
fn exhaustive_local_absence_allows_only_closed_unique_declarations() {
    let mut c = fixture("unique", "V", &[], 1, "Ljava/lang/Object;");
    c.interfaces = vec![
        "Ljava/nio/channels/ByteChannel;".into(),
        "Ljava/io/Flushable;".into(),
    ];
    assert!(permits(&c, &[]));
    c.interfaces.push("Lvendor/Channel;".into());
    assert!(!permits(&c, &[]));
}
#[test]
fn shadows_and_incompatible_interface_contracts_stay_unknown() {
    let c = fixture("read", "I", &["[C", "I", "I"], 1, "Ljava/io/Reader;");
    let shadow = class("Ljava/io/Reader;", "Ljava/lang/Object;");
    assert!(!permits(&c, &[&shadow]));
    let mut c = fixture("close", "V", &[], 1, "Ljava/lang/Object;");
    c.interfaces = vec!["Ljava/nio/channels/ByteChannel;".into()];
    let mut other = class("Lsample/Other;", "Ljava/lang/Object;");
    other.access_flags = 0x601;
    let mut m = declaration(&c.methods[0]);
    m.declaring_type = other.descriptor.clone();
    m.access_flags = 0x401;
    other.methods.push(m);
    c.interfaces.push(other.descriptor.clone());
    assert!(!permits(&c, &[&other]));
    c.interfaces.reverse();
    assert!(!permits(&c, &[&other]));
}
#[test]
fn complete_empty_body_header_matches_exact_call_contract() {
    let mut c = fixture("close", "V", &[], 1, "Ljava/lang/Object;");
    c.interfaces = vec!["Ljava/nio/channels/ByteChannel;".into()];
    c.methods[0].code.as_mut().unwrap().instructions = vec![0x000e];
    let h = TypeHierarchy::from_classes([&c]).unwrap();
    assert_eq!(
        h.call_checked_effects(&c.descriptor, "close", &[], "V", false)
            .unwrap(),
        vec![Arc::<str>::from(IO)]
    );
    c.symbols.hierarchy.set(Arc::new(h)).unwrap();
    assert!(
        native_java::render_method("sample.Stream", &c, &c.methods[0])
            .unwrap()
            .source
            .contains("throws java.io.IOException")
    );
}
#[test]
#[ignore = "requires javac and java"]
fn jvm_exact_reader_and_channel_methods_keep_fault_identity_and_order() {
    use std::{fs, process::Command};
    struct Temp(std::path::PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let t = Temp(std::env::temp_dir().join(format!(
            "rdx-sdk-io-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )));
    fs::create_dir_all(t.0.join("sample")).unwrap();
    let mut sources = Vec::new();
    for (parent, name, ret, params, interface) in [
        ("Ljava/io/Reader;", "read", "I", vec!["[C", "I", "I"], None),
        (
            "Ljava/lang/Object;",
            "close",
            "V",
            vec![],
            Some("Ljava/nio/channels/ByteChannel;"),
        ),
    ] {
        let mut c = fixture(name, ret, &params, 1, parent);
        c.interfaces = interface.into_iter().map(Arc::from).collect();
        let symbols = Arc::get_mut(&mut c.symbols).unwrap();
        symbols.strings.push("problem".into());
        symbols.fields.push((1, 0, 2));
        c.methods[0].code.as_mut().unwrap().instructions = vec![0x0071, 1, 0, 0x0062, 0, 0x0027];
        let h = TypeHierarchy::from_classes([&c]).unwrap();
        c.symbols.hierarchy.set(Arc::new(h)).unwrap();
        sources.push(
            native_java::render_method("sample.Stream", &c, &c.methods[0])
                .unwrap()
                .source,
        );
    }
    let reader = &sources[0];
    let channel = &sources[1];
    let java = format!(
        r#"package sample;
import java.io.*;import java.nio.*;import java.nio.channels.*;
final class Reading extends Reader {{public void close(){{}} {reader} }}
final class Closing implements ByteChannel {{public int read(ByteBuffer b){{return 0;}}public int write(ByteBuffer b){{return 0;}}public boolean isOpen(){{return true;}} {channel} }}
final class Effects {{static IOException problem;static Throwable failure;static int calls;static void before(){{calls++;if(failure instanceof RuntimeException)throw (RuntimeException)failure;if(failure instanceof Error)throw (Error)failure;}}}}
public class Verify {{public static void main(String[] a)throws Exception{{Effects.problem=new IOException("original");Throwable[] faults={{null,new IllegalStateException("before"),new AssertionError("before")}};for(int path=0;path<2;path++)for(Throwable fault:faults){{Effects.calls=0;Effects.failure=fault;Throwable seen=null;try{{if(path==0)new Reading().read(new char[3],0,3);else new Closing().close();}}catch(Throwable e){{seen=e;}}if(seen!=(fault==null?Effects.problem:fault)||Effects.calls!=1)throw new AssertionError("identity/order");}}System.out.println("6 exact identity/order cases passed");}}}}
"#
    );
    let source = t.0.join("sample/Verify.java");
    fs::write(&source, java).unwrap();
    let compile = Command::new("javac")
        .arg("-d")
        .arg(&t.0)
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}\n{}\n{}",
        String::from_utf8_lossy(&compile.stderr),
        reader,
        channel
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
    assert!(String::from_utf8_lossy(&run.stdout).contains("6 exact identity/order cases passed"));
}
