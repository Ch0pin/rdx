use rdx::{
    native_dex::{DexClass, DexCode, DexField, DexMethod, DexSymbols},
    native_java,
};
use std::sync::Arc;

fn fixture() -> DexClass {
    let descriptor: Arc<str> = "Lsample/StaticSlots;".into();
    DexClass {
        descriptor: descriptor.clone(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: ["v0", "v4"]
            .into_iter()
            .map(|name| DexField {
                declaring_type: descriptor.clone(),
                name: name.into(),
                field_type: "I".into(),
                access_flags: 0x19,
                is_static: true,
            })
            .collect(),
        methods: vec![DexMethod {
            declaring_type: descriptor,
            name: "<clinit>".into(),
            return_type: "V".into(),
            parameters: vec![],
            thrown_types: vec![],
            access_flags: 0x10008,
            code: Some(DexCode {
                registers: 1,
                ins: 0,
                outs: 0,
                tries: 0,
                try_regions: vec![],
                instructions: vec![
                    0x0071, 0, 0, 0x000a, 0x0067, 0, 0x0071, 0, 0, 0x000a, 0x0067, 1, 0x000e,
                ],
                offset: 0,
            }),
        }],
        symbols: Arc::new(DexSymbols {
            types: vec![
                "I".into(),
                "Lsample/Source;".into(),
                "Lsample/StaticSlots;".into(),
            ],
            strings: vec!["next".into(), "v0".into(), "v4".into()],
            protos: vec![("I".into(), vec![])],
            methods: vec![(1, 0, 0)],
            fields: vec![(2, 0, 1), (2, 0, 2)],
            ..Default::default()
        }),
    }
}

#[test]
fn blank_final_static_fields_do_not_bind_generated_locals() {
    let class = fixture();
    let body = native_java::render_method("sample.StaticSlots", &class, &class.methods[0]).unwrap();
    assert!(body.source.contains("int v5 ="), "{}", body.source);
    assert!(body.source.contains("v0 = v5;"), "{}", body.source);
    assert!(body.source.contains("v4 = v6;"), "{}", body.source);
    assert!(!body.source.contains("int v0 ="));
    assert!(!body.source.contains("int v4 ="));
    assert_eq!(body.source.matches("Source.next()").count(), 2);
    assert_eq!(
        body.links
            .iter()
            .filter(|link| link.label == "sample.Source.next()I")
            .count(),
        2
    );
}

#[test]
fn overflowing_field_suffix_keeps_conservative_fallback() {
    let mut class = fixture();
    class.fields[0].name = format!("v{}", usize::MAX).into();
    Arc::get_mut(&mut class.symbols).unwrap().strings[1] = class.fields[0].name.to_string();
    assert!(native_java::render_method("sample.StaticSlots", &class, &class.methods[0]).is_err());
}

#[test]
fn protected_initializer_keeps_handler_namespace_guard() {
    let mut class = fixture();
    let code = class.methods[0].code.as_mut().unwrap();
    code.tries = 1;
    code.instructions.extend([0x000d, 0x0027]);
    code.try_regions = vec![rdx::native_dex::DexTryRegion {
        start: 0,
        end: 6,
        catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 13)].into(),
    }];
    rdx::native_method::MethodAnalysis::build(&class, &class.methods[0]).unwrap();
    let error =
        native_java::render_method("sample.StaticSlots", &class, &class.methods[0]).unwrap_err();
    assert!(
        format!("{error:#}").contains("initializer field collides with generated local namespace")
    );
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn jvm_assigns_both_final_fields_once_and_preserves_first_or_second_throw() {
    use std::{fs, process::Command};
    let class = fixture();
    let body = native_java::render_method("sample.StaticSlots", &class, &class.methods[0]).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-static-local-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let source = r#"
package sample;
class Source {
    static String trace="C"; static int count, failAt;
    static final RuntimeException marker=new RuntimeException();
    static int next() { trace+="N"+(++count); if(count==failAt)throw marker; return count; }
}
class StaticSlots {
    static final int v0, v4;
METHOD
}
public class Harness {
    public static void main(String[] args) {
        Source.failAt=Integer.parseInt(args[0]);
        try {
            if(StaticSlots.v4!=2||StaticSlots.v0!=1||Source.failAt!=0)throw new AssertionError();
        } catch(ExceptionInInitializerError actual) {
            if(Source.failAt==0||actual.getCause()!=Source.marker)throw new AssertionError();
        }
        int expected=Source.failAt==1?1:2;
        if(Source.count!=expected||!Source.trace.equals(expected==1?"CN1":"CN1N2"))throw new AssertionError(Source.trace);
    }
}
"#;
    fs::write(
        dir.join("sample/Harness.java"),
        source.replace("METHOD", &body.source),
    )
    .unwrap();
    let compile = Command::new("javac")
        .arg("sample/Harness.java")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    for mode in ["0", "1", "2"] {
        let run = Command::new("java")
            .args(["-cp", ".", "sample.Harness", mode])
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "{}",
            String::from_utf8_lossy(&run.stderr)
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
