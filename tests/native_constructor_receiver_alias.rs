use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_java,
    native_method::MethodAnalysis,
};
use std::{fs, process::Command, sync::Arc};
fn fixture() -> DexClass {
    DexClass {
        descriptor: "Lsample/Alias;".into(),
        superclass: Some("Lsample/Base;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            types: vec![
                "Lsample/Alias;".into(),
                "Lsample/Base;".into(),
                "Ljava/lang/String;".into(),
                "Lsample/Hooks;".into(),
            ],
            strings: vec!["<init>".into(), "consume".into(), "MAIN".into()],
            protos: vec![
                ("V".into(), vec!["Ljava/lang/String;".into()]),
                ("V".into(), vec!["Ljava/lang/Object;".into()]),
            ],
            methods: vec![(1, 0, 0), (3, 1, 1)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/Alias;".into(),
            name: "<init>".into(),
            return_type: "V".into(),
            parameters: vec!["Ljava/lang/String;".into(), "I".into()],
            thrown_types: vec![],
            access_flags: 1,
            code: Some(DexCode {
                registers: 4,
                ins: 3,
                outs: 2,
                tries: 0,
                try_regions: vec![],
                offset: 0,
                // Both branch arms copy the exact incoming receiver into scratch r0.
                // Copies carry identity only, and must emit no illegal Java this read.
                instructions: vec![
                    0x0338, 7, 0x001a, 2, 0x0207, 0x1007, 0x0228, 0x1007, 0x2070, 0, 0x0020, 0x000e,
                ],
            }),
        }],
    }
}
fn source(c: &DexClass) -> String {
    MethodAnalysis::build(c, &c.methods[0])
        .unwrap()
        .infer_types()
        .unwrap();
    native_java::render_method("sample.Alias", c, &c.methods[0])
        .unwrap()
        .source
}
fn reject(c: &DexClass) {
    MethodAnalysis::build(c, &c.methods[0])
        .unwrap()
        .infer_types()
        .unwrap();
    assert!(native_java::render_method("sample.Alias", c, &c.methods[0]).is_err());
}
#[test]
fn identical_receiver_copies_preserve_identity_without_materializing_this_in_prologue() {
    let c = fixture();
    let s = source(&c);
    assert_eq!(s.matches("super(").count(), 1, "{s}");
    assert!(!s.contains("= this"), "{s}");
    assert!(!s.contains("masked"), "{s}");
    let body = native_java::render_method("sample.Alias", &c, &c.methods[0]).unwrap();
    let link = body
        .links
        .iter()
        .find(|l| l.label == "sample.Base.<init>(Ljava/lang/String;)V")
        .unwrap();
    assert_eq!(
        body.source
            .chars()
            .skip(link.start)
            .take(link.end - link.start)
            .collect::<String>(),
        "super"
    );
}
#[test]
fn alias_permissions_do_not_extend_to_invocation_condition_integer_wide_or_missing_predecessor() {
    for replacement in [0x1001, 0x1004, 0x0000] {
        let mut c = fixture();
        c.methods[0].code.as_mut().unwrap().instructions[7] = replacement;
        reject(&c);
    }
    // The alias is read by a static call in one arm before the merge.
    let mut escape = fixture();
    escape.methods[0].code.as_mut().unwrap().instructions = vec![
        0x0338, 10, 0x001a, 2, 0x0207, 0x1007, 0x1071, 1, 0, 0x0228, 0x1007, 0x2070, 0, 0x0020,
        0x000e,
    ];
    reject(&escape);
    // The alias is used as a condition while it still belongs to the arm.
    let mut condition = fixture();
    condition.methods[0].code.as_mut().unwrap().instructions = vec![
        0x0338, 9, 0x001a, 2, 0x0207, 0x1007, 0x0038, 5, 0x0328, 0x1007, 0, 0x2070, 0, 0x0020,
        0x000e,
    ];
    reject(&condition);
}
#[test]
fn allocation_decoder_cannot_consume_alias_but_preserves_an_unread_identity() {
    let mut base = fixture();
    let symbols = Arc::get_mut(&mut base.symbols).unwrap();
    symbols.types.push("Lsample/Holder;".into());
    symbols.methods.push((4, 1, 0)); // Holder(Object)
    let words = vec![
        0x0338, 12, 0x001a, 2, 0x0207, 0x1007, 0x0322, 4, 0x2070, 2, 0x0003, 0x0228, 0x1007,
        0x2070, 0, 0x0020, 0x000e,
    ];
    base.methods[0].code.as_mut().unwrap().instructions = words;
    reject(&base); // new Holder(alias) must retain ordinary uninitialized guard.
    base.methods[0].code.as_mut().unwrap().instructions[10] = 0x0023;
    let text = source(&base); // new Holder(p0) leaves unread identity intact.
    assert!(text.contains("new sample.Holder"), "{text}");
    assert!(!text.contains("masked"), "{text}");
}

#[test]
fn nested_join_cannot_expose_receiver_alias_inside_an_outer_arm() {
    let mut class = fixture();
    class.methods[0].code.as_mut().unwrap().instructions = vec![
        0x0338, 14, 0x1007, 0x0338, 5, 0x021a, 2, 0x0328, 0x021a, 2, 0x0038, 3, 0x0328, 0, 0x1007,
        0x2070, 0, 0x0020, 0x000e,
    ];
    reject(&class); // if(alias==null) after the inner join is still prohibited.
}

#[test]
#[ignore = "requires javac/java25 on PATH"]
fn jvm_keeps_default_argument_choice_and_superclass_throw_timing() {
    let directory =
        std::env::temp_dir().join(format!("rdx-constructor-alias-{}", std::process::id()));
    fs::create_dir_all(directory.join("sample")).unwrap();
    let java = format!(
        r#"package sample;
class Base {{ static String observed; static boolean fail; Base(String value) {{ observed=value; if(fail) throw new IllegalStateException(); }} }}
public class Alias extends Base {{ {}
 public static void main(String[] args) {{ for(String input:new String[]{{null,"","given","MAIN"}}) for(int mask:new int[]{{0,1,-1,Integer.MAX_VALUE}}) for(boolean fail:new boolean[]{{false,true}}) {{ Base.observed="unset"; Base.fail=fail; boolean thrown=false; try {{new Alias(input,mask);}} catch(IllegalStateException error) {{thrown=true;}} String expected=mask==0?input:"MAIN"; if(!java.util.Objects.equals(expected,Base.observed)||thrown!=fail) throw new AssertionError(mask+":"+Base.observed); }} }} }}"#,
        source(&fixture())
    );
    fs::write(directory.join("sample/Alias.java"), java).unwrap();
    for (program, arg) in [("javac", "sample/Alias.java"), ("java", "sample.Alias")] {
        let r = Command::new(program)
            .arg(arg)
            .current_dir(&directory)
            .output()
            .unwrap();
        assert!(
            r.status.success(),
            "{program}: {}",
            String::from_utf8_lossy(&r.stderr)
        );
    }
}
