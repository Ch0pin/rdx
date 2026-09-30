use rdx::{
    native_dex::{self, DexClass, DexSymbols},
    native_java,
};
use std::{fs, process::Command, sync::Arc};

fn fixture(duplicate: bool, first_type: &str) -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class.descriptor = "Lsample/Chain;".into();
    class
        .methods
        .retain(|method| method.name.as_ref() == "answer");
    class.symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Source;".into(), "Lsample/Sink;".into()],
        strings: vec!["make".into(), "accept".into()],
        protos: vec![
            ("Ljava/lang/String;".into(), vec![]),
            (
                "V".into(),
                vec![first_type.into(), "Ljava/lang/String;".into()],
            ),
        ],
        methods: vec![(0, 0, 0), (1, 1, 1)],
        ..Default::default()
    });
    let method = &mut class.methods[0];
    method.declaring_type = class.descriptor.clone();
    method.name = "run".into();
    method.return_type = "V".into();
    method.parameters = vec![first_type.into()];
    method.access_flags = 9;
    let code = method.code.as_mut().unwrap();
    code.registers = 2;
    code.ins = 1;
    code.outs = 2;
    code.tries = 0;
    code.try_regions.clear();
    code.instructions = vec![
        0x0071,
        0,
        0,
        0x000c,
        0x2071,
        1,
        if duplicate { 0x0000 } else { 0x0001 },
        0x000e,
    ];
    class
}

fn wide_fixture() -> DexClass {
    let mut class = fixture(false, "Ljava/lang/Object;");
    let symbols = Arc::get_mut(&mut class.symbols).unwrap();
    symbols.protos[0].0 = "J".into();
    symbols.protos[1].1[1] = "J".into();
    let code = class.methods[0].code.as_mut().unwrap();
    code.registers = 3;
    code.outs = 3;
    code.instructions = vec![0x0071, 0, 0, 0x000b, 0x3071, 1, 0x0102, 0x000e];
    class
}

fn render(class: &DexClass) -> rdx::engine::DecompiledCode {
    native_java::render_method("sample.Chain", class, &class.methods[0]).unwrap()
}

#[test]
fn single_use_later_argument_preserves_bound_links_and_static_type() {
    let body = render(&fixture(false, "Ljava/lang/Object;"));
    assert!(
        body.source
            .contains("sample.Sink.accept(p0, ((java.lang.String) sample.Source.make()));"),
        "{}",
        body.source
    );
    assert!(
        !body.source.contains("java.lang.String v0 ="),
        "{}",
        body.source
    );
    for (label, spelling) in [
        ("sample.Source.make()Ljava/lang/String;", "make"),
        (
            "sample.Sink.accept(Ljava/lang/Object;Ljava/lang/String;)V",
            "accept",
        ),
    ] {
        let link = body.links.iter().find(|link| link.label == label).unwrap();
        assert_eq!(
            body.source
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect::<String>(),
            spelling
        );
    }
}

#[test]
fn repeated_use_and_prefix_conversion_retain_the_local() {
    let repeated = render(&fixture(true, "Ljava/lang/String;"));
    assert!(
        repeated
            .source
            .contains("java.lang.String v0 = sample.Source.make();"),
        "{}",
        repeated.source
    );
    let mut converted_class = fixture(false, "Ljava/lang/String;");
    Arc::get_mut(&mut converted_class.symbols).unwrap().protos[1].1[0] =
        "Ljava/lang/Object;".into();
    let converted = render(&converted_class);
    assert!(
        converted
            .source
            .contains("java.lang.String v0 = sample.Source.make();"),
        "{}",
        converted.source
    );

    let mut delayed_class = fixture(false, "Ljava/lang/Object;");
    delayed_class.methods[0]
        .code
        .as_mut()
        .unwrap()
        .instructions
        .insert(4, 0x0000);
    let delayed = render(&delayed_class);
    assert!(
        delayed
            .source
            .contains("java.lang.String v0 = sample.Source.make();"),
        "{}",
        delayed.source
    );

    let mut live_after_class = fixture(false, "Ljava/lang/Object;");
    live_after_class.methods[0].return_type = "Ljava/lang/String;".into();
    *live_after_class.methods[0]
        .code
        .as_mut()
        .unwrap()
        .instructions
        .last_mut()
        .unwrap() = 0x0011;
    let live_after = render(&live_after_class);
    assert!(
        live_after
            .source
            .contains("java.lang.String v0 = sample.Source.make();"),
        "{}",
        live_after.source
    );
}

