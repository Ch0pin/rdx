//! Optional corpus regression; the APK stays outside the repository.
use rdx::{
    engine::{MethodSourceFormat, NativeEngine},
    package_container::PackageContainer,
};
#[test]
#[ignore = "requires RDX_BASECAMP_APK and RDX_BASECAMP_OUTPUT"]
fn reviewer_reconstruction_corpus() {
    let apk = std::env::var_os("RDX_BASECAMP_APK").unwrap();
    let dir = std::path::PathBuf::from(std::env::var_os("RDX_BASECAMP_OUTPUT").unwrap());
    std::fs::create_dir_all(&dir).unwrap();
    let mut engine = NativeEngine::start().unwrap();
    engine
        .open_prepared(PackageContainer::open(std::path::Path::new(&apk)).unwrap())
        .unwrap();
    for name in [
        "ze.v1",
        "ze.z3",
        "ze.r3",
        "androidx.fragment.app.h0",
        "ze.a4",
        "androidx.room.y1",
        "oj.c1",
        "oj.d1",
        "ad.r",
        "com.basecamp.shared.library.origin.feature.docsandfiles.DocsAndFilesWorker",
    ] {
        let (code, coverage) = engine
            .decompile_class_source_report(name, &false.into())
            .unwrap();
        std::fs::write(dir.join(format!("{name}.java")), &code.source).unwrap();
        std::fs::write(
            dir.join(format!("{name}.json")),
            serde_json::to_vec_pretty(&coverage).unwrap(),
        )
        .unwrap();
        eprintln!("{name}: {}", coverage["counts"]);
        let ids: Vec<_> = engine
            .dex_class(name)
            .unwrap()
            .methods
            .iter()
            .filter(|m| m.code.is_some())
            .map(|m| {
                format!(
                    "{}.{}({}){}",
                    name,
                    m.name,
                    m.parameters.join(""),
                    m.return_type
                )
            })
            .collect();
        let mut dex = String::new();
        for id in ids {
            dex.push_str(
                &engine
                    .method_source(&id, MethodSourceFormat::Dex, &false.into())
                    .unwrap()
                    .code
                    .source,
            );
        }
        std::fs::write(dir.join(format!("{name}.dex.txt")), dex).unwrap();
        assert_eq!(coverage["counts"]["omitted"], 0);
        if matches!(
            name,
            "ze.v1" | "ze.z3" | "ze.r3" | "androidx.fragment.app.h0" | "oj.c1"
        ) {
            assert_eq!(coverage["counts"]["dex-fallback"], 0, "{coverage}");
        }
    }
    let method = engine
        .dex_class("tj.c")
        .unwrap()
        .methods
        .iter()
        .find(|m| m.name.as_ref() == "a")
        .unwrap();
    let id = format!(
        "tj.c.a({}){}",
        method.parameters.join(""),
        method.return_type
    );
    let exact = engine
        .call_graph_query_by_id(
            &id,
            &serde_json::json!({"direction":"callers","projection":"call_sites"}),
            &false.into(),
        )
        .unwrap();
    let expanded=engine.call_graph_query_by_id(&id,&serde_json::json!({"direction":"callers","projection":"call_sites","dispatch_through_superclasses":true}),&false.into()).unwrap();
    std::fs::write(
        dir.join("superclass-callers.json"),
        serde_json::to_vec_pretty(&expanded).unwrap(),
    )
    .unwrap();
    assert_eq!(expanded["exact_matching_total"], exact["matching_total"]);
    assert!(
        expanded["call_sites"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["caller_method_id"]
                .as_str()
                .unwrap()
                .starts_with("xg.d.b(")
                && s["relationship"] == "possible_virtual_dispatch"),
        "{expanded}"
    );
    assert!(
        engine
            .read_resource_with_metadata("assets/json/android_v10.json")
            .unwrap()
            .source
            .contains("rules")
    );
}

