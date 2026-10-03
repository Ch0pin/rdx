use rdx::{
    native_dex::{DexClass, DexCode, DexField, DexMethod, DexSymbols},
    native_java,
};
use std::sync::Arc;

fn fixture() -> DexClass {
    let words = vec![
        0x0159, 0, // own final field before delegation
        0x012b, 26, 0, // selector and payload
        0x1070, 0, 0, 0x1071, 1, 1, 0x000e, // default: delegate, effect, return
        0x1070, 0, 0, 0x1112, 0x1071, 1, 1, 0x000e, // key 0
        0x0176, 0, 0, 0x2112, 0x1071, 1, 1, 0x000e, // key 1, range call
        0x0100, 2, 0, 0, 10, 0, 18, 0,
    ];
    DexClass {
        descriptor: "Lsample/SwitchChild;".into(),
        superclass: Some("Lsample/Base;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![DexField {
            declaring_type: "Lsample/SwitchChild;".into(),
            name: "a".into(),
            field_type: "I".into(),
            access_flags: 0x11,
            is_static: false,
        }],
        symbols: Arc::new(DexSymbols {
            types: vec![
                "Lsample/Base;".into(),
                "Lsample/Source;".into(),
                "Lsample/SwitchChild;".into(),
                "I".into(),
            ],
            strings: vec!["<init>".into(), "arm".into(), "a".into()],
            protos: vec![("V".into(), vec![]), ("V".into(), vec!["I".into()])],
            methods: vec![(0, 0, 0), (1, 1, 1)],
            fields: vec![(2, 3, 2)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/SwitchChild;".into(),
            name: "<init>".into(),
            return_type: "V".into(),
            parameters: vec!["I".into()],
            thrown_types: vec![],
            access_flags: 0x10001,
            code: Some(DexCode {
                registers: 2,
                ins: 2,
                outs: 1,
                tries: 0,
                try_regions: vec![],
                instructions: words,
                offset: 0,
            }),
        }],
    }
}

#[test]
fn shared_delegation_is_emitted_once_before_switch_after_original_field_write() {
    let class = fixture();
    let body = native_java::render_method("sample.SwitchChild", &class, &class.methods[0]).unwrap();
    assert_eq!(
        body.source.matches("super();").count(),
        1,
        "{}",
        body.source
    );
    assert!(body.source.find("this.a = p0;").unwrap() < body.source.find("super();").unwrap());
    assert!(body.source.find("super();").unwrap() < body.source.find("switch (").unwrap());
    assert_eq!(body.source.matches("Source.arm(").count(), 3);
    assert!(
        body.links
            .iter()
            .any(|link| link.label == "sample.Base.<init>()V")
    );
}

#[test]
fn nonuniform_or_nonleading_delegation_cannot_be_factored() {
    for position in [5, 12, 20] {
        let mut class = fixture();
        class.methods[0].code.as_mut().unwrap().instructions[position..position + 3]
            .copy_from_slice(&[0x1071, 1, 1]);
        rdx::native_method::MethodAnalysis::build(&class, &class.methods[0]).unwrap();
        assert!(
            native_java::render_method("sample.SwitchChild", &class, &class.methods[0]).is_err()
        );
    }
    let mut parameterized = fixture();
    Arc::get_mut(&mut parameterized.symbols).unwrap().protos[0].1 = vec!["I".into()];
    for position in [5, 12] {
        parameterized.methods[0].code.as_mut().unwrap().instructions[position..position + 3]
            .copy_from_slice(&[0x2070, 0, 0x0010]);
    }
    parameterized.methods[0].code.as_mut().unwrap().instructions[20] = 0x0276;
    rdx::native_method::MethodAnalysis::build(&parameterized, &parameterized.methods[0]).unwrap();
    assert!(
        native_java::render_method(
            "sample.SwitchChild",
            &parameterized,
            &parameterized.methods[0]
        )
        .is_err()
    );
    let mut escaped = fixture();
    escaped.methods[0].code.as_mut().unwrap().instructions[14] = 1; // parameter instead of this
    assert!(
        native_java::render_method("sample.SwitchChild", &escaped, &escaped.methods[0]).is_err()
    );
    let mut different = fixture();
    let symbols = Arc::get_mut(&mut different.symbols).unwrap();
    symbols.types.push("Lsample/Other;".into());
    symbols.methods.push((3, 0, 0));
    different.methods[0].code.as_mut().unwrap().instructions[13] = 2;
    assert!(
        native_java::render_method("sample.SwitchChild", &different, &different.methods[0])
            .is_err()
    );
}

#[test]
#[ignore = "requires Java25 flexible constructor bodies via RDX_JAVA25_HOME"]
fn jvm_preserves_final_field_callback_single_delegation_arm_effects_and_exception_identity() {
    use std::{fs, process::Command};
    let home = std::env::var("RDX_JAVA25_HOME").expect("RDX_JAVA25_HOME");
    let class = fixture();
    let rendered =
        native_java::render_method("sample.SwitchChild", &class, &class.methods[0]).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-constructor-switch-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let source = r#"
package sample;
class Source {
    static String trace; static boolean failBase,failArm;
    static final RuntimeException marker = new RuntimeException();
    static void arm(int value) { trace += "A"+value; if(failArm) throw marker; }
}
class Base {
    Base() { Source.trace += "B"+((SwitchChild)this).a; if(Source.failBase) throw Source.marker; }
}
public class SwitchChild extends Base {
    public final int a;
METHOD
    public static void main(String[] args) {
        for(int input : new int[]{Integer.MIN_VALUE,-1,0,1,2,17,Integer.MAX_VALUE}) {
            for(int failure=0;failure<3;failure++) {
                Source.trace=""; Source.failBase=failure==1; Source.failArm=failure==2;
                int arm=input==0?1:input==1?2:input;
                try { SwitchChild value=new SwitchChild(input); if(failure!=0||value.a!=input)throw new AssertionError(); }
                catch(RuntimeException actual) { if(failure==0||actual!=Source.marker)throw new AssertionError(); }
                String expected="B"+input+(failure==1?"":"A"+arm);
                if(!Source.trace.equals(expected))throw new AssertionError(Source.trace+" != "+expected);
            }
        }
    }
}
"#;
    fs::write(
        dir.join("sample/SwitchChild.java"),
        source.replace("METHOD", &rendered.source),
    )
    .unwrap();
    for (program, arguments) in [
        ("javac", vec!["sample/SwitchChild.java"]),
        ("java", vec!["-cp", ".", "sample.SwitchChild"]),
    ] {
        let result = Command::new(format!("{home}/bin/{program}"))
            .args(arguments)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