#[test]
fn wide_result_uses_both_ssa_words_once() {
    let body = render(&wide_fixture());
    assert!(
        body.source
            .contains("sample.Sink.accept(p0, ((long) sample.Source.make()));"),
        "{}",
        body.source
    );
    assert!(!body.source.contains("long v0 ="), "{}", body.source);
    let link = body
        .links
        .iter()
        .find(|link| link.label == "sample.Source.make()J")
        .unwrap();
    assert_eq!(
        body.source
            .chars()
            .skip(link.start)
            .take(link.end - link.start)
            .collect::<String>(),
        "make"
    );
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn jvm_keeps_effect_order_exception_identity_and_overload() {
    let body = render(&fixture(false, "Ljava/lang/Object;"));
    let dir = std::env::temp_dir().join(format!("rdx-call-shrink-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(
        dir.join("sample/Chain.java"),
        format!("package sample; public class Chain {{ {} }}", body.source),
    )
    .unwrap();
    fs::write(dir.join("sample/Source.java"), r#"package sample;
public class Source {
 static int calls; static boolean fail; static final RuntimeException ERROR = new RuntimeException();
 public static String make() { calls++; if (fail) throw ERROR; return "made"; }
}"#).unwrap();
    fs::write(dir.join("sample/Sink.java"), r#"package sample;
public class Sink {
 static int calls; static Object seen;
 public static void accept(Object first, String second) { calls++; seen = first; if (!second.equals("made")) throw new AssertionError(); }
 public static void accept(Object first, Object second) { throw new AssertionError("wrong overload"); }
 public static void main(String[] args) {
  Object marker = new Object(); Chain.run(marker);
  if (Source.calls != 1 || calls != 1 || seen != marker) throw new AssertionError();
  Source.fail = true;
  try { Chain.run(marker); throw new AssertionError(); }
  catch (RuntimeException error) { if (error != Source.ERROR || Source.calls != 2 || calls != 1) throw new AssertionError(); }
 }
}"#).unwrap();
    let compile = Command::new("javac")
        .current_dir(&dir)
        .args([
            "sample/Chain.java",
            "sample/Source.java",
            "sample/Sink.java",
        ])
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new("java")
        .current_dir(&dir)
        .args(["-cp", ".", "sample.Sink"])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn generic_producer_cast_retains_original_overload() {
    let mut class = fixture(false, "Ljava/lang/Object;");
    let symbols = Arc::get_mut(&mut class.symbols).unwrap();
    symbols.protos[0].0 = "Ljava/lang/Object;".into();
    symbols.protos[1].1[1] = "Ljava/lang/Object;".into();
    let body = render(&class);
    assert!(
        body.source
            .contains("((java.lang.Object) sample.Source.make())"),
        "{}",
        body.source
    );
    let presented = native_java::render("sample.Chain", &class).unwrap();
    assert!(
        presented.source.contains("((Object) Source.make())"),
        "{}",
        presented.source
    );
    let source_link = presented
        .links
        .iter()
        .find(|link| link.label == "sample.Source.make()Ljava/lang/Object;")
        .unwrap();
    assert_eq!(
        presented
            .source
            .chars()
            .skip(source_link.start)
            .take(source_link.end - source_link.start)
            .collect::<String>(),
        "make"
    );
    let dir = std::env::temp_dir().join(format!("rdx-generic-shrink-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/Chain.java"), presented.source).unwrap();
    fs::write(dir.join("sample/Source.java"), "package sample; public class Source { @SuppressWarnings(\"unchecked\") public static <T> T make() { return (T) \"value\"; } }").unwrap();
    fs::write(dir.join("sample/Sink.java"), r#"package sample;
public class Sink {
 static String selected = "";
 public static void accept(Object first, Object second) { selected = "object"; }
 public static void accept(Object first, String second) { selected = "string"; }
 public static void main(String[] args) { Chain.run(new Object()); if (!selected.equals("object")) throw new AssertionError(selected); }
}"#).unwrap();
    let compile = Command::new("javac")
        .current_dir(&dir)
        .args([
            "sample/Chain.java",
            "sample/Source.java",
            "sample/Sink.java",
        ])
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new("java")
        .current_dir(&dir)
        .args(["-cp", ".", "sample.Sink"])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    fs::remove_dir_all(dir).unwrap();
}