fn method_fixture(
    name: &str,
    words: Vec<u16>,
    tries: Vec<rdx::native_dex::DexTryRegion>,
    parameters: Vec<&str>,
) -> rdx::native_dex::DexClass {
    use rdx::native_dex::*;
    let mut c = rdx::native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    c.descriptor = "Lsample/Recovery;".into();
    c.superclass = Some("Ljava/lang/Object;".into());
    c.fields.clear();
    c.annotations_offset = 0;
    c.static_values.clear();
    c.symbols = std::sync::Arc::new(DexSymbols {
        types: vec!["Lsample/Effects;".into()],
        strings: vec!["work".into(), "capture".into()],
        protos: vec![
            ("V".into(), vec!["I".into()]),
            ("V".into(), vec!["Ljava/lang/Throwable;".into()]),
        ],
        methods: vec![(0, 0, 0), (0, 1, 1)],
        ..Default::default()
    });
    c.methods = vec![DexMethod {
        declaring_type: c.descriptor.clone(),
        name: name.into(),
        parameters: parameters.iter().map(|s| (*s).into()).collect(),
        return_type: "I".into(),
        thrown_types: vec![],
        access_flags: 9,
        code: Some(DexCode {
            registers: 3,
            ins: parameters.len() as u16,
            outs: 1,
            tries: tries.len() as u16,
            offset: 0,
            try_regions: tries,
            instructions: words,
        }),
    }];
    c
}
fn backward_handler_fixture() -> rdx::native_dex::DexClass {
    use rdx::native_dex::DexTryRegion;
    method_fixture(
        "handlers",
        vec![
            0x0238, 9, 0x1071, 0, 2, 0x0b28, 0x000d, 0x0107, 0x0b28, 0x1071, 0, 2, 0x0238, 4,
            0x1012, 0x000f, 0x020f, 0x000d, 0xf528, 0x1071, 1, 1, 0xf012, 0x000f,
        ],
        vec![
            DexTryRegion {
                start: 2,
                end: 5,
                catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 6)].into(),
            },
            DexTryRegion {
                start: 9,
                end: 12,
                catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 17)].into(),
            },
        ],
        vec!["I"],
    )
}
fn constant_fixture() -> rdx::native_dex::DexClass {
    // A side-effecting invocation precedes a provably constant diamond.
    method_fixture(
        "constant",
        vec![
            0x1071, 0, 2, 0x0012, 0x0038, 6, 0x1112, 0x1071, 0, 1, 0x2112, 0x010f,
        ],
        vec![],
        vec!["I"],
    )
}
#[test]
fn backwards_shared_catch_tail_and_constant_predicate() {
    let c = backward_handler_fixture();
    let source = rdx::native_java::render_method("sample.Recovery", &c, &c.methods[0])
        .unwrap()
        .source;
    assert!(source.contains("catch ("), "{source}");
    assert!(source.contains("Effects.capture"), "{source}");
    let c = constant_fixture();
    let source = rdx::native_java::render_method("sample.Recovery", &c, &c.methods[0])
        .unwrap()
        .source;
    assert!(!source.contains("0 == 0"), "{source}");
    assert!(source.contains("Effects.work"), "{source}");
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn catch_tail_and_literal_folding_execute_original_effects() {
    let methods = [backward_handler_fixture(), constant_fixture()]
        .iter()
        .map(|c| {
            rdx::native_java::render_method("sample.Recovery", c, &c.methods[0])
                .unwrap()
                .source
        })
        .collect::<Vec<_>>()
        .join("\n");
    let dir = std::env::temp_dir().join(format!("rdx-catch-constant-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    let java = format!(
        r#"package sample; public class Effects {{
 static int calls; static boolean fail; static Throwable caught; static RuntimeException fault=new RuntimeException();
 public static void work(int n){{calls++;if(fail)throw fault;}}
 public static void capture(Throwable t){{caught=t;}}
 {methods}
 public static void main(String[] a){{
  for(int n:new int[]{{0,1,-1,5}})for(boolean f:new boolean[]{{false,true}}){{calls=0;fail=f;caught=null;int result=handlers(n);if(result!=(f?-1:(n==0?0:n))||calls!=1||(f&&caught!=fault))throw new AssertionError("handler");}}
  calls=0;fail=false;if(constant(5)!=2||calls!=1)throw new AssertionError("constant");
  calls=0;fail=true;try{{constant(5);throw new AssertionError();}}catch(RuntimeException e){{if(e!=fault||calls!=1)throw new AssertionError("identity");}}
 }} }}"#
    );
    std::fs::write(dir.join("sample/Effects.java"), &java).unwrap();
    for (program, arg) in [("javac", "sample/Effects.java"), ("java", "sample.Effects")] {
        let r = std::process::Command::new(program)
            .arg(arg)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            r.status.success(),
            "{}\n{java}",
            String::from_utf8_lossy(&r.stderr)
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}
