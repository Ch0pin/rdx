use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_java,
};
use std::sync::Arc;
fn code(words: Vec<u16>) -> DexCode {
    DexCode {
        registers: 1,
        ins: 1,
        outs: 1,
        tries: 0,
        try_regions: vec![],
        instructions: words,
        offset: 0,
    }
}
fn method(name: &str, flags: u32, words: Option<Vec<u16>>) -> DexMethod {
    DexMethod {
        declaring_type: "Lsample/Model;".into(),
        name: name.into(),
        return_type: "Ljava/lang/String;".into(),
        parameters: if flags & 8 != 0 {
            vec!["Lsample/Model;".into()]
        } else {
            vec![]
        },
        thrown_types: vec![],
        access_flags: flags,
        code: words.map(code),
    }
}
fn fixture() -> DexClass {
    DexClass {
        descriptor: "Lsample/Model;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 0x601,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            strings: vec!["toPayload".into(), "value".into(), "constant".into()],
            types: vec!["Lsample/Model;".into(), "Lsample/Other;".into()],
            protos: vec![("Ljava/lang/String;".into(), vec![])],
            methods: vec![(0, 0, 0), (0, 0, 1)],
            ..Default::default()
        }),
        methods: vec![
            method(
                "access$toPayload$jd",
                0x1009,
                Some(vec![0x106f, 0, 0, 0x000c, 0x0011]),
            ),
            method("toPayload", 1, Some(vec![0x1072, 1, 0, 0x000c, 0x0011])),
            method("value", 0x401, None),
        ],
    }
}
#[test]
fn exact_default_body_expands_with_static_receiver_and_original_target_link() {
    let class = fixture();
    let emitted = native_java::render("sample.Model", &class).unwrap();
    assert!(
        emitted
            .source
            .contains("if (model == null) { throw null; }"),
        "{}",
        emitted.source
    );
    assert!(
        emitted.source.contains("model.value()"),
        "{}",
        emitted.source
    );
    assert!(!emitted.source.contains("model.toPayload()"));
    assert!(
        emitted.source.contains("default public String toPayload()"),
        "{}",
        emitted.source
    );
    let chars: Vec<_> = emitted.source.chars().collect();
    let link = emitted
        .links
        .iter()
        .find(|link| {
            link.label == "sample.Model.toPayload()Ljava/lang/String;"
                && chars[link.start..link.end].iter().collect::<String>() == "toPayload"
                && chars[..link.start]
                    .iter()
                    .collect::<String>()
                    .ends_with("Expanded nonvirtual default: ")
        })
        .unwrap();
    assert_eq!(
        chars[link.start..link.end].iter().collect::<String>(),
        "toPayload"
    );
    assert!(
        emitted
            .definitions
            .iter()
            .any(|def| def.name == "access$toPayload$jd")
    );
    for link in emitted.links {
        assert!(link.start < link.end && link.end <= chars.len());
    }
}
#[test]
fn virtual_override_dispatch_stays_virtual_and_special_contexts_fail_closed() {
    let mut class = fixture();
    class.methods[0].code.as_mut().unwrap().instructions[0] = 0x1072;
    let emitted = native_java::render_method("sample.Model", &class, &class.methods[0]).unwrap();
    assert!(
        emitted.source.contains("p0.toPayload()"),
        "{}",
        emitted.source
    );
    assert!(!emitted.source.contains("Expanded nonvirtual"));
    for shape in 0..8 {
        let mut class = fixture();
        match shape {
            0 => class.methods[0].access_flags = 9,
            1 => class.methods[0].name = "arbitrary".into(),
            2 => Arc::get_mut(&mut class.symbols).unwrap().methods[0].0 = 1,
            3 => class.methods[0]
                .code
                .as_mut()
                .unwrap()
                .instructions
                .insert(0, 0),
            4 => class.methods[1].access_flags = 0x21,
            5 => class.methods[1].code.as_mut().unwrap().instructions[0] = 0x106f,
            6 => class.methods[1].parameters.push("I".into()),
            7 => class.methods[1].code.as_mut().unwrap().instructions = vec![0x0028, 0x0011],
            _ => unreachable!(),
        }
        assert!(
            native_java::render_method("sample.Model", &class, &class.methods[0]).is_err(),
            "accepted shape {shape}"
        );
    }
}
#[test]
fn constant_default_still_checks_null_before_string_resolution() {
    let mut class = fixture();
    class.methods[1].code.as_mut().unwrap().instructions = vec![0x001a, 2, 0x0011];
    let emitted = native_java::render_method("sample.Model", &class, &class.methods[0]).unwrap();
    assert!(
        emitted.source.find("if (p0 == null)").unwrap()
            < emitted.source.find("\"constant\"").unwrap()
    );
}
#[test]
#[ignore = "requires javac and java"]
fn jvm_special_default_dispatch_dynamic_getters_null_and_failure_identity() {
    use std::{fs, process::Command};
    let class = fixture();
    let emitted = native_java::render("sample.Model", &class).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-default-bridge-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/Model.java"), &emitted.source).unwrap();
    fs::write(dir.join("sample/Check.java"),r#"package sample;
import java.lang.invoke.*;
public class Check {
static String trace; static RuntimeException marker=new RuntimeException();
static class Receiver implements Model {
boolean fail; public String toPayload(){trace+="O";return "override";}
public String value(){trace+="G";if(fail)throw marker;return "getter";}
}
static MethodHandle original;
static String run(Receiver receiver,boolean expanded) throws Throwable {trace="";try {String result=expanded?Model.access$toPayload$jd(receiver):(String)original.invokeExact((Model)receiver);return trace+":"+result;}catch(RuntimeException e){if(receiver!=null && e!=marker)throw new AssertionError(e);if(receiver==null && !(e instanceof NullPointerException))throw new AssertionError(e);return trace+":throw";}}
public static void main(String[] args) throws Throwable {
original=MethodHandles.privateLookupIn(Model.class,MethodHandles.lookup()).findSpecial(Model.class,"toPayload",MethodType.methodType(String.class),Model.class);
Receiver receiver=new Receiver();
for(int mode=0;mode<3;mode++){receiver.fail=mode==1;Receiver r=mode==2?null:receiver;String a=run(r,false),b=run(r,true);if(!a.equals(b))throw new AssertionError(a+" != "+b);if(mode==0&&!b.equals("G:getter"))throw new AssertionError(b);}
trace="";if(!receiver.toPayload().equals("override")||!trace.equals("O"))throw new AssertionError();
System.out.print("ok");
}}
"#).unwrap();
    let compiled = Command::new("javac")
        .arg(dir.join("sample/Model.java"))
        .arg(dir.join("sample/Check.java"))
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&compiled.stderr),
        emitted.source
    );
    let run = Command::new("java")
        .arg("-cp")
        .arg(&dir)
        .arg("sample.Check")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(run.stdout, b"ok");
    fs::remove_dir_all(dir).unwrap();
}
#[test]
#[ignore = "requires RDX_NOTIFICATIONS_APK with supplied CapitalOne build"]
fn exact_apk_static_default_bridge_is_reconstructed_and_linked() {
    use rdx::engine::{DecompilerEngine, NativeEngine};
    let path = std::env::var_os("RDX_NOTIFICATIONS_APK").expect("APK required");
    let mut engine = NativeEngine::start().unwrap();
    engine.open(std::path::Path::new(&path)).unwrap();
    let code = engine
        .decompile_with_metadata("uk.co.capitalone.android.core.deeplink.data.model.IDeepLinkModel")
        .unwrap();
    assert!(!code.source.contains(".method "), "{}", code.source);
    assert!(
        code.source
            .contains("public static Map access$toPayload$jd(IDeepLinkModel iDeepLinkModel)"),
        "{}",
        code.source
    );
    assert!(code.source.contains("iDeepLinkModel.getUriString()"));
    assert!(code.source.contains("iDeepLinkModel.getData()"));
    assert!(!code.source.contains("iDeepLinkModel.toPayload()"));
    assert!(code.links.iter().any(|link|link.label=="uk.co.capitalone.android.core.deeplink.data.model.IDeepLinkModel.toPayload()Ljava/util/Map;"));
}
