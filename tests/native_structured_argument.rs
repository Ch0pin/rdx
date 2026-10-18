use rdx::{
    native_calls, native_cfg, native_constructors,
    native_dex::{self, DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_hierarchy, native_ir, native_java, native_method,
};
use std::{fs, process::Command, sync::Arc};
#[path = "../src/native_java/structured_argument_region.rs"]
mod proof;
fn fixture() -> DexClass {
    DexClass {
        symbols: Arc::new(DexSymbols {
            strings: vec!["value".into(), "<init>".into()],
            types: vec!["Lsample/A;".into(), "Lsample/Source;".into()],
            protos: vec![("I".into(), vec![]), ("V".into(), vec!["I".into()])],
            methods: vec![(1, 0, 0), (0, 1, 1)],
            ..Default::default()
        }),
        descriptor: "Lsample/Test;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![DexMethod {
            declaring_type: "Lsample/Test;".into(),
            name: "make".into(),
            return_type: "Lsample/A;".into(),
            parameters: vec!["Z".into()],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 3,
                ins: 1,
                outs: 2,
                tries: 0,
                try_regions: vec![],
                instructions: vec![
                    0x0022, 0, 0x0238, 7, 0x0071, 0, 0, 0x010a, 0x0228, 0x2112, 0x2070, 1, 0x0010,
                    0x0011,
                ],
                offset: 0,
            }),
        }],
    }
}
fn string_fixture(jumbo: bool) -> DexClass {
    let mut class = fixture();
    let symbols = Arc::get_mut(&mut class.symbols).unwrap();
    symbols.strings.extend(["é🙂".into(), "alternative".into()]);
    symbols.protos[1].1 = vec!["Ljava/lang/String;".into()];
    let code = class.methods[0].code.as_mut().unwrap();
    code.instructions = if jumbo {
        vec![
            0x0022, 0, 0x0238, 6, 0x011b, 2, 0, 0x0428, 0x011b, 3, 0, 0x2070, 1, 0x0010, 0x0011,
        ]
    } else {
        vec![
            0x0022, 0, 0x0238, 5, 0x011a, 2, 0x0328, 0x011a, 3, 0x2070, 1, 0x0010, 0x0011,
        ]
    };
    class
}
fn string_effect_fixture() -> DexClass {
    let mut class = string_fixture(false);
    let symbols = Arc::get_mut(&mut class.symbols).unwrap();
    symbols.strings.push("effect".into());
    symbols.protos.push(("V".into(), vec![]));
    symbols.methods.push((1, 2, 4));
    class.methods[0].code.as_mut().unwrap().instructions = vec![
        0x0022, 0, 0x0238, 8, 0x011a, 2, 0x0071, 2, 0, 0x0328, 0x011a, 3, 0x2070, 1, 0x0010, 0x0011,
    ];
    class
}
#[test]
fn string_resolution_is_materialized_before_intervening_throwing_effect() {
    let class = string_effect_fixture();
    let rendered = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert!(rendered.source.find("new sample.A").unwrap() < rendered.source.find("é🙂").unwrap());
    assert!(
        rendered.source.find("é🙂").unwrap()
            < rendered.source.find("sample.Source.effect()").unwrap(),
        "{}",
        rendered.source
    );
    assert_eq!(rendered.source.matches("é🙂").count(), 1);
}
fn nested_fixture() -> DexClass {
    let mut class = fixture();
    let symbols = Arc::get_mut(&mut class.symbols).unwrap();
    symbols.types.push("Lsample/B;".into());
    symbols.protos[1].1.push("Lsample/B;".into());
    symbols.protos.push(("V".into(), vec![]));
    symbols.methods.push((2, 2, 1));
    let code = class.methods[0].code.as_mut().unwrap();
    code.registers = 4;
    code.outs = 3;
    code.instructions = vec![
        0x0022, 0, 0x0338, 7, 0x0071, 0, 0, 0x020a, 0x0228, 0x2212, 0x0122, 2, 0x1070, 2, 1,
        0x3070, 1, 0x0120, 0x0011,
    ];
    class
}
fn nested_capture_fixture() -> DexClass {
    let mut class = fixture();
    let symbols = Arc::get_mut(&mut class.symbols).unwrap();
    symbols.types.push("Lsample/B;".into());
    symbols.methods.push((2, 1, 1));
    class.methods[0].parameters.clear();
    let code = class.methods[0].code.as_mut().unwrap();
    code.ins = 0;
    code.instructions = vec![
        0x0022, 0, 0x0122, 2, 0x0071, 0, 0, 0x020a, 0x02d8, 0x0102, 0x2070, 2, 0x0021, 0x2070, 1,
        0x0020, 0x0011,
    ];
    class
}
#[test]
fn nested_expression_capture_declarations_keep_typed_hoisting_metadata() {
    let class = nested_capture_fixture();
    let rendered = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert!(
        rendered.source.contains("new sample.A(switch (0)"),
        "{}",
        rendered.source
    );
    assert!(rendered.source.contains("new sample.B"));
    assert_eq!(rendered.source.matches("sample.Source.value()").count(), 1);
    assert!(
        rendered.source.find("new sample.B").unwrap()
            < rendered.source.find("sample.Source.value()").unwrap()
    );
    assert!(
        rendered
            .links
            .iter()
            .any(|link| link.label == "sample.B.<init>(I)V")
    );
}
fn switch_fixture() -> DexClass {
    let mut class = fixture();
    class.methods[0].parameters[0] = "I".into();
    class.methods[0].code.as_mut().unwrap().instructions = vec![
        0x0022, 0, 0x022b, 14, 0, 0x3112, 0x0528, 0x0071, 0, 0, 0x010a, 0x2070, 1, 0x0010, 0x0011,
        0, 0x0100, 1, 0, 0, 5, 0,
    ];
    class
}
fn loop_fixture() -> DexClass {
    let mut class = fixture();
    class.methods[0].parameters[0] = "I".into();
    let code = class.methods[0].code.as_mut().unwrap();
    code.registers = 6;
    code.instructions = vec![
        0x0022, 0, 0x0112, 0x0212, 0x0312, 0x5235, 11, 0x0071, 0, 0, 0x030a, 0x0190, 0x0301,
        0x02d8, 0x0102, 0xf628, 0x2070, 1, 0x0010, 0x0011,
    ];
    class
}
#[test]
fn loop_argument_first_iteration_inputs_are_proven_by_all_predecessors() {
    let class = loop_fixture();
    let plan = proof::prove(&class, &class.methods[0], 0, 16).unwrap();
    assert_eq!(plan.definitely_written, vec![1, 2, 3]);
    assert!(plan.first_reads.iter().any(|read| read.register == 5));
    let rendered = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert!(
        rendered.source.contains("new sample.A(switch (0)"),
        "{}",
        rendered.source
    );
    assert!(rendered.source.contains("while ("), "{}", rendered.source);
    assert!(
        rendered.source.find("new sample.A").unwrap()
            < rendered.source.find("sample.Source.value()").unwrap()
    );
    let mut undefined = loop_fixture();
    undefined.methods[0].code.as_mut().unwrap().instructions[2] = 0;
    let incomplete = proof::prove(&undefined, &undefined.methods[0], 0, 16).unwrap();
    assert!(incomplete.first_reads.iter().any(|read| read.register == 1));
    assert!(native_java::render_method("sample.Test", &undefined, &undefined.methods[0]).is_err());
    let mut backedge_only = loop_fixture();
    let words = &mut backedge_only.methods[0].code.as_mut().unwrap().instructions;
    words[4] = 0;
    words[18] = 0x0030;
    let incomplete = proof::prove(&backedge_only, &backedge_only.methods[0], 0, 16).unwrap();
    assert!(
        incomplete
            .first_reads
            .iter()
            .any(|read| read.pc == 16 && read.register == 3)
    );
    assert!(
        native_java::render_method("sample.Test", &backedge_only, &backedge_only.methods[0])
            .is_err()
    );
    let mut alias = loop_fixture();
    alias.methods[0].code.as_mut().unwrap().instructions[4] = 0x0307;
    assert!(proof::prove(&alias, &alias.methods[0], 0, 16).is_none());
    assert!(native_java::render_method("sample.Test", &alias, &alias.methods[0]).is_err());
}
#[test]
fn forward_switch_argument_region_freezes_original_payload_and_case_edges() {
    let class = switch_fixture();
    let plan = proof::prove(&class, &class.methods[0], 0, 11).unwrap();
    let rendered = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert!(
        rendered.source.contains("new sample.A(switch (0)"),
        "{}",
        rendered.source
    );
    assert!(rendered.source.contains("case 0"), "{}", rendered.source);
    assert!(
        rendered.source.find("new sample.A").unwrap()
            < rendered.source.find("sample.Source.value()").unwrap()
    );
    let mut changed = switch_fixture();
    changed.methods[0].code.as_mut().unwrap().instructions[18] = 1;
    assert!(!proof::validate(&changed, &changed.methods[0], &plan));
    let mut escaping = switch_fixture();
    escaping.methods[0].code.as_mut().unwrap().instructions[20] = 12;
    assert!(proof::prove(&escaping, &escaping.methods[0], 0, 11).is_none());
}
#[test]
fn nested_initialization_after_branch_retains_exact_identity_and_links() {
    let class = nested_fixture();
    let plan = proof::prove(&class, &class.methods[0], 0, 15).unwrap();
    assert_eq!(plan.nested_initializations, vec![(10, 12)]);
    let rendered = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert!(
        rendered.source.contains("new sample.A(switch (0)"),
        "{}",
        rendered.source
    );
    assert_eq!(rendered.source.matches("new sample.B").count(), 1);
    assert!(
        rendered.source.find("new sample.A").unwrap()
            < rendered.source.find("new sample.B").unwrap()
    );
    assert!(
        rendered
            .links
            .iter()
            .any(|link| link.label == "sample.B.<init>()V")
    );
    let mut poisoned = plan.clone();
    poisoned.nested_initializations.clear();
    assert!(!proof::validate(&class, &class.methods[0], &poisoned));
    let mut leaking = nested_fixture();
    leaking.methods[0].code.as_mut().unwrap().instructions[13] = 0;
    assert!(proof::prove(&leaking, &leaking.methods[0], 0, 15).is_none());
}
#[test]
fn duplicate_nested_initialization_and_preinitialization_escape_decline() {
    let mut duplicate = nested_fixture();
    duplicate.methods[0]
        .code
        .as_mut()
        .unwrap()
        .instructions
        .splice(15..15, [0x1070, 2, 1]);
    assert!(proof::prove(&duplicate, &duplicate.methods[0], 0, 18).is_none());
    assert!(native_java::render_method("sample.Test", &duplicate, &duplicate.methods[0]).is_err());
    let mut escape = nested_fixture();
    let symbols = Arc::get_mut(&mut escape.symbols).unwrap();
    symbols.strings.push("consume".into());
    symbols.protos.push(("V".into(), vec!["Lsample/B;".into()]));
    symbols.methods.push((1, 3, 2));
    escape.methods[0]
        .code
        .as_mut()
        .unwrap()
        .instructions
        .splice(12..12, [0x1071, 3, 1]);
    assert!(native_java::render_method("sample.Test", &escape, &escape.methods[0]).is_err());
}
#[test]
#[ignore = "requires explicit JDK25"]
fn nested_argument_initializers_and_constructor_faults_follow_original_order() {
    let mut nested = nested_fixture();
    nested.methods[0].name = "makeNested".into();
    let nested = native_java::render_method("sample.Test", &nested, &nested.methods[0]).unwrap();
    let mut string = string_fixture(true);
    string.methods[0].name = "makeString".into();
    let string = native_java::render_method("sample.Test", &string, &string.methods[0]).unwrap();
    let mut switched = switch_fixture();
    switched.methods[0].name = "makeSwitch".into();
    let switched =
        native_java::render_method("sample.Test", &switched, &switched.methods[0]).unwrap();
    let mut looped = loop_fixture();
    looped.methods[0].name = "makeLoop".into();
    let looped = native_java::render_method("sample.Test", &looped, &looped.methods[0]).unwrap();
    let mut string_effect = string_effect_fixture();
    string_effect.methods[0].name = "makeStringEffect".into();
    let string_effect =
        native_java::render_method("sample.Test", &string_effect, &string_effect.methods[0])
            .unwrap();
    let mut captured = nested_capture_fixture();
    captured.methods[0].name = "makeNestedCapture".into();
    let captured =
        native_java::render_method("sample.Test", &captured, &captured.methods[0]).unwrap();
    let mut earlier = earlier_string_fixture(true, true);
    earlier.methods[0].name = "makeEarlierString".into();
    let earlier = native_java::render_method("sample.Test", &earlier, &earlier.methods[0]).unwrap();
    let mut phi = earlier_phi_fixture();
    phi.methods[0].name = "makeEarlierPhi".into();
    let phi = native_java::render_method("sample.Test", &phi, &phi.methods[0]).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-nested-argument-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/Test.java"), format!(r#"package sample;
public class Test {{ {} {} {} {} {} {} {} {} public static String run(boolean branch,int mask,int kind) {{State.mask=mask;try {{A result=kind==6?makeEarlierString(branch):kind==7?makeEarlierPhi(branch):kind==1?makeString(branch):kind==2?makeSwitch(branch?0:1):kind==3?makeLoop(branch?2:0):kind==4?makeStringEffect(branch):kind==5?makeNestedCapture():makeNested(branch);return State.trace+":"+result.value;}}catch(Throwable fault){{if(fault==State.fault)return State.trace+":fault";throw new AssertionError(fault);}}}} }}
class State {{static int mask;static String trace="";static final AssertionError fault=new AssertionError();static void event(String name,int bit){{trace+=name;if((mask&bit)!=0)throw fault;}}}}
class A {{static {{State.event("A",1);}}final String value;A(int value,B child){{State.event("C",16);this.value=""+value;}}A(int value){{State.event("C",16);this.value=""+value;}}A(String value){{State.event("C",16);this.value=value;}}}}
class B {{static {{State.event("B",4);}}B(){{State.event("I",8);}}B(int value){{State.event("I",8);}}}}
class Source {{static void effect(){{State.event("E",2);}}static int value(){{State.event("S",2);return 42;}}}}
"#,nested.source,string.source,switched.source,looped.source,string_effect.source,captured.source,earlier.source,phi.source)).unwrap();
    let mut cases = String::new();
    // Expected events come from original new-instance/call/constructor order.
    for kind in 0..8 {
        for branch in [false, true] {
            for mask in 0..32 {
                let sequence = if kind == 6 {
                    vec![("E", 2), ("A", 1), ("E", 2), ("C", 16)]
                } else if kind == 7 {
                    vec![("A", 1), ("E", 2), ("C", 16)]
                } else if kind == 5 {
                    vec![("A", 1), ("B", 4), ("S", 2), ("I", 8), ("C", 16)]
                } else if kind == 4 && branch {
                    vec![("A", 1), ("E", 2), ("C", 16)]
                } else if kind == 3 && branch {
                    vec![("A", 1), ("S", 2), ("S", 2), ("C", 16)]
                } else if kind == 1 || (kind >= 2 && !branch) {
                    vec![("A", 1), ("C", 16)]
                } else if kind == 2 {
                    vec![("A", 1), ("S", 2), ("C", 16)]
                } else if branch {
                    vec![("A", 1), ("S", 2), ("B", 4), ("I", 8), ("C", 16)]
                } else {
                    vec![("A", 1), ("B", 4), ("I", 8), ("C", 16)]
                };
                let mut expected = String::new();
                let mut fault = false;
                for (name, bit) in sequence {
                    expected.push_str(name);
                    if mask & bit != 0 {
                        fault = true;
                        break;
                    }
                }
                expected.push(':');
                expected.push_str(if fault {
                    "fault"
                } else if kind == 6 {
                    "é🙂"
                } else if kind == 5 {
                    "43"
                } else if kind == 1 || kind == 4 || kind == 7 {
                    if branch { "é🙂" } else { "alternative" }
                } else if kind == 3 && branch {
                    "84"
                } else if kind == 3 {
                    "0"
                } else if branch {
                    "42"
                } else if kind == 2 {
                    "3"
                } else {
                    "2"
                });
                cases.push_str(&format!("check({branch},{mask},{kind},\"{expected}\");"));
            }
        }
    }
    fs::write(dir.join("Runner.java"), format!(r#"import java.net.*;import java.nio.file.*;public class Runner{{static void check(boolean branch,int mask,int kind,String expected)throws Exception{{try(URLClassLoader loader=new URLClassLoader(new URL[]{{Paths.get("{}").toUri().toURL()}},null)){{Object actual=loader.loadClass("sample.Test").getMethod("run",boolean.class,int.class,int.class).invoke(null,branch,mask,kind);if(!expected.equals(actual))throw new AssertionError(expected+" != "+actual);}}}}public static void main(String[]args)throws Exception{{{}System.out.print("ok:512");}}}}"#,dir.display(),cases)).unwrap();
    let home = std::env::var("RDX_JAVA25_HOME").expect("explicit JDK25 home");
    let compiled = Command::new(format!("{home}/bin/javac"))
        .arg(dir.join("sample/Test.java"))
        .arg(dir.join("Runner.java"))
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let disassembled = Command::new(format!("{home}/bin/javap"))
        .args(["-c", "-cp", dir.to_str().unwrap(), "sample.Test"])
        .output()
        .unwrap();
    assert!(disassembled.status.success());
    let disassembled = String::from_utf8(disassembled.stdout).unwrap();
    let start = disassembled.find("makeStringEffect(boolean)").unwrap();
    let body = &disassembled[start..];
    assert!(
        body.find("String é🙂").unwrap() < body.find("sample/Source.effect").unwrap(),
        "{body}"
    );
    let earlier_body = &disassembled[disassembled.find("makeEarlierString(boolean)").unwrap()
        ..disassembled.find("makeEarlierPhi(boolean)").unwrap()];
    assert!(
        earlier_body.find("String é🙂").unwrap()
            < earlier_body.find("sample/Source.effect").unwrap(),
        "{earlier_body}"
    );
    assert!(
        earlier_body.find("sample/Source.effect").unwrap()
            < earlier_body.find("class sample/A").unwrap(),
        "{earlier_body}"
    );
    let executed = Command::new(format!("{home}/bin/java"))
        .args(["-cp", dir.to_str().unwrap(), "Runner"])
        .output()
        .unwrap();
    assert!(
        executed.status.success(),
        "{}",
        String::from_utf8_lossy(&executed.stderr)
    );
    assert_eq!(executed.stdout, b"ok:512");
    fs::remove_dir_all(dir).unwrap();
}
#[test]
fn branch_string_resolution_retains_allocation_position_and_pool_identity() {
    for jumbo in [false, true] {
        let class = string_fixture(jumbo);
        let constructor = if jumbo { 11 } else { 9 };
        let plan = proof::prove(&class, &class.methods[0], 0, constructor).unwrap();
        assert!(
            plan.semantic_references
                .iter()
                .any(|(_, value)| value == "é🙂")
        );
        let rendered =
            native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
        assert!(
            rendered.source.find("new sample.A").unwrap() < rendered.source.find("é🙂").unwrap(),
            "{}",
            rendered.source
        );
        assert_eq!(rendered.source.matches("é🙂").count(), 1);
        let mut changed = string_fixture(jumbo);
        Arc::get_mut(&mut changed.symbols).unwrap().strings[2] = "changed".into();
        assert!(!proof::validate(&changed, &changed.methods[0], &plan));
    }
}
fn wide_fixture() -> DexClass {
    let mut c = fixture();
    let symbols = Arc::get_mut(&mut c.symbols).unwrap();
    symbols.protos[0] = ("J".into(), vec!["J".into(), "Ljava/lang/Object;".into()]);
    symbols.protos[1].1 = vec!["J".into(), "Ljava/lang/Object;".into()];
    let m = &mut c.methods[0];
    m.name = "makeWide".into();
    m.return_type = "J".into();
    m.parameters = vec!["Z".into(), "J".into(), "Ljava/lang/Object;".into()];
    let code = m.code.as_mut().unwrap();
    code.registers = 8;
    code.ins = 4;
    code.outs = 4;
    code.instructions = vec![
        0x0022, 0, 0x0438, 7, 0x3071, 0, 0x0765, 0x010b, 0x0328, 0x0116, 2, 0x4070, 1, 0x7210,
        0x0110,
    ];
    c
}
#[test]
fn constructor_liveouts_keep_wide_pairs_and_reference_inputs() {
    let c = wide_fixture();
    let plan = proof::prove(&c, &c.methods[0], 0, 11).unwrap();
    assert!(plan.definitely_written.contains(&1) && plan.definitely_written.contains(&2));
    assert!(
        plan.first_reads
            .iter()
            .any(|r| r.register == 5 && r.words == 2)
    );
    let code = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
    assert!(code.source.contains("switch (0)"), "{}", code.source);
    assert!(code.source.contains("long v"), "{}", code.source);
    let mut c = wide_fixture();
    c.methods[0].code.as_mut().unwrap().instructions[9] = 0x1113;
    assert!(proof::prove(&c, &c.methods[0], 0, 11).is_none());
    assert!(native_java::render_method("sample.Test", &c, &c.methods[0]).is_err());
}
fn protected_fixture() -> DexClass {
    let mut c = fixture();
    let sy = Arc::get_mut(&mut c.symbols).unwrap();
    sy.strings.push("handled".into());
    sy.protos
        .push(("V".into(), vec!["Ljava/lang/RuntimeException;".into()]));
    sy.methods.push((1, 2, 2));
    let m = &mut c.methods[0];
    m.name = "makeProtected".into();
    let code = m.code.as_mut().unwrap();
    code.instructions
        .extend([0x010d, 0x1071, 2, 1, 0x0012, 0x0011]);
    code.tries = 1;
    code.try_regions = vec![DexTryRegion {
        start: 0,
        end: 13,
        catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 14)].into(),
    }];
    c
}
fn store_fixture() -> DexClass {
    let mut c = fixture();
    let sy = Arc::get_mut(&mut c.symbols).unwrap();
    sy.types[1] = "Lsample/Store;".into();
    sy.types.push("I".into());
    sy.strings = vec!["number".into(), "<init>".into()];
    sy.fields = vec![(1, 2, 0)];
    sy.methods = vec![(0, 1, 1)];
    let m = &mut c.methods[0];
    m.name = "makeStore".into();
    m.parameters.clear();
    let code = m.code.as_mut().unwrap();
    code.ins = 0;
    code.instructions = vec![
        0x0022, 0, 0x7112, 0x0167, 0, 0x0160, 0, 0x2070, 0, 0x0010, 0x0011,
    ];
    c
}
#[test]
fn original_field_store_and_read_retain_metadata_and_allocation_position() {
    let c = store_fixture();
    let rendered = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
    assert!(
        rendered.source.find("new sample.A").unwrap()
            < rendered.source.find("sample.Store.number = 7").unwrap()
    );
    let links: Vec<_> = rendered
        .links
        .iter()
        .filter(|l| l.label == "sample.Store.number:I")
        .collect();
    assert_eq!(links.len(), 2);
    for link in links {
        assert_eq!(
            rendered
                .source
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect::<String>(),
            "number"
        );
    }
}
fn cast_fixture() -> DexClass {
    let mut c = fixture();
    let sy = Arc::get_mut(&mut c.symbols).unwrap();
    sy.types.push("Ljava/lang/String;".into());
    sy.protos[1].1 = vec!["Ljava/lang/String;".into()];
    let m = &mut c.methods[0];
    m.name = "makeCast".into();
    m.parameters = vec!["Z".into(), "Ljava/lang/Object;".into()];
    let code = m.code.as_mut().unwrap();
    code.registers = 4;
    code.ins = 2;
    code.instructions = vec![
        0x0022, 0, 0x0238, 6, 0x031f, 2, 0x3107, 0x0228, 0x0112, 0x2070, 1, 0x0010, 0x0011,
    ];
    c
}
#[test]
fn original_cast_and_complete_enclosing_dispatch_are_retained() {
    let cast = cast_fixture();
    assert!(proof::prove(&cast, &cast.methods[0], 0, 9).is_some());
    let rendered = native_java::render_method("sample.Test", &cast, &cast.methods[0]).unwrap();
    assert!(
        rendered.source.find("new sample.A").unwrap()
            < rendered.source.find("((java.lang.String) p1)").unwrap(),
        "{}",
        rendered.source
    );
    let c = protected_fixture();
    assert!(proof::prove(&c, &c.methods[0], 0, 10).is_some());
    let rendered = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
    assert_eq!(rendered.source.matches("catch (").count(), 1);
    assert!(
        rendered.source.contains("sample.Source.handled("),
        "{}",
        rendered.source
    );
    let mut c = fixture();
    c.methods[0]
        .code
        .as_mut()
        .unwrap()
        .instructions
        .resize(4097, 0);
    assert!(proof::prove(&c, &c.methods[0], 0, 10).is_none());
}
#[test]
fn complete_forward_region_and_raw_reproof() {
    let c = fixture();
    let m = &c.methods[0];
    let p = proof::prove(&c, m, 0, 10).unwrap();
    assert_eq!(p.owned, vec![2, 4, 7, 8, 9]);
    assert!(proof::validate(&c, m, &p));
    assert_eq!(p.definitely_written, vec![1]);
    assert_eq!(p.first_reads.len(), 1);
    for mutation in 0..10 {
        let mut b = p.clone();
        match mutation {
            0 => b.words[3] += 1,
            1 => b.registers += 1,
            2 => {
                b.owned.pop();
            }
            3 => {
                b.edges.pop();
            }
            4 => b.first_reads[0].register = 0,
            5 => b.definitely_written.clear(),
            6 => b.calls.clear(),
            7 => b.semantic_references.clear(),
            8 => b.constructor = 9,
            9 => b.ins = 0,
            _ => unreachable!(),
        };
        assert!(!proof::validate(&c, m, &b), "poison {mutation}");
    }
    let rendered = native_java::render_method("sample.Test", &c, m).unwrap();
    assert!(
        rendered.source.contains("new sample.A(switch (0)"),
        "{}",
        rendered.source
    );
    assert!(
        rendered.source.find("new sample.A").unwrap()
            < rendered.source.find("sample.Source.value()").unwrap()
    );
    for link in &rendered.links {
        assert!(link.start <= link.end && link.end <= rendered.source.chars().count());
    }
}
#[test]
fn raw_escaping_pending_alias_allocation_and_handler_edges_reject() {
    for mutation in 0..9 {
        let mut c = fixture();
        let code = c.methods[0].code.as_mut().unwrap();
        match mutation {
            0 => code.instructions[3] = 11, // escape past original constructor
            1 => code.instructions[3] = 5,  // branch into move-result
            2 => code.instructions[8] = 0xf828, // backedge escapes to outer allocation
            3 => code.instructions[4] = 0x0022, // nested allocation
            4 => code.instructions[4] = 0x001a, // deferred string
            5 => code.instructions[2] = 0x0038, // read uninitialized receiver
            6 => code.instructions[9] = 0x0012, // destroys receiver on one arm
            7 => {
                code.tries = 1;
                code.try_regions.push(DexTryRegion {
                    start: 2,
                    end: 10,
                    catches: vec![(None, 13)].into(),
                });
            }
            8 => {
                code.tries = 1;
                code.try_regions.push(DexTryRegion {
                    start: 0,
                    end: 13,
                    catches: vec![(None, 7)].into(),
                });
            }
            _ => unreachable!(),
        }
        assert!(
            proof::prove(&c, &c.methods[0], 0, 10).is_none(),
            "negative {mutation}"
        );
    }
}
#[test]
fn every_original_reference_receiver_and_argument_requires_positive_domain() {
    for receiver in [true, false] {
        let mut c = fixture();
        let symbols = Arc::get_mut(&mut c.symbols).unwrap();
        symbols.strings = vec!["provider".into(), "value".into(), "<init>".into()];
        symbols.types.push("Ljava/lang/String;".into());
        symbols.protos[0] = ("Ljava/lang/Object;".into(), vec![]);
        symbols.protos.push((
            "I".into(),
            if receiver {
                vec![]
            } else {
                vec!["Ljava/lang/String;".into()]
            },
        ));
        symbols.methods = vec![(1, 0, 0), (1, 2, 1), (0, 1, 2)];
        let m = &mut c.methods[0];
        m.parameters.clear();
        let code = m.code.as_mut().unwrap();
        code.ins = 0;
        code.instructions = vec![
            0x0022,
            0,
            0x0071,
            0,
            0,
            0x010c,
            if receiver { 0x1072 } else { 0x1071 },
            1,
            1,
            0x010a,
            0x2070,
            2,
            0x0010,
            0x0011,
        ];
        assert!(proof::prove(&c, &c.methods[0], 0, 10).is_some());
        assert!(
            native_java::render_method("sample.Test", &c, &c.methods[0]).is_err(),
            "missing original cast: receiver={receiver}"
        );
        c.methods[0]
            .code
            .as_mut()
            .unwrap()
            .instructions
            .splice(6..6, [0x011f, if receiver { 1 } else { 2 }]);
        let rendered = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
        assert!(
            rendered.source.contains(if receiver {
                "sample.Source)"
            } else {
                "java.lang.String)"
            }),
            "{}",
            rendered.source
        );
    }
}
// Independent execution reads original encoded registers, offsets and method-pool identity.
fn oracle(c: &DexClass, branch: bool, mask: u8) -> String {
    let w = &c.methods[0].code.as_ref().unwrap().instructions;
    let mut r = [0i32; 3];
    r[2] = i32::from(branch);
    let mut pc = 0;
    let mut pending = 0;
    let mut trace = String::new();
    loop {
        let op = w[pc] as u8;
        let a = (w[pc] >> 8) as usize;
        match op {
            0x22 => {
                trace.push('N');
                if mask & 1 != 0 {
                    return format!("{trace}:N");
                }
                r[a] = -1;
                pc += 2;
            }
            0x38 => {
                pc = if r[a] == 0 {
                    (pc as isize + w[pc + 1] as i16 as isize) as usize
                } else {
                    pc + 2
                };
            }
            0x71 => {
                let id = w[pc + 1] as usize;
                let (_, proto, name) = c.symbols.methods[id];
                assert_eq!(c.symbols.strings[name as usize], "value");
                assert!(c.symbols.protos[proto as usize].1.is_empty());
                trace.push('S');
                if mask & 2 != 0 {
                    return format!("{trace}:S");
                }
                pending = 42;
                pc += 3;
            }
            0x0a => {
                r[a] = pending;
                pc += 1;
            }
            0x28 => pc = (pc as isize + (w[pc] >> 8) as u8 as i8 as isize) as usize,
            0x12 => {
                r[a & 15] = (a as u8 as i8 >> 4) as i32;
                pc += 1;
            }
            0x70 => {
                let packed = w[pc + 2];
                let recv = (packed & 15) as usize;
                let arg = ((packed >> 4) & 15) as usize;
                assert_eq!(r[recv], -1);
                trace.push('C');
                if mask & 4 != 0 {
                    return format!("{trace}:C");
                }
                r[recv] = r[arg];
                pc += 3;
            }
            0x11 => return format!("{trace}:{}", r[a]),
            _ => panic!("unsupported oracle opcode {op:x}@{pc}"),
        }
    }
}
fn oracle_wide(c: &DexClass, branch: bool, mask: u8, seed: i64) -> String {
    let w = &c.methods[0].code.as_ref().unwrap().instructions;
    let mut r = vec![0u32; 8];
    r[4] = u32::from(branch);
    r[5] = seed as u32;
    r[6] = (seed as u64 >> 32) as u32;
    r[7] = 0xdefa;
    let mut pc = 0;
    let mut pending = 0i64;
    let mut trace = String::new();
    let wide = |r: &[u32], p: usize| ((r[p] as u64) | ((r[p + 1] as u64) << 32)) as i64;
    loop {
        let op = w[pc] as u8;
        let a = (w[pc] >> 8) as usize;
        match op {
            0x22 => {
                trace.push('N');
                if mask & 1 != 0 {
                    return format!("{trace}:N");
                }
                r[a] = 0xffff_ffff;
                pc += 2;
            }
            0x38 => {
                pc = if r[a] == 0 {
                    (pc as isize + w[pc + 1] as i16 as isize) as usize
                } else {
                    pc + 2
                }
            }
            0x71 => {
                let packed = w[pc + 2];
                let regs = [
                    (packed & 15) as usize,
                    ((packed >> 4) & 15) as usize,
                    ((packed >> 8) & 15) as usize,
                ];
                assert_eq!(regs[1], regs[0] + 1);
                assert_eq!(r[regs[2]], 0xdefa);
                trace.push('S');
                if mask & 2 != 0 {
                    return format!("{trace}:S");
                }
                pending = wide(&r, regs[0]) ^ 0x1234_5678_9abc_def0;
                pc += 3;
            }
            0x0b => {
                r[a] = pending as u32;
                r[a + 1] = (pending as u64 >> 32) as u32;
                pc += 1;
            }
            0x28 => pc = (pc as isize + (w[pc] >> 8) as u8 as i8 as isize) as usize,
            0x16 => {
                let n = w[pc + 1] as i16 as i64;
                r[a] = n as u32;
                r[a + 1] = (n as u64 >> 32) as u32;
                pc += 2;
            }
            0x70 => {
                let packed = w[pc + 2];
                assert_eq!(r[(packed & 15) as usize], 0xffff_ffff);
                assert_eq!(r[((packed >> 12) & 15) as usize], 0xdefa);
                trace.push('C');
                if mask & 4 != 0 {
                    return format!("{trace}:C");
                }
                pc += 3;
            }
            0x10 => return format!("{trace}:{}", wide(&r, a)),
            _ => panic!("unsupported wide opcode {op:x}@{pc}"),
        }
    }
}
fn oracle_store(c: &DexClass, mask: u8) -> String {
    let w = &c.methods[0].code.as_ref().unwrap().instructions;
    let mut r = [0i32; 3];
    let mut pc = 0;
    let mut field = 0;
    let mut ready = false;
    let mut trace = String::new();
    loop {
        let op = w[pc] as u8;
        let a = (w[pc] >> 8) as usize;
        match op {
            0x22 => {
                trace.push('N');
                if mask & 1 != 0 {
                    return format!("{trace}:N");
                }
                r[a] = -1;
                pc += 2;
            }
            0x12 => {
                r[a & 15] = (a as u8 as i8 >> 4) as i32;
                pc += 1;
            }
            0x67 | 0x60 => {
                let index = w[pc + 1] as usize;
                assert_eq!(c.symbols.fields[index].0, 1);
                if !ready {
                    trace.push('F');
                    if mask & 16 != 0 {
                        return format!("{trace}:F");
                    }
                    ready = true;
                }
                if op == 0x67 {
                    field = r[a];
                } else {
                    r[a] = field;
                }
                pc += 2;
            }
            0x70 => {
                let packed = w[pc + 2];
                let recv = (packed & 15) as usize;
                let arg = ((packed >> 4) & 15) as usize;
                trace.push('C');
                if mask & 4 != 0 {
                    return format!("{trace}:C:{field}");
                }
                r[recv] = r[arg];
                pc += 3;
            }
            0x11 => return format!("{trace}:{}:{field}", r[a]),
            _ => panic!("unexpected store opcode {op:x}@{pc}"),
        }
    }
}
#[test]
#[ignore = "requires explicit JDK25"]
fn unchanged_emission_preserves_new_class_initialization_and_both_branch_faults() {
    let c = fixture();
    let code = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
    let sc = store_fixture();
    let storecode = native_java::render_method("sample.Test", &sc, &sc.methods[0]).unwrap();
    let pc = protected_fixture();
    let protectedcode = native_java::render_method("sample.Test", &pc, &pc.methods[0]).unwrap();
    let cc = cast_fixture();
    let castcode = native_java::render_method("sample.Test", &cc, &cc.methods[0]).unwrap();
    let wc = wide_fixture();
    let widecode = native_java::render_method("sample.Test", &wc, &wc.methods[0]).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-structured-argument-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let mut cases = vec![];
    for branch in [false, true] {
        for mask in 0..8 {
            cases.push(format!(
                "check({}, {}, \"{}\");",
                branch,
                mask,
                oracle(&c, branch, mask)
            ));
        }
    }
    for branch in [false, true] {
        for mask in 0..8 {
            for seed in [i64::MIN, i64::MAX, -1, 0, 7, 0x4567_89ab_cdef_1234] {
                let literal = if seed == i64::MIN {
                    "Long.MIN_VALUE".into()
                } else {
                    format!("{seed}L")
                };
                cases.push(format!(
                    "checkWide({}, {}, {}, \"{}\");",
                    branch,
                    mask,
                    literal,
                    oracle_wide(&wc, branch, mask, seed)
                ));
            }
        }
    }
    for branch in [false, true] {
        for mask in 0..16 {
            let mut expected = oracle(&pc, branch, mask);
            if expected.ends_with(":S") || expected.ends_with(":C") {
                expected.truncate(expected.len() - 2);
                expected.push_str(if mask & 8 != 0 { "H:H" } else { "H:null" });
            }
            cases.push(format!(
                "checkProtected({}, {}, \"{}\");",
                branch, mask, expected
            ));
        }
    }
    for branch in [false, true] {
        for mask in 0..8 {
            for kind in 0..3 {
                let expected = if mask & 1 != 0 {
                    "N:N"
                } else if branch && kind == 2 {
                    "N:cast"
                } else if mask & 4 != 0 {
                    "NC:C"
                } else {
                    "NC:0"
                };
                cases.push(format!(
                    "checkCast({}, {}, {}, \"{}\");",
                    branch, mask, kind, expected
                ));
            }
        }
    }
    for mask in [0, 1, 4, 5, 16, 17, 20, 21] {
        cases.push(format!(
            "checkStore({}, \"{}\");",
            mask,
            oracle_store(&sc, mask)
        ));
    }
    fs::write(dir.join("sample/Test.java"),format!(r#"package sample;
public class Test {{ {} {} {} {} {}
public static String runStore(int mask) {{State.mask=mask;try{{A a=makeStore();return State.trace+":"+a.value+":"+Store.number;}}catch(Throwable fault){{String result=outcome(fault);return fault==State.c?result+":"+Store.number:result;}}}}
public static String runCast(boolean branch,int mask,int kind) {{State.mask=mask;Object value=kind==0?null:kind==1?"ok":State.ref;try{{A a=makeCast(branch,value);return State.trace+":"+a.value;}}catch(ClassCastException fault){{return State.trace+":cast";}}catch(Throwable fault){{return outcome(fault);}}}}
public static String runProtected(boolean branch,int mask) {{State.mask=mask;try{{A a=makeProtected(branch);return State.trace+":"+(a==null?"null":a.value);}}catch(Throwable fault){{return outcome(fault);}}}}
public static String runWide(boolean branch,int mask,long seed) {{ State.mask=mask;State.seed=seed;try {{long result=makeWide(branch,seed,State.ref);return State.trace+":"+result;}}catch(Throwable fault){{return outcome(fault);}} }}
static String outcome(Throwable fault){{if(fault==State.n)return State.trace+":N";if(fault==State.s)return State.trace+":S";if(fault==State.c)return State.trace+":C";if(fault==State.h)return State.trace+":H";if(fault==State.f)return State.trace+":F";throw new AssertionError("unexpected fault identity",fault);}}
public static String run(boolean branch,int mask) {{ State.mask=mask;try {{ A a=make(branch);return State.trace+":"+a.value; }}catch(Throwable fault){{if(fault==State.n)return State.trace+":N";if(fault==State.s)return State.trace+":S";if(fault==State.c)return State.trace+":C";if(fault==State.h)return State.trace+":H";if(fault==State.f)return State.trace+":F";throw new AssertionError("unexpected fault identity",fault);}} }} }}
class State {{ static int mask;static long seed;static final Object ref=new Object();static String trace="";static final AssertionError n=new AssertionError("N"),f=new AssertionError("F");static final RuntimeException s=new RuntimeException("S"),c=new RuntimeException("C"),h=new RuntimeException("H"); }}
class Store {{static int number;static {{State.trace+="F";if((State.mask&16)!=0)throw State.f;}}}}
class Source {{static void handled(RuntimeException fault){{if(fault!=State.s&&fault!=State.c)throw new AssertionError("handler identity");State.trace+="H";if((State.mask&8)!=0)throw State.h;}}static long value(long seed,Object ref){{if(seed!=State.seed||ref!=State.ref)throw new AssertionError("input identity");State.trace+="S";if((State.mask&2)!=0)throw State.s;return seed^0x123456789abcdef0L;}} static int value() {{State.trace+="S";if((State.mask&2)!=0)throw State.s;return 42;}} }}
class A {{static {{State.trace+="N";if((State.mask&1)!=0)throw State.n;}}A(String value){{State.trace+="C";if((State.mask&4)!=0)throw State.c;this.value=0;}} A(long value,Object ref){{if(ref!=State.ref)throw new AssertionError("ctor reference");State.trace+="C";if((State.mask&4)!=0)throw State.c;this.value=0;}} final int value;A(int value){{State.trace+="C";if((State.mask&4)!=0)throw State.c;this.value=value;}} }}
"#,code.source,widecode.source,protectedcode.source,castcode.source,storecode.source)).unwrap();
    fs::write(dir.join("Runner.java"),format!(r#"import java.net.*;import java.nio.file.*;
public class Runner {{static int count;static void checkStore(int mask,String expected)throws Exception{{checkSpecial("runStore",new Class<?>[]{{int.class}},new Object[]{{mask}},expected);}}static void checkProtected(boolean branch,int mask,String expected)throws Exception{{checkSpecial("runProtected",new Class<?>[]{{boolean.class,int.class}},new Object[]{{branch,mask}},expected);}}static void checkCast(boolean branch,int mask,int kind,String expected)throws Exception{{checkSpecial("runCast",new Class<?>[]{{boolean.class,int.class,int.class}},new Object[]{{branch,mask,kind}},expected);}}static void checkSpecial(String name,Class<?>[] types,Object[] inputs,String expected)throws Exception{{try(URLClassLoader loader=new URLClassLoader(new URL[]{{Path.of(System.getProperty("classes")).toUri().toURL()}},null)){{Class<?> t=Class.forName("sample.Test",true,loader);String actual=(String)t.getMethod(name,types).invoke(null,inputs);if(!expected.equals(actual))throw new AssertionError(expected+" != "+actual);count++;}}}}static void checkWide(boolean branch,int mask,long seed,String expected)throws Exception{{try(URLClassLoader loader=new URLClassLoader(new URL[]{{Path.of(System.getProperty("classes")).toUri().toURL()}},null)){{Class<?> t=Class.forName("sample.Test",true,loader);String actual=(String)t.getMethod("runWide",boolean.class,int.class,long.class).invoke(null,branch,mask,seed);if(!expected.equals(actual))throw new AssertionError(expected+" != "+actual);count++;}}}}static void check(boolean branch,int mask,String expected)throws Exception{{try(URLClassLoader loader=new URLClassLoader(new URL[]{{Path.of(System.getProperty("classes")).toUri().toURL()}},null)){{Class<?> t=Class.forName("sample.Test",true,loader);String actual=(String)t.getMethod("run",boolean.class,int.class).invoke(null,branch,mask);if(!expected.equals(actual))throw new AssertionError(expected+" != "+actual);count++;}}}}public static void main(String[] args)throws Exception{{{}System.out.print("ok:"+count);}}}}
"#,cases.join("\n"))).unwrap();
    let home = std::env::var("RDX_JAVA25_HOME").expect("explicit JDK25 home");
    let javac = Command::new(format!("{home}/bin/javac"))
        .arg(dir.join("sample/Test.java"))
        .arg(dir.join("Runner.java"))
        .output()
        .unwrap();
    assert!(
        javac.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&javac.stderr),
        code.source
    );
    let java = Command::new(format!("{home}/bin/java"))
        .arg("-Xverify:all")
        .arg(format!("-Dclasses={}", dir.display()))
        .arg("-cp")
        .arg(&dir)
        .arg("Runner")
        .output()
        .unwrap();
    assert!(
        java.status.success(),
        "{}",
        String::from_utf8_lossy(&java.stderr)
    );
    assert_eq!(java.stdout, b"ok:200");
    fs::remove_dir_all(dir).unwrap();
}

fn earlier_string_fixture(jumbo: bool, moved: bool) -> DexClass {
    let mut class = string_effect_fixture();
    let code = class.methods[0].code.as_mut().unwrap();
    code.registers = 4;
    code.instructions = if jumbo {
        vec![0x011b, 2, 0]
    } else {
        vec![0x011a, 2]
    };
    if moved {
        code.instructions.push(0x1207);
    }
    code.instructions.extend([
        0x0071,
        2,
        0,
        0x0022,
        0,
        0x0071,
        2,
        0,
        0x2070,
        1,
        if moved { 0x0020 } else { 0x0010 },
        0x0011,
    ]);
    class
}
fn earlier_phi_fixture() -> DexClass {
    let mut class = string_effect_fixture();
    class.methods[0].code.as_mut().unwrap().instructions = vec![
        0x0238, 5, 0x011a, 2, 0x0328, 0x011a, 3, 0x0022, 0, 0x0071, 2, 0, 0x2070, 1, 0x0010, 0x0011,
    ];
    class
}
#[test]
fn earlier_literal_moves_and_phi_inputs_materialize_at_original_producers() {
    for jumbo in [false, true] {
        for moved in [false, true] {
            let class = earlier_string_fixture(jumbo, moved);
            let rendered =
                native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
            assert!(
                rendered.source.find("é🙂").unwrap()
                    < rendered.source.find("sample.Source.effect()").unwrap(),
                "{}",
                rendered.source
            );
            assert!(
                rendered.source.find("sample.Source.effect()").unwrap()
                    < rendered.source.find("new sample.A").unwrap(),
                "{}",
                rendered.source
            );
            assert_eq!(rendered.source.matches("sample.Source.effect()").count(), 2);
        }
    }
    let class = earlier_phi_fixture();
    let rendered = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    for text in ["é🙂", "alternative"] {
        assert!(
            rendered.source.find(text).unwrap() < rendered.source.find("new sample.A").unwrap(),
            "{}",
            rendered.source
        );
    }
}

#[test]
fn earlier_producer_cache_keeps_missing_phi_and_raw_domains_conservative() {
    let mut undefined = earlier_phi_fixture();
    undefined.methods[0].code.as_mut().unwrap().instructions[5] = 0;
    undefined.methods[0].code.as_mut().unwrap().instructions[6] = 0;
    assert!(native_java::render_method("sample.Test", &undefined, &undefined.methods[0]).is_err());
    let mut invalid = earlier_string_fixture(false, true);
    Arc::get_mut(&mut invalid.symbols).unwrap().protos[1].1 = vec!["Lsample/Source;".into()];
    assert!(native_java::render_method("sample.Test", &invalid, &invalid.methods[0]).is_err());
    let mut noarg = earlier_string_fixture(false, false);
    Arc::get_mut(&mut noarg.symbols).unwrap().protos[1]
        .1
        .clear();
    let words = &mut noarg.methods[0].code.as_mut().unwrap().instructions;
    words[10] = 0x1070;
    words[12] = 0;
    assert!(native_java::render_method("sample.Test", &noarg, &noarg.methods[0]).is_err());
}

#[test]
fn cached_argument_region_inputs_match_independent_raw_proof() {
    for class in [
        fixture(),
        string_fixture(true),
        earlier_string_fixture(true, true),
        earlier_phi_fixture(),
        loop_fixture(),
        nested_fixture(),
        canonical_earlier_fixture(0),
        canonical_earlier_fixture(1),
        canonical_earlier_fixture(2),
        large_external_cfg_fixture(),
    ] {
        let method = &class.methods[0];
        let code = method.code.as_ref().unwrap();
        let analysis = native_method::MethodAnalysis::build(&class, method).unwrap();
        let constructors = analysis.constructors().unwrap();
        let front = native_method::MethodFrontEnd::build(&class, method).unwrap();
        let cfg = native_cfg::ControlFlowGraph::build(code).unwrap();
        for binding in &constructors.bindings {
            let native_constructors::ConstructorOrigin::Allocation { pc, .. } = binding.origin
            else {
                continue;
            };
            assert_eq!(
                proof::prove(&class, method, pc, binding.invoke_pc),
                proof::prove_cached(
                    &class,
                    method,
                    pc,
                    binding.invoke_pc,
                    proof::ProofInputs {
                        ir: &front.ir,
                        cfg: &cfg,
                        bound: &front.bound,
                        bindings: &constructors.bindings
                    }
                )
            );
        }
    }
}

fn namespace_fixture(kind: usize) -> DexClass {
    let mut class = string_effect_fixture();
    let symbols = Arc::get_mut(&mut class.symbols).unwrap();
    symbols.strings[2] = "v9.Source".into();
    if kind == 0 {
        symbols.types[1] = "Lv9/Source;".into();
    }
    if kind == 1 {
        symbols.types.extend(["Lv9/Holder;".into(), "I".into()]);
        symbols.fields.push((2, 3, 0));
    }
    if kind == 2 {
        symbols.types.push("Lv9/Holder;".into());
    }
    if kind == 3 {
        class.descriptor = "Lv9/Test;".into();
        class.methods[0].declaring_type = class.descriptor.clone();
    }
    if kind == 4 {
        class.methods[0].parameters = vec!["[Lv9/Holder;".into()];
    }
    if kind == 5 {
        symbols.types.push("Lv9/Outer$Holder;".into());
    }
    if kind == 6 {
        symbols.types.push("Lv9/Unused;".into());
    }
    let code = class.methods[0].code.as_mut().unwrap();
    code.instructions = vec![0x0022, 0];
    for _ in 0..10 {
        code.instructions.extend([0x011a, 2]);
    }
    if kind == 1 {
        code.instructions.extend([0x0260, 0]);
    }
    if kind == 2 || kind == 5 {
        code.instructions.extend([0x021c, 2]);
    }
    code.instructions
        .extend([0x0071, 2, 0, 0x2070, 1, 0x0010, 0x0011]);
    if kind == 7 {
        class.methods[0].thrown_types.push("Lv9/Fault;".into());
    }
    if kind == 8 {
        let code = class.methods[0].code.as_mut().unwrap();
        let handler = code.instructions.len() as u32;
        code.tries = 1;
        code.try_regions.push(DexTryRegion {
            start: 0,
            end: handler - 1,
            catches: vec![(Some("Lv9/Fault;".into()), handler)].into(),
        });
        code.instructions.extend([0x010d, 0x0127]);
    }
    if kind == 7 || kind == 8 {
        let mut fault = fixture();
        fault.descriptor = "Lv9/Fault;".into();
        fault.superclass = Some("Ljava/lang/RuntimeException;".into());
        fault.methods.clear();
        let hierarchy =
            Arc::new(rdx::native_hierarchy::TypeHierarchy::from_classes([&class, &fault]).unwrap());
        assert!(class.symbols.hierarchy.set(hierarchy).is_ok());
    }
    class
}
#[test]
fn method_references_reserve_package_type_field_and_method_roots() {
    for kind in [0, 1, 2, 3, 4, 5, 7, 8] {
        let class = namespace_fixture(kind);
        let rendered =
            native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
        assert!(
            !rendered.source.contains(" v9;") && !rendered.source.contains(" v9 ="),
            "kind={kind} {}",
            rendered.source
        );
        assert_eq!(rendered.source.matches("new sample.A").count(), 1);
    }
    let control = namespace_fixture(6);
    let rendered =
        native_java::render_method("sample.Test", &control, &control.methods[0]).unwrap();
    assert!(rendered.source.contains(" v9;"), "{}", rendered.source);
}

#[test]
#[ignore = "requires explicit JDK25"]
fn qualified_namespace_is_available_after_hoisting_and_constructor_faults() {
    let class = namespace_fixture(0);
    let rendered = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-namespace-order-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::create_dir_all(dir.join("v9")).unwrap();
    fs::write(dir.join("sample/Test.java"),format!(r#"package sample;
public class Test {{ {} public static String run(int mask){{State.mask=mask;try{{A result=make(false);return State.trace+":"+result.value;}}catch(Throwable error){{if(error==State.fault)return State.trace+":fault";throw new AssertionError(error);}}}} }}
class A {{static {{State.event("A",1);}}final String value;A(String value){{State.event("C",8);this.value=value;}}}}
"#,rendered.source)).unwrap();
    fs::write(dir.join("sample/State.java"),r#"package sample;public class State {public static int mask;public static String trace="";public static final AssertionError fault=new AssertionError();public static void event(String text,int bit){trace+=text;if((mask&bit)!=0)throw fault;}}"#).unwrap();
    fs::write(dir.join("v9/Source.java"),r#"package v9;public class Source {static {sample.State.event("B",4);}public static void effect(){sample.State.event("E",2);}}"#).unwrap();
    fs::write(dir.join("Runner.java"),format!(r#"import java.net.*;import java.nio.file.*;public class Runner{{public static void main(String[]args)throws Exception{{for(int mask=0;mask<16;mask++){{try(URLClassLoader loader=new URLClassLoader(new URL[]{{Paths.get("{}").toUri().toURL()}},null)){{String expected=(mask&1)!=0?"A:fault":(mask&4)!=0?"AB:fault":(mask&2)!=0?"ABE:fault":(mask&8)!=0?"ABEC:fault":"ABEC:v9.Source";Object actual=loader.loadClass("sample.Test").getMethod("run",int.class).invoke(null,mask);if(!expected.equals(actual))throw new AssertionError(expected+" != "+actual);}}}}System.out.print("ok:16");}}}}"#,dir.display())).unwrap();
    let home = std::env::var("RDX_JAVA25_HOME").expect("explicit JDK25 home");
    let compiled = Command::new(format!("{home}/bin/javac"))
        .arg(dir.join("sample/Test.java"))
        .arg(dir.join("sample/State.java"))
        .arg(dir.join("v9/Source.java"))
        .arg(dir.join("Runner.java"))
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&compiled.stderr),
        rendered.source
    );
    let executed = Command::new(format!("{home}/bin/java"))
        .args(["-cp", dir.to_str().unwrap(), "Runner"])
        .output()
        .unwrap();
    assert!(
        executed.status.success(),
        "{}",
        String::from_utf8_lossy(&executed.stderr)
    );
    assert_eq!(executed.stdout, b"ok:16");
    fs::remove_dir_all(&dir).unwrap();
}

fn namespace_capture_fixture() -> DexClass {
    let mut class = fixture();
    Arc::get_mut(&mut class.symbols).unwrap().types[1] = "Lv9/Source;".into();
    class.methods[0].parameters.clear();
    class.methods[0].return_type = "I".into();
    let code = class.methods[0].code.as_mut().unwrap();
    code.ins = 0;
    code.instructions = vec![0x0022, 0];
    for _ in 0..12 {
        code.instructions.extend([0x0071, 0, 0, 0x010a]);
    }
    code.instructions.extend([0x2070, 1, 0x0010, 0x010f]);
    class
}
#[test]
#[ignore = "requires explicit JDK25"]
fn capture_names_skip_namespaces_and_live_return_keeps_original_value() {
    let class = namespace_capture_fixture();
    let rendered = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert!(
        !rendered.source.contains(" v9;") && !rendered.source.contains(" v9 ="),
        "{}",
        rendered.source
    );
    assert_eq!(
        rendered.source.matches("v9.Source.value()").count(),
        12,
        "{}",
        rendered.source
    );
    let dir = std::env::temp_dir().join(format!("rdx-capture-name-hole-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::create_dir_all(dir.join("v9")).unwrap();
    fs::write(dir.join("sample/Test.java"),format!(r#"package sample;
public class Test {{ {} public static String run(int mask){{State.mask=mask;try{{int result=make();return State.trace+":"+result;}}catch(Throwable error){{if(error==State.fault)return State.trace+":fault";throw new AssertionError(error);}}}} }}
class A {{static {{State.event("A",1);}}A(int value){{State.event("C",8);if(value!=12)throw new AssertionError(value);}}}}
"#,rendered.source)).unwrap();
    fs::write(dir.join("sample/State.java"),r#"package sample;public class State {public static int mask,count;public static String trace="";public static final AssertionError fault=new AssertionError();public static void event(String text,int bit){trace+=text;if((mask&bit)!=0)throw fault;}}"#).unwrap();
    fs::write(dir.join("v9/Source.java"),r#"package v9;public class Source {static {sample.State.event("B",4);}public static int value(){int count=++sample.State.count;sample.State.event("S",count==5?2:0);return count;}}"#).unwrap();
    let normal = format!("AB{}C", "S".repeat(12));
    fs::write(dir.join("Runner.java"),format!(r#"import java.net.*;import java.nio.file.*;public class Runner{{public static void main(String[]args)throws Exception{{for(int mask=0;mask<16;mask++){{try(URLClassLoader loader=new URLClassLoader(new URL[]{{Paths.get("{}").toUri().toURL()}},null)){{String expected=(mask&1)!=0?"A:fault":(mask&4)!=0?"AB:fault":(mask&2)!=0?"ABSSSSS:fault":(mask&8)!=0?"{}:fault":"{}:12";Object actual=loader.loadClass("sample.Test").getMethod("run",int.class).invoke(null,mask);if(!expected.equals(actual))throw new AssertionError(expected+" != "+actual);}}}}System.out.print("ok:16");}}}}"#,dir.display(),normal,normal)).unwrap();
    let home = std::env::var("RDX_JAVA25_HOME").expect("explicit JDK25 home");
    let compiled = Command::new(format!("{home}/bin/javac"))
        .arg(dir.join("sample/Test.java"))
        .arg(dir.join("sample/State.java"))
        .arg(dir.join("v9/Source.java"))
        .arg(dir.join("Runner.java"))
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&compiled.stderr),
        rendered.source
    );
    let executed = Command::new(format!("{home}/bin/java"))
        .args(["-cp", dir.to_str().unwrap(), "Runner"])
        .output()
        .unwrap();
    assert!(
        executed.status.success(),
        "{}",
        String::from_utf8_lossy(&executed.stderr)
    );
    assert_eq!(executed.stdout, b"ok:16");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn namespace_reservations_skip_only_used_names_without_large_sequence_jumps() {
    for prefix in ["v0", "v1", "v999999999999999999999999999999"] {
        let mut class = namespace_fixture(0);
        Arc::get_mut(&mut class.symbols).unwrap().types[1] = format!("L{prefix}/Source;").into();
        let rendered =
            native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
        assert!(
            !rendered.source.contains(&format!(" {prefix};")),
            "{}",
            rendered.source
        );
        if prefix.starts_with("v999") {
            assert!(rendered.source.contains(" v0;"), "{}", rendered.source);
        }
    }
    let mut comment = namespace_fixture(6);
    Arc::get_mut(&mut comment.symbols).unwrap().strings[2] = "/* v9.Source */".into();
    assert!(
        native_java::render_method("sample.Test", &comment, &comment.methods[0])
            .unwrap()
            .source
            .contains(" v9;")
    );
}

fn canonical_earlier_fixture(shape: usize) -> DexClass {
    let mut c = string_effect_fixture();
    c.methods[0].parameters = vec!["I".into()];
    let code = c.methods[0].code.as_mut().unwrap();
    code.registers = if shape == 1 { 4 } else { 3 };
    code.instructions = match shape {
        0 => vec![
            0x011a, 2, 0x022b, 16, 0, 0x0328, 0x011a, 3, 0x0022, 0, 0x0071, 2, 0, 0x2070, 1,
            0x0010, 0x0011, 0, 0x0100, 1, 0, 0, 4, 0,
        ],
        1 => vec![
            0x011a, 2, 0x033d, 7, 0x011a, 3, 0x03d8, 0xff03, 0xfa28, 0x0022, 0, 0x0071, 2, 0,
            0x2070, 1, 0x0010, 0x0011,
        ],
        2 => vec![
            0x011a, 2, 0x0071, 2, 0, 0x0528, 0x000d, 0x011a, 3, 0, 0x0022, 0, 0x0071, 2, 0, 0x2070,
            1, 0x0010, 0x0011,
        ],
        _ => unreachable!(),
    };
    if shape == 2 {
        code.tries = 1;
        code.try_regions = vec![DexTryRegion {
            start: 2,
            end: 5,
            catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 6)].into(),
        }];
    }
    c
}
#[test]
fn canonical_producers_keep_loop_payload_and_handler_literal_resolution() {
    for shape in 0..3 {
        let c = canonical_earlier_fixture(shape);
        let result = native_java::render_method("sample.Test", &c, &c.methods[0]);
        let rendered = result.unwrap_or_else(|e| panic!("shape={shape}: {e:#}"));
        for literal in ["é🙂", "alternative"] {
            assert!(
                rendered.source.find(literal).unwrap()
                    < rendered.source.find("new sample.A").unwrap(),
                "shape={shape}: {}",
                rendered.source
            );
        }
        assert_eq!(rendered.source.matches("new sample.A").count(), 1);
    }
    let mut missing = canonical_earlier_fixture(0);
    missing.methods[0].code.as_mut().unwrap().instructions[0] = 0;
    missing.methods[0].code.as_mut().unwrap().instructions[1] = 0;
    assert!(native_java::render_method("sample.Test", &missing, &missing.methods[0]).is_err());
    let mut alias = canonical_earlier_fixture(1);
    alias.methods[0].code.as_mut().unwrap().instructions[4] = 0x0107;
    alias.methods[0].code.as_mut().unwrap().instructions[5] = 0;
    assert!(native_java::render_method("sample.Test", &alias, &alias.methods[0]).is_err());
}

#[test]
#[ignore = "requires Java 25 via RDX_JAVA25_HOME"]
fn canonical_producer_java_paths_and_faults_match_original_order() {
    let home = std::env::var("RDX_JAVA25_HOME").expect("RDX_JAVA25_HOME");
    let dir = std::env::temp_dir().join(format!("rdx-canonical-producer-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/State.java"),r#"package sample; public class State { public static int mask; public static String events=""; public static void hit(String e,int b){events+=e;if((mask&b)!=0)throw new RuntimeException(e);} }"#).unwrap();
    fs::write(dir.join("sample/A.java"),r#"package sample; public class A { public final String text; static{State.hit("A",1);} public A(String s){State.hit("C",8);text=s;} }"#).unwrap();
    fs::write(dir.join("sample/Source.java"),r#"package sample; public class Source { static{State.hit("B",2);} public static void effect(){State.hit("E",4);} }"#).unwrap();
    fs::write(dir.join("Runner.java"),r#"import java.net.*;import java.nio.file.*;import java.lang.reflect.*;public class Runner { static String call(String type,int mask,int flag)throws Exception { try(URLClassLoader l=new URLClassLoader(new URL[]{Path.of(System.getProperty("classes")).toUri().toURL()},null)){Class<?> s=Class.forName("sample.State",true,l);s.getField("mask").setInt(null,mask);String result;try{Object a=Class.forName("sample."+type,true,l).getMethod("make",int.class).invoke(null,flag);result=(String)a.getClass().getField("text").get(a);}catch(InvocationTargetException e){result=e.getCause().getClass().getName();}return s.getField("events").get(null)+":"+result;}} public static void main(String[]x)throws Exception{int n=0;for(int m=0;m<16;m++)for(int p:new int[]{0,1,3}){String a=call("Test",m,p),b=call("Reference",m,p);if(!a.equals(b))throw new AssertionError(m+"/"+p+" "+a+" != "+b);n++;}System.out.print("ok:"+n);} }"#).unwrap();
    let prefixes = [
        r#"String text="é🙂";switch(flag){case 0:text="alternative";break;}"#,
        r#"String text="é🙂";while(flag>0){text="alternative";flag--;}"#,
        r#"String text="é🙂";try{Source.effect();}catch(RuntimeException e){text="alternative";}"#,
        r#"String text="é🙂";Source.effect();"#,
    ];
    for (shape, prefix) in prefixes.iter().enumerate() {
        let c = if shape == 3 {
            large_external_cfg_fixture()
        } else {
            canonical_earlier_fixture(shape)
        };
        let rendered = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
        fs::write(
            dir.join("sample/Test.java"),
            format!(
                "package sample; public class Test {{ {} }}",
                rendered.source
            ),
        )
        .unwrap();
        fs::write(dir.join("sample/Reference.java"),format!("package sample; public class Reference {{ public static A make(int flag) {{ {prefix} return new A(switch(0){{default->{{Source.effect();yield text;}}}}); }} }}")).unwrap();
        let javac = Command::new(format!("{home}/bin/javac"))
            .arg("--release")
            .arg("25")
            .arg("-d")
            .arg(&dir)
            .args([
                dir.join("sample/State.java"),
                dir.join("sample/A.java"),
                dir.join("sample/Source.java"),
                dir.join("sample/Test.java"),
                dir.join("sample/Reference.java"),
                dir.join("Runner.java"),
            ])
            .output()
            .unwrap();
        assert!(
            javac.status.success(),
            "shape={shape}: {}\n{}",
            String::from_utf8_lossy(&javac.stderr),
            rendered.source
        );
        let java = Command::new(format!("{home}/bin/java"))
            .arg("-Xverify:all")
            .arg(format!("-Dclasses={}", dir.display()))
            .arg("-cp")
            .arg(&dir)
            .arg("Runner")
            .output()
            .unwrap();
        assert!(
            java.status.success(),
            "shape={shape}: {}",
            String::from_utf8_lossy(&java.stderr)
        );
        assert_eq!(java.stdout, b"ok:48");
        let javap = Command::new(format!("{home}/bin/javap"))
            .arg("-c")
            .arg("-classpath")
            .arg(&dir)
            .arg("sample.Test")
            .output()
            .unwrap();
        assert!(javap.status.success());
        let bytecode = String::from_utf8(javap.stdout).unwrap();
        assert!(
            bytecode.find("String é🙂").unwrap() < bytecode.find("class sample/A").unwrap(),
            "shape={shape}: {bytecode}"
        );
    }
    fs::remove_dir_all(dir).unwrap();
}

fn large_external_cfg_fixture() -> DexClass {
    let mut class = earlier_string_fixture(false, false);
    class.methods[0].parameters[0] = "I".into();
    let mut prefix = Vec::new();
    for _ in 0..72 {
        prefix.extend([0x0338, 3, 0]);
    }
    prefix.extend(
        class.methods[0]
            .code
            .as_ref()
            .unwrap()
            .instructions
            .iter()
            .copied(),
    );
    class.methods[0].code.as_mut().unwrap().instructions = prefix;
    class
}
#[test]
fn allocation_proof_budget_owns_region_and_keeps_complete_foreign_edges() {
    let c = large_external_cfg_fixture();
    let m = &c.methods[0];
    let code = m.code.as_ref().unwrap();
    let cfg = native_cfg::ControlFlowGraph::build(code).unwrap();
    assert!(cfg.blocks.len() > 128);
    let base = earlier_string_fixture(false, false);
    let plan = proof::prove(&c, m, 221, 226).unwrap();
    let plain = proof::prove(&base, &base.methods[0], 5, 10).unwrap();
    assert_eq!(
        plan.owned.iter().map(|pc| pc - 216).collect::<Vec<_>>(),
        plain.owned
    );
    assert_eq!(plan.definitely_written, plain.definitely_written);
    let rendered = native_java::render_method("sample.Test", &c, m).unwrap();
    assert_eq!(rendered.source.matches("new sample.A").count(), 1);
    assert!(rendered.source.find("é🙂").unwrap() < rendered.source.find("new sample.A").unwrap());
    let mut external = large_external_cfg_fixture();
    external.methods[0].code.as_mut().unwrap().instructions[1] = 223;
    assert!(proof::prove(&external, &external.methods[0], 221, 226).is_none());
    let mut big = earlier_string_fixture(false, false);
    let mut region = vec![0x011a, 2, 0x0022, 0];
    for _ in 0..72 {
        region.extend([0x0338, 3, 0]);
    }
    region.extend([0x0071, 2, 0, 0x2070, 1, 0x0010, 0x0011]);
    big.methods[0].code.as_mut().unwrap().instructions = region;
    assert!(proof::prove(&big, &big.methods[0], 2, 223).is_none());
}

fn caught_argument_fixture() -> DexClass {
    let mut c = fixture();
    let symbols = Arc::get_mut(&mut c.symbols).unwrap();
    symbols.strings.extend(["cause".into(), "effect".into()]);
    symbols.protos[1].1 = vec!["Ljava/lang/Throwable;".into(), "Ljava/lang/String;".into()];
    symbols.protos.push(("V".into(), vec![]));
    symbols.methods.push((1, 2, 3));
    c.methods[0].parameters.clear();
    let code = c.methods[0].code.as_mut().unwrap();
    code.ins = 0;
    code.tries = 1;
    code.try_regions = vec![DexTryRegion {
        start: 0,
        end: 3,
        catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 5)].into(),
    }];
    code.instructions = vec![
        0x0071, 2, 0, 0x0012, 0x0011, 0x010d, 0x0022, 0, 0x021a, 2, 0x0071, 2, 0, 0x3070, 1,
        0x0210, 0x0011,
    ];
    let hierarchy = Arc::new(rdx::native_hierarchy::TypeHierarchy::from_classes([&c]).unwrap());
    c.symbols.hierarchy.set(hierarchy).unwrap();
    c
}
#[test]
fn caught_arguments_use_existing_handler_value_and_keep_original_domains() {
    let c = caught_argument_fixture();
    let code = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
    assert!(
        code.source.contains("catch (java.lang.RuntimeException e"),
        "{}",
        code.source
    );
    assert!(
        code.source.contains("new sample.A(switch (0)"),
        "{}",
        code.source
    );
    assert!(!code.source.contains("__rdx_caught__"), "{}", code.source);
    assert!(code.source.find("new sample.A").unwrap() < code.source.find("cause").unwrap());
    let mut invalid = caught_argument_fixture();
    Arc::get_mut(&mut invalid.symbols).unwrap().protos[1].1[0] = "Lsample/Source;".into();
    assert!(native_java::render_method("sample.Test", &invalid, &invalid.methods[0]).is_err());
}

#[test]
#[ignore = "requires Java 25 via RDX_JAVA25_HOME"]
fn caught_argument_java_identity_and_allocation_fault_order() {
    let home = std::env::var("RDX_JAVA25_HOME").expect("RDX_JAVA25_HOME");
    let dir = std::env::temp_dir().join(format!("rdx-caught-argument-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let c = caught_argument_fixture();
    let rendered = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
    fs::write(
        dir.join("sample/Test.java"),
        format!("package sample;public class Test {{{}}}", rendered.source),
    )
    .unwrap();
    fs::write(dir.join("sample/Reference.java"),r#"package sample;public class Reference {public static A make(){try{Source.effect();return null;}catch(RuntimeException e){return new A(e,switch(0){default->{String text="cause";Source.effect();yield text;}});}}}"#).unwrap();
    fs::write(dir.join("sample/State.java"),r#"package sample;public class State {public static int mask,calls;public static String events="";public static RuntimeException first=new RuntimeException("first"),second=new RuntimeException("second");public static void hit(String name,int bit){events+=name;if((mask&bit)!=0)throw new RuntimeException(name);}}"#).unwrap();
    fs::write(dir.join("sample/Source.java"),r#"package sample;public class Source {static{State.hit("S",1);}public static void effect(){int n=++State.calls;State.events+="E"+n;if(n==1&&(State.mask&2)!=0)throw State.first;if(n==2&&(State.mask&8)!=0)throw State.second;}}"#).unwrap();
    fs::write(dir.join("sample/A.java"),r#"package sample;public class A {static{State.hit("A",4);}public Throwable cause;public String text;public A(Throwable cause,String text){State.hit("C",16);this.cause=cause;this.text=text;}}"#).unwrap();
    fs::write(dir.join("Runner.java"),r#"import java.net.*;import java.nio.file.*;import java.lang.reflect.*;public class Runner {static String call(String type,int mask)throws Exception{try(URLClassLoader l=new URLClassLoader(new URL[]{Path.of(System.getProperty("classes")).toUri().toURL()},null)){Class<?> s=Class.forName("sample.State",true,l);s.getField("mask").setInt(null,mask);String result;try{Object a=Class.forName("sample."+type,true,l).getMethod("make").invoke(null);result=a==null?"null":(a.getClass().getField("cause").get(a)==s.getField("first").get(null))+":"+a.getClass().getField("text").get(a);}catch(InvocationTargetException e){Throwable t=e.getCause();result=t.getClass().getName()+":"+(t==s.getField("first").get(null))+":"+(t==s.getField("second").get(null));}return s.getField("events").get(null)+":"+result;}}public static void main(String[]args)throws Exception{for(int mask=0;mask<32;mask++){String a=call("Test",mask),b=call("Reference",mask);if(!a.equals(b))throw new AssertionError(mask+" "+a+" != "+b);}System.out.print("ok:32");}}"#).unwrap();
    let javac = Command::new(format!("{home}/bin/javac"))
        .arg("-d")
        .arg(&dir)
        .args(
            ["Test", "Reference", "State", "Source", "A"]
                .map(|name| dir.join(format!("sample/{name}.java"))),
        )
        .arg(dir.join("Runner.java"))
        .output()
        .unwrap();
    assert!(
        javac.status.success(),
        "{}\n{}",
        rendered.source,
        String::from_utf8_lossy(&javac.stderr)
    );
    let java = Command::new(format!("{home}/bin/java"))
        .arg(format!("-Dclasses={}", dir.display()))
        .arg("-cp")
        .arg(&dir)
        .arg("Runner")
        .output()
        .unwrap();
    assert!(
        java.status.success(),
        "{}",
        String::from_utf8_lossy(&java.stderr)
    );
    assert_eq!(java.stdout, b"ok:32");
    let javap = Command::new(format!("{home}/bin/javap"))
        .arg("-c")
        .arg("-classpath")
        .arg(&dir)
        .arg("sample.Test")
        .output()
        .unwrap();
    assert!(javap.status.success());
    let bytecode = String::from_utf8(javap.stdout).unwrap();
    assert!(
        bytecode.find("class sample/A").unwrap() < bytecode.find("String cause").unwrap(),
        "{bytecode}"
    );
    fs::remove_dir_all(dir).unwrap();
}

fn terminal_island_fixture() -> DexClass {
    let mut c = fixture();
    let symbols = Arc::get_mut(&mut c.symbols).unwrap();
    symbols
        .types
        .push("Ljava/lang/IllegalStateException;".into());
    symbols.strings.push("bad".into());
    symbols
        .protos
        .push(("V".into(), vec!["Ljava/lang/String;".into()]));
    symbols.methods.push((2, 2, 1));
    c.methods[0].code.as_mut().unwrap().instructions = vec![
        0x0022, 0, 0x0238, 10, 0x0071, 0, 0, 0x010a, 0x2070, 1, 0x0010, 0x0011, 0x0222, 2, 0x011a,
        2, 0x2070, 2, 0x0012, 0x0227,
    ];
    c
}
#[test]
fn terminal_island_arguments_preserve_allocation_before_throw_branch() {
    let c = terminal_island_fixture();
    let plan = proof::prove(&c, &c.methods[0], 0, 8).unwrap();
    assert_eq!(plan.terminal_islands, vec![(12, 20)]);
    let front = native_method::MethodFrontEnd::build(&c, &c.methods[0]).unwrap();
    let analysis = native_method::MethodAnalysis::build(&c, &c.methods[0]).unwrap();
    let constructors = analysis.constructors().unwrap();
    let cfg = native_cfg::ControlFlowGraph::build(c.methods[0].code.as_ref().unwrap()).unwrap();
    assert_eq!(
        Some(plan.clone()),
        proof::prove_cached(
            &c,
            &c.methods[0],
            0,
            8,
            proof::ProofInputs {
                ir: &front.ir,
                cfg: &cfg,
                bound: &front.bound,
                bindings: &constructors.bindings
            }
        )
    );

    let rendered = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
    assert!(rendered.source.contains("throw"), "{}", rendered.source);
    assert!(
        rendered.source.find("new sample.A").unwrap()
            < rendered
                .source
                .find("new java.lang.IllegalStateException")
                .unwrap(),
        "{}",
        rendered.source
    );
    let mut poisoned = plan.clone();
    poisoned.terminal_islands.clear();
    assert!(!proof::validate(&c, &c.methods[0], &poisoned));
    let mut overwrite = terminal_island_fixture();
    let words = &mut overwrite.methods[0].code.as_mut().unwrap().instructions;
    words[14] = 0x001a;
    words[18] = 0x0002;
    assert!(proof::prove(&overwrite, &overwrite.methods[0], 0, 8).is_some());
    assert!(native_java::render_method("sample.Test", &overwrite, &overwrite.methods[0]).is_ok());
    for (label, target) in [
        ("sample.A.<init>(I)V", "sample.A"),
        (
            "java.lang.IllegalStateException.<init>(Ljava/lang/String;)V",
            "java.lang.IllegalStateException",
        ),
        ("sample.Source.value()I", "value"),
    ] {
        assert!(
            rendered.links.iter().any(|link| link.label == label
                && rendered
                    .source
                    .chars()
                    .skip(link.start)
                    .take(link.end - link.start)
                    .collect::<String>()
                    == target),
            "missing {label}: {:?}",
            rendered.links
        );
    }
    for link in &rendered.links {
        assert!(link.start < link.end && link.end <= rendered.source.chars().count());
    }
}

#[test]
fn terminal_islands_reject_foreign_shared_nonterminal_and_handler_entries() {
    let mut c = terminal_island_fixture();
    let mut words = vec![0x0238, 14];
    words.extend(
        c.methods[0]
            .code
            .as_ref()
            .unwrap()
            .instructions
            .iter()
            .copied(),
    );
    c.methods[0].code.as_mut().unwrap().instructions = words;
    assert!(proof::prove(&c, &c.methods[0], 2, 10).is_none());
    for op in [0, 0x000e] {
        let mut c = terminal_island_fixture();
        c.methods[0].code.as_mut().unwrap().instructions[if op == 0 { 11 } else { 19 }] = op;
        assert!(proof::prove(&c, &c.methods[0], 0, 8).is_none());
    }
    let mut c = terminal_island_fixture();
    let code = c.methods[0].code.as_mut().unwrap();
    code.tries = 1;
    code.try_regions = vec![DexTryRegion {
        start: 12,
        end: 20,
        catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 11)].into(),
    }];
    assert!(proof::prove(&c, &c.methods[0], 0, 8).is_none());
    let mut c = terminal_island_fixture();
    let words = &mut c.methods[0].code.as_mut().unwrap().instructions;
    words[3] = 12;
    words.splice(4..4, [0x0238, 10]);
    assert!(proof::prove(&c, &c.methods[0], 0, 10).is_none());
    let mut c = terminal_island_fixture();
    let words = &mut c.methods[0].code.as_mut().unwrap().instructions;
    words[14] = 0x0107;
    words[15] = 0;
    assert!(proof::prove(&c, &c.methods[0], 0, 8).is_none());
}

fn terminal_fault_fixture() -> DexClass {
    let mut c = terminal_island_fixture();
    Arc::get_mut(&mut c.symbols).unwrap().types[2] = "Lsample/Problem;".into();
    let problem = DexClass {
        symbols: c.symbols.clone(),
        descriptor: "Lsample/Problem;".into(),
        superclass: Some("Ljava/lang/RuntimeException;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![DexMethod {
            declaring_type: "Lsample/Problem;".into(),
            name: "<init>".into(),
            return_type: "V".into(),
            parameters: vec!["Ljava/lang/String;".into()],
            thrown_types: vec![],
            access_flags: 1,
            code: None,
        }],
    };
    c.symbols
        .hierarchy
        .set(Arc::new(
            rdx::native_hierarchy::TypeHierarchy::from_classes([&c, &problem]).unwrap(),
        ))
        .unwrap();
    c
}
#[test]
#[ignore = "requires Java 25 via RDX_JAVA25_HOME"]
fn terminal_island_java_paths_and_faults_match_original_allocation_order() {
    let home = std::env::var("RDX_JAVA25_HOME").expect("RDX_JAVA25_HOME");
    let dir = std::env::temp_dir().join(format!("rdx-terminal-island-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let c = terminal_fault_fixture();
    let rendered = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
    fs::write(
        dir.join("sample/Test.java"),
        format!("package sample;public class Test {{{}}}", rendered.source),
    )
    .unwrap();
    let mut overwrite = terminal_fault_fixture();
    let words = &mut overwrite.methods[0].code.as_mut().unwrap().instructions;
    words[14] = 0x001a;
    words[18] = 0x0002;
    let overwrite =
        native_java::render_method("sample.Overwrite", &overwrite, &overwrite.methods[0]).unwrap();
    fs::write(
        dir.join("sample/Overwrite.java"),
        format!(
            "package sample;public class Overwrite {{{}}}",
            overwrite.source
        ),
    )
    .unwrap();
    let mut protected = terminal_fault_fixture();
    let symbols = Arc::get_mut(&mut protected.symbols).unwrap();
    symbols.strings.push("handled".into());
    symbols
        .protos
        .push(("V".into(), vec!["Ljava/lang/Throwable;".into()]));
    symbols.methods.push((1, 3, 3));
    let code = protected.methods[0].code.as_mut().unwrap();
    code.tries = 1;
    code.try_regions = vec![DexTryRegion {
        start: 0,
        end: 20,
        catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 20)].into(),
    }];
    code.instructions.extend([0x010d, 0x1071, 3, 1, 0x0127]);
    let protected =
        native_java::render_method("sample.Protected", &protected, &protected.methods[0]).unwrap();
    fs::write(
        dir.join("sample/Protected.java"),
        format!(
            "package sample;public class Protected {{{}}}",
            protected.source
        ),
    )
    .unwrap();
    fs::write(dir.join("sample/ReferenceProtected.java"),r#"package sample;public class ReferenceProtected {public static A make(boolean branch){try{return new A(switch(0){default->{if(!branch)throw new Problem("bad");yield Source.value();}});}catch(RuntimeException e){Source.handled(e);throw e;}}}"#).unwrap();
    fs::write(dir.join("sample/Reference.java"),r#"package sample;public class Reference {public static A make(boolean branch){return new A(switch(0){default->{if(!branch)throw new Problem("bad");yield Source.value();}});}}"#).unwrap();
    fs::write(dir.join("sample/State.java"),r#"package sample;public class State {public static int mask;public static String events="";public static Throwable problem;public static void hit(String event,int bit){events+=event;if((mask&bit)!=0)throw new RuntimeException(event);}}"#).unwrap();
    fs::write(dir.join("sample/Source.java"),r#"package sample;public class Source {static{State.hit("S",2);}public static int value(){State.hit("V",4);return 7;}public static void handled(Throwable e){State.hit("H",0);}}"#).unwrap();
    fs::write(dir.join("sample/A.java"),r#"package sample;public class A {static{State.hit("A",1);}public int value;public A(int value){State.hit("C",32);this.value=value;}}"#).unwrap();
    fs::write(dir.join("sample/Problem.java"),r#"package sample;public class Problem extends RuntimeException {static{State.hit("P",8);}public Problem(String text){super(text);State.problem=this;State.hit("Q",16);}}"#).unwrap();
    fs::write(dir.join("Runner.java"),r#"import java.net.*;import java.nio.file.*;import java.lang.reflect.*;public class Runner {static String call(String type,int mask,boolean branch)throws Exception{try(URLClassLoader l=new URLClassLoader(new URL[]{Path.of(System.getProperty("classes")).toUri().toURL()},null)){Class<?> s=Class.forName("sample.State",true,l);s.getField("mask").setInt(null,mask);String result;try{Object a=Class.forName("sample."+type,true,l).getMethod("make",boolean.class).invoke(null,branch);result="value="+a.getClass().getField("value").get(a);}catch(InvocationTargetException e){Throwable t=e.getCause();result=t.getClass().getName()+":"+(t==s.getField("problem").get(null));}return s.getField("events").get(null)+":"+result;}}public static void main(String[]args)throws Exception{for(int mask=0;mask<64;mask++)for(boolean branch:new boolean[]{false,true}){for(String type:new String[]{"Test","Overwrite","Protected"}){String a=call(type,mask,branch),b=call(type.equals("Protected")?"ReferenceProtected":"Reference",mask,branch);if(!a.equals(b))throw new AssertionError(mask+"/"+branch+" "+a+" != "+b);}}System.out.print("ok:384");}}"#).unwrap();
    let javac = Command::new(format!("{home}/bin/javac"))
        .arg("-d")
        .arg(&dir)
        .args(
            [
                "Test",
                "Overwrite",
                "Reference",
                "State",
                "Source",
                "A",
                "Problem",
            ]
            .map(|name| dir.join(format!("sample/{name}.java"))),
        )
        .arg(dir.join("Runner.java"))
        .arg(dir.join("sample/Protected.java"))
        .arg(dir.join("sample/ReferenceProtected.java"))
        .output()
        .unwrap();
    assert!(
        javac.status.success(),
        "{}\n{}",
        rendered.source,
        String::from_utf8_lossy(&javac.stderr)
    );
    let java = Command::new(format!("{home}/bin/java"))
        .arg(format!("-Dclasses={}", dir.display()))
        .arg("-cp")
        .arg(&dir)
        .arg("Runner")
        .output()
        .unwrap();
    assert!(
        java.status.success(),
        "{}",
        String::from_utf8_lossy(&java.stderr)
    );
    assert_eq!(java.stdout, b"ok:384");
    let javap = Command::new(format!("{home}/bin/javap"))
        .arg("-c")
        .arg("-classpath")
        .arg(&dir)
        .arg("sample.Test")
        .output()
        .unwrap();
    assert!(javap.status.success());
    let bytecode = String::from_utf8(javap.stdout).unwrap();
    assert!(
        bytecode.find("class sample/A").unwrap() < bytecode.find("class sample/Problem").unwrap(),
        "{bytecode}"
    );
    assert!(
        bytecode.find("class sample/A").unwrap() < bytecode.find("sample/Source.value").unwrap(),
        "{bytecode}"
    );
    assert!(
        bytecode.find("class sample/Problem").unwrap() < bytecode.find("String bad").unwrap(),
        "{bytecode}"
    );
    fs::remove_dir_all(dir).unwrap();
}

fn handler_gap_fixture() -> DexClass {
    let mut c = fixture();
    let sy = Arc::get_mut(&mut c.symbols).unwrap();
    sy.strings.push("handled".into());
    sy.protos
        .push(("V".into(), vec!["Ljava/lang/Throwable;".into()]));
    sy.methods.push((1, 2, 2));
    let code = c.methods[0].code.as_mut().unwrap();
    code.tries = 1;
    code.try_regions = vec![DexTryRegion {
        start: 0,
        end: 12,
        catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 7)].into(),
    }];
    code.instructions = vec![
        0x0022, 0, 0x0071, 0, 0, 0x010a, 0x0328, 0x020d, 0x0528, 0x2070, 1, 0x0010, 0x0011, 0x1071,
        2, 2, 0x0227,
    ];
    c
}
#[test]
fn normal_cfg_handler_gap_is_owned_by_caller() {
    let c = handler_gap_fixture();
    let p = proof::prove(&c, &c.methods[0], 0, 9).unwrap();
    assert!(!p.owned.contains(&7));
    assert!(!p.owned.contains(&8));
    assert!(proof::validate(&c, &c.methods[0], &p));
    let front = native_method::MethodFrontEnd::build(&c, &c.methods[0]).unwrap();
    let a = native_method::MethodAnalysis::build(&c, &c.methods[0]).unwrap();
    let cs = a.constructors().unwrap();
    let cfg = native_cfg::ControlFlowGraph::build(c.methods[0].code.as_ref().unwrap()).unwrap();
    assert_eq!(
        Some(p),
        proof::prove_cached(
            &c,
            &c.methods[0],
            0,
            9,
            proof::ProofInputs {
                ir: &front.ir,
                cfg: &cfg,
                bound: &front.bound,
                bindings: &cs.bindings
            }
        )
    );
    let r = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
    for (label, target) in [
        ("sample.A.<init>(I)V", "sample.A"),
        ("sample.Source.value()I", "value"),
        ("sample.Source.handled(Ljava/lang/Throwable;)V", "handled"),
    ] {
        assert!(
            r.links.iter().any(|link| link.label == label
                && r.source
                    .chars()
                    .skip(link.start)
                    .take(link.end - link.start)
                    .collect::<String>()
                    == target),
            "missing {label}: {:?}",
            r.links
        );
    }
    assert!(r.source.contains("new sample.A"));
    assert!(r.source.contains("catch (java.lang.RuntimeException"));
    assert!(r.source.contains("sample.Source.handled"));
}
#[test]
fn normal_cfg_handler_gap_rejects_successful_path_and_foreign_entry() {
    let mut malformed = handler_gap_fixture();
    malformed.methods[0].code.as_mut().unwrap().instructions[6] = 0x0228;
    assert!(proof::prove(&malformed, &malformed.methods[0], 0, 9).is_none());
    let mut payload = handler_gap_fixture();
    payload.methods[0].code.as_mut().unwrap().instructions[6] = 0x0300;
    assert!(proof::prove(&payload, &payload.methods[0], 0, 9).is_none());

    let mut leak = handler_gap_fixture();
    leak.methods[0].code.as_mut().unwrap().instructions[15] = 0;
    assert!(native_java::render_method("sample.Test", &leak, &leak.methods[0]).is_err());
    let mut c = handler_gap_fixture();
    c.methods[0].code.as_mut().unwrap().instructions[6] = 0x0128;
    assert!(proof::prove(&c, &c.methods[0], 0, 9).is_none());
    let mut c = handler_gap_fixture();
    c.methods[0].code.as_mut().unwrap().instructions[8] = 0x0128;
    assert!(proof::prove(&c, &c.methods[0], 0, 9).is_none());
    let mut c = handler_gap_fixture();
    c.methods[0].code.as_mut().unwrap().try_regions[0].end = 9;
    assert!(proof::prove(&c, &c.methods[0], 0, 9).is_none());
}
#[test]
#[ignore = "requires Java 25"]
fn normal_cfg_handler_gap_java_fault_identity_and_order() {
    let home = std::env::var("RDX_JAVA25_HOME").unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-handler-gap-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let rendered = native_java::render_method(
        "sample.Test",
        &handler_gap_fixture(),
        &handler_gap_fixture().methods[0],
    )
    .unwrap();
    fs::write(
        dir.join("sample/Test.java"),
        format!("package sample;public class Test {{{}}}", rendered.source),
    )
    .unwrap();
    fs::write(dir.join("sample/Reference.java"),"package sample;public class Reference {public static A make(boolean b){try{return new A(Source.value());}catch(RuntimeException e){Source.handled(e);throw e;}}}").unwrap();
    fs::write(dir.join("sample/State.java"),"package sample;public class State {public static int mask;public static String events=\"\";public static Throwable fault;public static void hit(String e,int bit){events+=e;if((mask&bit)!=0){RuntimeException x=new RuntimeException(e);fault=x;throw x;}}}").unwrap();
    fs::write(dir.join("sample/A.java"),"package sample;public class A {static{State.hit(\"A\",1);}public int value;public A(int v){State.hit(\"C\",8);value=v;}}").unwrap();
    fs::write(dir.join("sample/Source.java"),"package sample;public class Source {static{State.hit(\"S\",2);}public static int value(){State.hit(\"V\",4);return 7;}public static void handled(Throwable t){State.events+=\"H\"+(t==State.fault);}}").unwrap();
    fs::write(dir.join("Runner.java"),r#"import java.net.*;import java.nio.file.*;import java.lang.reflect.*;public class Runner{static String call(String name,int m,boolean b)throws Exception{try(URLClassLoader l=new URLClassLoader(new URL[]{Path.of(System.getProperty("classes")).toUri().toURL()},null)){Class<?>s=Class.forName("sample.State",true,l);s.getField("mask").setInt(null,m);String r;try{Object v=Class.forName("sample."+name,true,l).getMethod("make",boolean.class).invoke(null,b);r="value="+v.getClass().getField("value").get(v);}catch(InvocationTargetException x){Throwable t=x.getCause();r=t.getClass().getName()+":"+(t==s.getField("fault").get(null));}return s.getField("events").get(null)+":"+r;}}public static void main(String[]a)throws Exception{for(int m=0;m<16;m++)for(boolean b:new boolean[]{false,true}){String x=call("Test",m,b),y=call("Reference",m,b);if(!x.equals(y))throw new AssertionError(m+" "+x+" != "+y);}System.out.print("ok:32");}}"#).unwrap();
    let paths =
        ["Test", "Reference", "State", "A", "Source"].map(|n| dir.join(format!("sample/{n}.java")));
    let c = Command::new(format!("{home}/bin/javac"))
        .arg("-d")
        .arg(&dir)
        .args(paths)
        .arg(dir.join("Runner.java"))
        .output()
        .unwrap();
    assert!(c.status.success(), "{}", String::from_utf8_lossy(&c.stderr));
    let c = Command::new(format!("{home}/bin/java"))
        .arg(format!("-Dclasses={}", dir.display()))
        .arg("-cp")
        .arg(&dir)
        .arg("Runner")
        .output()
        .unwrap();
    assert!(c.status.success(), "{}", String::from_utf8_lossy(&c.stderr));
    assert_eq!(c.stdout, b"ok:32");
    fs::remove_dir_all(dir).unwrap();
}

fn branched_alias_fixture() -> DexClass {
    let mut c = fixture();
    let code = c.methods[0].code.as_mut().unwrap();
    code.registers = 5;
    code.instructions = vec![
        0x0022, 0, 0x0307, 0x0438, 7, 0x0071, 0, 0, 0x010a, 0x0228, 0x1112, 0x2070, 1, 0x0013,
        0x0311,
    ];
    c
}
#[test]
fn branched_aliases_require_original_identity_on_every_path() {
    let c = branched_alias_fixture();
    let p = proof::prove(&c, &c.methods[0], 0, 11).unwrap();
    assert_eq!(p.alias_moves, vec![2]);
    assert_eq!(p.constructor_aliases, vec![0, 3]);
    let r = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
    assert!(r.source.contains("new sample.A"));
    let mut c = branched_alias_fixture();
    let w = &mut c.methods[0].code.as_mut().unwrap().instructions;
    w.insert(3, 0x0012);
    assert!(proof::prove(&c, &c.methods[0], 0, 12).is_some());
    assert!(native_java::render_method("sample.Test", &c, &c.methods[0]).is_ok());
    let mut c = branched_alias_fixture();
    c.methods[0].code.as_mut().unwrap().instructions[10] = 0x0312;
    assert!(proof::prove(&c, &c.methods[0], 0, 11).is_none());
    let mut c = branched_alias_fixture();
    c.methods[0].code.as_mut().unwrap().instructions[3] = 0x0338;
    assert!(proof::prove(&c, &c.methods[0], 0, 11).is_none());
    let mut c = loop_fixture();
    let w = &mut c.methods[0].code.as_mut().unwrap().instructions;
    w.insert(2, 0x0407);
    w[19] = 0x0014;
    w[20] = 0x0411;
    let p = proof::prove(&c, &c.methods[0], 0, 17).unwrap();
    assert_eq!(p.alias_moves, vec![2]);
    assert_eq!(p.constructor_aliases, vec![0, 4]);
    native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
    let mut c = branched_alias_fixture();
    c.methods[0].code.as_mut().unwrap().instructions[10] = 0x3207;
    assert!(proof::prove(&c, &c.methods[0], 0, 11).is_none());
}
#[test]
#[ignore = "requires Java 25"]
fn branched_alias_java_fault_identity_and_order() {
    let home = std::env::var("RDX_JAVA25_HOME").unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-branch-alias-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let rendered = native_java::render_method(
        "sample.Test",
        &branched_alias_fixture(),
        &branched_alias_fixture().methods[0],
    )
    .unwrap();
    fs::write(
        dir.join("sample/Test.java"),
        format!("package sample;public class Test {{{}}}", rendered.source),
    )
    .unwrap();
    fs::write(dir.join("sample/Reference.java"),"package sample;public class Reference {public static A make(boolean b){return new A(b?Source.value():1);}}").unwrap();
    fs::write(dir.join("sample/State.java"),"package sample;public class State {public static int mask;public static String events=\"\";public static Throwable fault;public static void hit(String e,int bit){events+=e;if((mask&bit)!=0){RuntimeException x=new RuntimeException(e);fault=x;throw x;}}}").unwrap();
    fs::write(dir.join("sample/A.java"),"package sample;public class A {static{State.hit(\"A\",1);}public int value;public A(int v){State.hit(\"C\",8);value=v;}}").unwrap();
    fs::write(dir.join("sample/Source.java"),"package sample;public class Source {static{State.hit(\"S\",2);}public static int value(){State.hit(\"V\",4);return 7;}public static void handled(Throwable t){State.events+=\"H\"+(t==State.fault);}}").unwrap();
    fs::write(dir.join("Runner.java"),r#"import java.net.*;import java.nio.file.*;import java.lang.reflect.*;public class Runner{static String call(String name,int m,boolean b)throws Exception{try(URLClassLoader l=new URLClassLoader(new URL[]{Path.of(System.getProperty("classes")).toUri().toURL()},null)){Class<?>s=Class.forName("sample.State",true,l);s.getField("mask").setInt(null,m);String r;try{Object v=Class.forName("sample."+name,true,l).getMethod("make",boolean.class).invoke(null,b);r="value="+v.getClass().getField("value").get(v);}catch(InvocationTargetException x){Throwable t=x.getCause();r=t.getClass().getName()+":"+(t==s.getField("fault").get(null));}return s.getField("events").get(null)+":"+r;}}public static void main(String[]a)throws Exception{for(int m=0;m<16;m++)for(boolean b:new boolean[]{false,true}){String x=call("Test",m,b),y=call("Reference",m,b);if(!x.equals(y))throw new AssertionError(m+" "+x+" != "+y);}System.out.print("ok:32");}}"#).unwrap();
    let paths =
        ["Test", "Reference", "State", "A", "Source"].map(|n| dir.join(format!("sample/{n}.java")));
    let c = Command::new(format!("{home}/bin/javac"))
        .arg("-d")
        .arg(&dir)
        .args(paths)
        .arg(dir.join("Runner.java"))
        .output()
        .unwrap();
    assert!(c.status.success(), "{}", String::from_utf8_lossy(&c.stderr));
    let c = Command::new(format!("{home}/bin/java"))
        .arg(format!("-Dclasses={}", dir.display()))
        .arg("-cp")
        .arg(&dir)
        .arg("Runner")
        .output()
        .unwrap();
    assert!(c.status.success(), "{}", String::from_utf8_lossy(&c.stderr));
    assert_eq!(c.stdout, b"ok:32");
    fs::remove_dir_all(dir).unwrap();
}
#[test]
fn alias_fixed_point_keeps_raw_cached_handler_and_zero_iteration_boundaries() {
    let c = branched_alias_fixture();
    let p = proof::prove(&c, &c.methods[0], 0, 11).unwrap();
    let front = native_method::MethodFrontEnd::build(&c, &c.methods[0]).unwrap();
    let a = native_method::MethodAnalysis::build(&c, &c.methods[0]).unwrap();
    let bindings = a.constructors().unwrap();
    let cfg = native_cfg::ControlFlowGraph::build(c.methods[0].code.as_ref().unwrap()).unwrap();
    assert_eq!(
        Some(p.clone()),
        proof::prove_cached(
            &c,
            &c.methods[0],
            0,
            11,
            proof::ProofInputs {
                ir: &front.ir,
                cfg: &cfg,
                bound: &front.bound,
                bindings: &bindings.bindings
            }
        )
    );
    let mut poison = p;
    poison.alias_moves.clear();
    assert!(!proof::validate(&c, &c.methods[0], &poison));
    let mut c = loop_fixture();
    let w = &mut c.methods[0].code.as_mut().unwrap().instructions;
    w[11] = 0x0407;
    w[12] = 0;
    w[18] = 0x0014;
    assert!(proof::prove(&c, &c.methods[0], 0, 16).is_none());
    let mut c = branched_alias_fixture();
    let sy = Arc::get_mut(&mut c.symbols).unwrap();
    sy.strings.push("handled".into());
    sy.protos
        .push(("V".into(), vec!["Ljava/lang/Throwable;".into()]));
    sy.methods.push((1, 2, 2));
    let code = c.methods[0].code.as_mut().unwrap();
    code.tries = 1;
    code.try_regions = vec![DexTryRegion {
        start: 0,
        end: 14,
        catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 15)].into(),
    }];
    code.instructions.extend([0x010d, 0x1071, 2, 1, 0x0127]);
    assert!(proof::prove(&c, &c.methods[0], 0, 11).is_some());
    assert!(native_java::render_method("sample.Test", &c, &c.methods[0]).is_ok());
    c.methods[0].code.as_mut().unwrap().instructions[18] = 3;
    assert!(native_java::render_method("sample.Test", &c, &c.methods[0]).is_err());
}
#[test]
#[ignore = "requires Java 25"]
fn loop_alias_java_fault_identity_and_order() {
    let home = std::env::var("RDX_JAVA25_HOME").unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-loop-alias-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let mut c = loop_fixture();
    let w = &mut c.methods[0].code.as_mut().unwrap().instructions;
    w.insert(2, 0x0407);
    w[19] = 0x0014;
    w[20] = 0x0411;
    let rendered = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
    fs::write(
        dir.join("sample/Test.java"),
        format!("package sample;public class Test {{{}}}", rendered.source),
    )
    .unwrap();
    fs::write(dir.join("sample/Reference.java"),"package sample;public class Reference {public static A make(int n){return new A(sum(n));}static int sum(int n){int result=0;for(int i=0;i<n;i++)result+=Source.value();return result;}}").unwrap();
    fs::write(dir.join("sample/State.java"),"package sample;public class State {public static int mask;public static String events=\"\";public static Throwable fault;public static void hit(String e,int bit){events+=e;if((mask&bit)!=0){RuntimeException x=new RuntimeException(e);fault=x;throw x;}}}").unwrap();
    fs::write(dir.join("sample/A.java"),"package sample;public class A {static{State.hit(\"A\",1);}public int value;public A(int v){State.hit(\"C\",8);value=v;}}").unwrap();
    fs::write(dir.join("sample/Source.java"),"package sample;public class Source {static{State.hit(\"S\",2);}public static int value(){State.hit(\"V\",4);return 7;}public static void handled(Throwable t){State.events+=\"H\"+(t==State.fault);}}").unwrap();
    fs::write(dir.join("Runner.java"),r#"import java.net.*;import java.nio.file.*;import java.lang.reflect.*;public class Runner{static String call(String name,int m,boolean b)throws Exception{try(URLClassLoader l=new URLClassLoader(new URL[]{Path.of(System.getProperty("classes")).toUri().toURL()},null)){Class<?>s=Class.forName("sample.State",true,l);s.getField("mask").setInt(null,m);String r;try{Object v=Class.forName("sample."+name,true,l).getMethod("make",int.class).invoke(null,b?2:0);r="value="+v.getClass().getField("value").get(v);}catch(InvocationTargetException x){Throwable t=x.getCause();r=t.getClass().getName()+":"+(t==s.getField("fault").get(null));}return s.getField("events").get(null)+":"+r;}}public static void main(String[]a)throws Exception{for(int m=0;m<16;m++)for(boolean b:new boolean[]{false,true}){String x=call("Test",m,b),y=call("Reference",m,b);if(!x.equals(y))throw new AssertionError(m+" "+x+" != "+y);}System.out.print("ok:32");}}"#).unwrap();
    let paths =
        ["Test", "Reference", "State", "A", "Source"].map(|n| dir.join(format!("sample/{n}.java")));
    let c = Command::new(format!("{home}/bin/javac"))
        .arg("-d")
        .arg(&dir)
        .args(paths)
        .arg(dir.join("Runner.java"))
        .output()
        .unwrap();
    assert!(c.status.success(), "{}", String::from_utf8_lossy(&c.stderr));
    let c = Command::new(format!("{home}/bin/java"))
        .arg(format!("-Dclasses={}", dir.display()))
        .arg("-cp")
        .arg(&dir)
        .arg("Runner")
        .output()
        .unwrap();
    assert!(c.status.success(), "{}", String::from_utf8_lossy(&c.stderr));
    assert_eq!(c.stdout, b"ok:32");
    fs::remove_dir_all(dir).unwrap();
}
fn loop_counter_then_alias_fixture() -> DexClass {
    let mut c = loop_fixture();
    let w = &mut c.methods[0].code.as_mut().unwrap().instructions;
    w[3] = 0x0412;
    w[5] = 0x5435;
    w[13] = 0x04d8;
    w[14] = 0x0104;
    w.insert(16, 0x0407);
    w[19] = 0x0014;
    w[20] = 0x0411;
    c
}
#[test]
fn final_receiver_alias_retains_earlier_ordinary_counter_lifetime() {
    let c = loop_counter_then_alias_fixture();
    let p = proof::prove(&c, &c.methods[0], 0, 17).unwrap();
    assert_eq!(p.alias_moves, vec![16]);
    assert!(
        p.alias_masks
            .iter()
            .all(|&(from, _, r)| r != 4 || from >= 17)
    );
    let r = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
    assert!(r.source.contains("while"));
}
#[test]
#[ignore = "requires Java 25"]
fn loop_counter_then_alias_java_fault_identity_and_order() {
    let home = std::env::var("RDX_JAVA25_HOME").unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-loop-counter-alias-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let c = loop_counter_then_alias_fixture();
    let rendered = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
    fs::write(
        dir.join("sample/Test.java"),
        format!("package sample;public class Test {{{}}}", rendered.source),
    )
    .unwrap();
    fs::write(dir.join("sample/Reference.java"),"package sample;public class Reference {public static A make(int n){return new A(sum(n));}static int sum(int n){int result=0;for(int i=0;i<n;i++)result+=Source.value();return result;}}").unwrap();
    fs::write(dir.join("sample/State.java"),"package sample;public class State {public static int mask;public static String events=\"\";public static Throwable fault;public static void hit(String e,int bit){events+=e;if((mask&bit)!=0){RuntimeException x=new RuntimeException(e);fault=x;throw x;}}}").unwrap();
    fs::write(dir.join("sample/A.java"),"package sample;public class A {static{State.hit(\"A\",1);}public int value;public A(int v){State.hit(\"C\",8);value=v;}}").unwrap();
    fs::write(dir.join("sample/Source.java"),"package sample;public class Source {static{State.hit(\"S\",2);}public static int value(){State.hit(\"V\",4);return 7;}public static void handled(Throwable t){State.events+=\"H\"+(t==State.fault);}}").unwrap();
    fs::write(dir.join("Runner.java"),r#"import java.net.*;import java.nio.file.*;import java.lang.reflect.*;public class Runner{static String call(String name,int m,boolean b)throws Exception{try(URLClassLoader l=new URLClassLoader(new URL[]{Path.of(System.getProperty("classes")).toUri().toURL()},null)){Class<?>s=Class.forName("sample.State",true,l);s.getField("mask").setInt(null,m);String r;try{Object v=Class.forName("sample."+name,true,l).getMethod("make",int.class).invoke(null,b?2:0);r="value="+v.getClass().getField("value").get(v);}catch(InvocationTargetException x){Throwable t=x.getCause();r=t.getClass().getName()+":"+(t==s.getField("fault").get(null));}return s.getField("events").get(null)+":"+r;}}public static void main(String[]a)throws Exception{for(int m=0;m<16;m++)for(boolean b:new boolean[]{false,true}){String x=call("Test",m,b),y=call("Reference",m,b);if(!x.equals(y))throw new AssertionError(m+" "+x+" != "+y);}System.out.print("ok:32");}}"#).unwrap();
    let paths =
        ["Test", "Reference", "State", "A", "Source"].map(|n| dir.join(format!("sample/{n}.java")));
    let c = Command::new(format!("{home}/bin/javac"))
        .arg("-d")
        .arg(&dir)
        .args(paths)
        .arg(dir.join("Runner.java"))
        .output()
        .unwrap();
    assert!(c.status.success(), "{}", String::from_utf8_lossy(&c.stderr));
    let c = Command::new(format!("{home}/bin/java"))
        .arg(format!("-Dclasses={}", dir.display()))
        .arg("-cp")
        .arg(&dir)
        .arg("Runner")
        .output()
        .unwrap();
    assert!(c.status.success(), "{}", String::from_utf8_lossy(&c.stderr));
    assert_eq!(c.stdout, b"ok:32");
    fs::remove_dir_all(dir).unwrap();
}

fn terminal_comparison_fixture(op: u8) -> DexClass {
    let mut c = terminal_fault_fixture();
    let s = Arc::get_mut(&mut c.symbols).unwrap();
    s.strings.push("compare".into());
    s.protos.push(("V".into(), vec!["I".into()]));
    s.methods.push((1, 3, 3));
    let ty = match op {
        0x2d | 0x2e => "F",
        0x2f | 0x30 => "D",
        0x31 => "J",
        _ => panic!("comparison opcode"),
    };
    c.methods[0].parameters = vec!["Z".into(), ty.into(), ty.into()];
    let code = c.methods[0].code.as_mut().unwrap();
    code.registers = if ty == "F" { 6 } else { 8 };
    code.ins = code.registers - 3;
    code.instructions = vec![
        0x0022,
        0,
        0x0338,
        10,
        0x0071,
        0,
        0,
        0x010a,
        0x2070,
        1,
        0x0010,
        0x0011,
        0x0222,
        2,
        0x0100 | op as u16,
        if ty == "F" { 0x0504 } else { 0x0604 },
        0x1071,
        3,
        1,
        0x011a,
        2,
        0x2070,
        2,
        0x0012,
        0x0227,
    ];
    c
}
#[test]
fn terminal_comparisons_preserve_raw_wide_reads_and_control_refusals() {
    for op in 0x2d..=0x31 {
        let c = terminal_comparison_fixture(op);
        let plan = proof::prove(&c, &c.methods[0], 0, 8).unwrap();
        assert_eq!(plan.terminal_islands, vec![(12, 25)]);
        let rendered = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
        assert!(
            rendered.source.contains("sample.Source.compare("),
            "{}",
            rendered.source
        );
        for target in [
            "sample.A.<init>(I)V",
            "sample.Source.compare(I)V",
            "sample.Problem.<init>(Ljava/lang/String;)V",
        ] {
            assert!(
                rendered.links.iter().any(|link| link.label == target),
                "{target}: {:?}",
                rendered.links
            );
        }
        let mut bad = terminal_comparison_fixture(op);
        bad.methods[0].code.as_mut().unwrap().instructions[15] = 0xff04;
        assert!(proof::prove(&bad, &bad.methods[0], 0, 8).is_none());
        for transfer in [0x0228, 0x0138] {
            let mut bad = terminal_comparison_fixture(op);
            let words = &mut bad.methods[0].code.as_mut().unwrap().instructions;
            words[14] = transfer;
            words[15] = 2;
            assert!(proof::prove(&bad, &bad.methods[0], 0, 8).is_none());
        }
    }
}
#[test]
#[ignore = "requires Java 25 via RDX_JAVA25_HOME"]
fn terminal_comparison_java_nan_wide_faults_and_identity() {
    let home = std::env::var("RDX_JAVA25_HOME").expect("RDX_JAVA25_HOME");
    let dir = std::env::temp_dir().join(format!("rdx-terminal-comparison-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    for op in 0x2d..=0x31 {
        let c = terminal_comparison_fixture(op);
        let rendered = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
        fs::write(
            dir.join(format!("sample/Test{op}.java")),
            format!(
                "package sample;public class Test{op} {{{}}}",
                rendered.source
            ),
        )
        .unwrap();
        let ty = match op {
            0x2d | 0x2e => "float",
            0x2f | 0x30 => "double",
            _ => "long",
        };
        let compare = if op == 0x31 {
            "(left>right?1:left==right?0:-1)".to_owned()
        } else {
            let wrapper = if ty == "float" { "Float" } else { "Double" };
            let nan = if op == 0x2d || op == 0x2f { -1 } else { 1 };
            format!(
                "({wrapper}.isNaN(left)||{wrapper}.isNaN(right)?{nan}:left>right?1:left==right?0:-1)"
            )
        };
        fs::write(dir.join(format!("sample/Reference{op}.java")),format!("package sample;public class Reference{op} {{public static A make(boolean branch,{ty} left,{ty} right) {{return new A(switch(0){{default->{{if(!branch)throw new Problem(switch(0){{default->{{Source.compare({compare});yield \"bad\";}}}});yield Source.value();}}}});}}}}" )).unwrap();
    }
    fs::write(dir.join("sample/State.java"),"package sample;public class State {public static int mask;public static String events=\"\";public static Throwable problem;public static void hit(String event,int bit){events+=event;if((mask&bit)!=0)throw new RuntimeException(event);}}").unwrap();
    fs::write(dir.join("sample/Source.java"),"package sample;public class Source {static{State.hit(\"S\",2);}public static int value(){State.hit(\"V\",4);return 7;}public static void compare(int value){State.hit(\"X\"+value,4);}}").unwrap();
    fs::write(dir.join("sample/A.java"),"package sample;public class A {static{State.hit(\"A\",1);}public int value;public A(int value){State.hit(\"C\",32);this.value=value;}}").unwrap();
    fs::write(dir.join("sample/Problem.java"),"package sample;public class Problem extends RuntimeException {static{State.hit(\"P\",8);}public Problem(String text){super(text);State.problem=this;State.hit(\"Q\",16);}}").unwrap();
    fs::write(dir.join("Runner.java"),r#"import java.net.*;import java.nio.file.*;import java.lang.reflect.*;public class Runner {
static String call(String type,int op,int mask,boolean branch,int pair)throws Exception{try(URLClassLoader l=new URLClassLoader(new URL[]{Path.of(System.getProperty("classes")).toUri().toURL()},null)){Class<?> s=Class.forName("sample.State",true,l);s.getField("mask").setInt(null,mask);Class<?> p;Object left,right;if(op==49){p=long.class;long[][] values={{Long.MIN_VALUE,Long.MAX_VALUE},{Long.MAX_VALUE,Long.MIN_VALUE},{0,0},{-1,0},{0,-1},{Long.MIN_VALUE,Long.MIN_VALUE}};left=values[pair][0];right=values[pair][1];}else if(op<=46){p=float.class;float[][] values={{Float.NaN,1},{1,Float.NaN},{-0.0f,0.0f},{Float.NEGATIVE_INFINITY,Float.POSITIVE_INFINITY},{2,1},{1,2}};left=values[pair][0];right=values[pair][1];}else{p=double.class;double[][] values={{Double.NaN,1},{1,Double.NaN},{-0.0,0.0},{Double.NEGATIVE_INFINITY,Double.POSITIVE_INFINITY},{2,1},{1,2}};left=values[pair][0];right=values[pair][1];}String result;try{Object a=Class.forName("sample."+type,true,l).getMethod("make",boolean.class,p,p).invoke(null,branch,left,right);result="value="+a.getClass().getField("value").get(a);}catch(InvocationTargetException e){Throwable t=e.getCause();result=t.getClass().getName()+":"+(t==s.getField("problem").get(null));}return s.getField("events").get(null)+":"+result;}}
public static void main(String[]args)throws Exception{int count=0;for(int op=45;op<=49;op++)for(int pair=0;pair<6;pair++)for(int mask:new int[]{0,1,2,4,8,16,32})for(boolean branch:new boolean[]{false,true}){String a=call("Test"+op,op,mask,branch,pair),b=call("Reference"+op,op,mask,branch,pair);if(!a.equals(b))throw new AssertionError(op+"/"+pair+"/"+mask+"/"+branch+" "+a+" != "+b);count++;}System.out.print("ok:"+count);}}"#).unwrap();
    let mut javac = Command::new(format!("{home}/bin/javac"));
    javac.arg("-d").arg(&dir);
    for entry in fs::read_dir(dir.join("sample")).unwrap() {
        javac.arg(entry.unwrap().path());
    }
    javac.arg(dir.join("Runner.java"));
    let result = javac.output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let result = Command::new(format!("{home}/bin/java"))
        .arg(format!("-Dclasses={}", dir.display()))
        .arg("-cp")
        .arg(&dir)
        .arg("Runner")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, b"ok:420");
    fs::remove_dir_all(dir).unwrap();
}

fn projection_nested_fixture(wide_move: bool) -> DexClass {
    let mut c = nested_capture_fixture();
    let s = Arc::get_mut(&mut c.symbols).unwrap();
    s.types.push("Lsample/C;".into());
    s.methods.push((3, 1, 1));
    let code = c.methods[0].code.as_mut().unwrap();
    code.registers = 7;
    code.outs = 2;
    let mut words = vec![0x0022, 0, 0x0122, 2, 0x0222, 3];
    if wide_move {
        words.extend([0x0009, 5, 0]);
    } else {
        words.extend([0x0508, 0]);
    }
    words.extend([0x0009, 6, 1]);
    words.extend([
        0x0071, 0, 0, 0x030a, 0x2070, 3, 0x0032, 0x2070, 2, 0x0036, 0x2070, 1, 0x0035, 0x0511,
    ]);
    code.instructions = words;
    c
}
#[test]
fn canonical_projection_retains_multiword_boundaries_through_two_parents() {
    for wide in [false, true] {
        let mut c = projection_nested_fixture(wide);
        let ctor = if wide { 22 } else { 21 };
        let outer = proof::prove(&c, &c.methods[0], 0, ctor).unwrap();
        assert_eq!(outer.alias_moves, vec![6]);
        let projection = proof::Projection::extend(&c, &c.methods[0], None, &outer).unwrap();
        c.methods[0].code.as_mut().unwrap().instructions[6..if wide { 9 } else { 8 }].fill(0);
        let ir = projection.checked_ir(&c, &c.methods[0]).unwrap();
        assert!(ir.instructions.iter().any(|i| i.pc == 6
            && i.width == if wide { 3 } else { 2 }
            && i.opcode == 0
            && i.reads.is_empty()
            && i.writes.is_empty()));
        assert!(!ir.instructions.iter().any(|i| i.pc == 7));
        let inner_ctor = ctor - 3;
        let inner = projection.prove(&c, &c.methods[0], 2, inner_ctor).unwrap();
        assert!(inner.owned.iter().all(|pc| outer.owned.contains(pc)));
        let second =
            proof::Projection::extend(&c, &c.methods[0], Some(&projection), &inner).unwrap();
        let pc = if wide { 9 } else { 8 };
        c.methods[0].code.as_mut().unwrap().instructions[pc..pc + 3].fill(0);
        let child = second.prove(&c, &c.methods[0], 4, inner_ctor - 3).unwrap();
        assert!(child.owned.iter().all(|pc| inner.owned.contains(pc)));
        assert!(second.validate(&c, &c.methods[0], &child));
        c.methods[0].code.as_mut().unwrap().instructions[pc + 1] = 1;
        assert!(second.checked_ir(&c, &c.methods[0]).is_none());
        let c = projection_nested_fixture(wide);
        let rendered = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
        for label in [
            "sample.A.<init>(I)V",
            "sample.B.<init>(I)V",
            "sample.C.<init>(I)V",
            "sample.Source.value()I",
        ] {
            assert!(
                rendered.links.iter().any(|link| link.label == label),
                "{label}: {}",
                rendered.source
            );
        }
    }
}
#[test]
fn projection_rejects_poisoned_witness_words_handlers_and_metadata() {
    for wide in [false, true] {
        let mut c = projection_nested_fixture(wide);
        let ctor = if wide { 22 } else { 21 };
        let plan = proof::prove(&c, &c.methods[0], 0, ctor).unwrap();
        for bad_pc in [7, 4, ctor] {
            let mut poisoned = plan.clone();
            poisoned.alias_moves.push(bad_pc);
            assert!(proof::Projection::extend(&c, &c.methods[0], None, &poisoned).is_none());
        }
        let projection = proof::Projection::extend(&c, &c.methods[0], None, &plan).unwrap();
        c.methods[0].code.as_mut().unwrap().instructions[6..if wide { 9 } else { 8 }].fill(0);
        assert!(projection.checked_ir(&c, &c.methods[0]).is_some());
        assert!(
            projection
                .original_code()
                .instructions
                .iter()
                .any(|word| *word == 0x0508 || *word == 0x0009)
        );
        let index = if wide { 12 } else { 11 };
        let old = c.methods[0].code.as_ref().unwrap().instructions[index];
        c.methods[0].code.as_mut().unwrap().instructions[index] = 0;
        assert!(projection.checked_ir(&c, &c.methods[0]).is_none());
        c.methods[0].code.as_mut().unwrap().instructions[index] = old;
        c.methods[0].parameters.push("I".into());
        assert!(projection.checked_ir(&c, &c.methods[0]).is_none());
        c.methods[0].parameters.clear();
        c.methods[0]
            .code
            .as_mut()
            .unwrap()
            .try_regions
            .push(DexTryRegion {
                start: 0,
                end: ctor as u32,
                catches: vec![(None, 0)].into(),
            });
        assert!(projection.checked_ir(&c, &c.methods[0]).is_none());
    }
}
fn projection_loop_lifetime_fixture() -> DexClass {
    let mut c = projection_nested_fixture(false);
    c.methods[0].parameters = vec!["I".into()];
    let code = c.methods[0].code.as_mut().unwrap();
    code.registers = 8;
    code.ins = 1;
    code.instructions = vec![
        0x0022, 0, 0x0122, 2, 0x0222, 3, 0x0512, 0x7312, 0x7535, 9, 0x0071, 0, 0, 0x030a, 0x05d8,
        0x0105, 0xf828, 0x0508, 0, 0x0009, 6, 1, 0x2070, 3, 0x0032, 0x2070, 2, 0x0036, 0x2070, 1,
        0x0035, 0x0511,
    ];
    c
}
fn projection_branch_fixture() -> DexClass {
    let mut c = projection_nested_fixture(false);
    c.methods[0].parameters = vec!["Z".into()];
    let code = c.methods[0].code.as_mut().unwrap();
    code.registers = 8;
    code.ins = 1;
    code.instructions = vec![
        0x0022, 0, 0x0122, 2, 0x0222, 3, 0x7312, 0x0738, 6, 0x0071, 0, 0, 0x030a, 0x0508, 0,
        0x0009, 6, 1, 0x2070, 3, 0x0032, 0x2070, 2, 0x0036, 0x2070, 1, 0x0035, 0x0511,
    ];
    c
}
#[test]
fn projection_keeps_original_counter_lifetime_and_join_boundaries() {
    for c in [
        projection_loop_lifetime_fixture(),
        projection_branch_fixture(),
    ] {
        let rendered = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
        assert!(rendered.source.contains("sample.C"), "{}", rendered.source);
    }
    let mut c = projection_loop_lifetime_fixture();
    let plan = proof::prove(&c, &c.methods[0], 0, 28).unwrap();
    assert!(
        plan.alias_masks
            .iter()
            .any(|&(from, _, register)| register == 5 && from == 19)
    );
    c.methods[0].code.as_mut().unwrap().instructions[7] = 0;
    assert!(native_java::render_method("sample.Test", &c, &c.methods[0]).is_err());
    let mut c = projection_branch_fixture();
    c.methods[0].code.as_mut().unwrap().instructions[8] = 9;
    assert!(native_java::render_method("sample.Test", &c, &c.methods[0]).is_err());
}
#[test]
#[ignore = "requires Java 25 via RDX_JAVA25_HOME"]
fn projection_java_nested_fault_identity_and_loop_lifetimes() {
    let home = std::env::var("RDX_JAVA25_HOME").expect("RDX_JAVA25_HOME");
    let dir = std::env::temp_dir().join(format!("rdx-projection-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    for (name, c) in [
        ("Narrow", projection_nested_fixture(false)),
        ("Wide", projection_nested_fixture(true)),
        ("Loop", projection_loop_lifetime_fixture()),
        ("Branch", projection_branch_fixture()),
        ("Protected", projection_protected_fixture(false)),
        ("Sibling", projection_sibling_fixture()),
    ] {
        let rendered = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
        fs::write(
            dir.join(format!("sample/{name}.java")),
            format!("package sample;public class {name} {{{}}}", rendered.source),
        )
        .unwrap();
    }
    fs::write(dir.join("sample/Reference.java"),"package sample;public class Reference {public static A make(){return new A(switch(0){default->{new B(switch(0){default->{new C(Source.value());yield 7;}});yield 7;}});}public static A loop(int n){return new A(switch(0){default->{new B(switch(0){default->{new C(sum(n));yield 7;}});yield 7;}});}public static A sibling(){make();return make();}static int sum(int n){int value=7;for(int i=0;i<n;i++)value=Source.value();return value;}public static A branch(boolean b){return new A(switch(0){default->{new B(switch(0){default->{new C(b?Source.value():7);yield 7;}});yield 7;}});}}").unwrap();
    fs::write(dir.join("sample/State.java"),"package sample;public class State {public static int mask;public static String events=\"\";public static Throwable problem;public static void hit(String event,int bit){events+=event;if((mask&bit)!=0){problem=new RuntimeException(event);throw (RuntimeException)problem;}}}").unwrap();
    for (name, init, ctor) in [("A", 1, 128), ("B", 2, 64), ("C", 4, 32)] {
        fs::write(dir.join(format!("sample/{name}.java")),format!("package sample;public class {name} {{static{{State.hit(\"{name}\",{init});}}public int value;public {name}(int value){{State.hit(\"{name}c\",{ctor});this.value=value;}}}}")).unwrap();
    }
    fs::write(dir.join("sample/Source.java"),"package sample;public class Source {static{State.hit(\"S\",8);}public static int value(){State.hit(\"V\",16);return 7;}public static void handled(Throwable t){State.hit(\"H\",0);}}").unwrap();
    fs::write(dir.join("sample/ReferenceProtected.java"),"package sample;public class ReferenceProtected {public static A make()throws Throwable{try{return Reference.make();}catch(Throwable e){Source.handled(e);throw e;}}}").unwrap();
    fs::write(dir.join("Runner.java"),r#"import java.net.*;import java.nio.file.*;import java.lang.reflect.*;public class Runner {
static String call(String type,String method,int mask,Object input)throws Exception{try(URLClassLoader l=new URLClassLoader(new URL[]{Path.of(System.getProperty("classes")).toUri().toURL()},null)){Class<?> s=Class.forName("sample.State",true,l);s.getField("mask").setInt(null,mask);String result;try{Class<?> c=Class.forName("sample."+type,true,l);Object a=input==null?c.getMethod(method).invoke(null):c.getMethod(method,input instanceof Boolean?boolean.class:int.class).invoke(null,input);result="value="+a.getClass().getField("value").get(a);}catch(InvocationTargetException e){Throwable t=e.getCause();result=t.getClass().getName()+":"+(t==s.getField("problem").get(null));}return s.getField("events").get(null)+":"+result;}}
static void compare(String type,String method,int mask,Object input)throws Exception{String a=call(type,"make",mask,input),b=call(type.equals("Protected")?"ReferenceProtected":"Reference",method,mask,input);if(!a.equals(b))throw new AssertionError(type+"/"+mask+"/"+input+" "+a+" != "+b);}
public static void main(String[]args)throws Exception{int count=0;for(int mask=0;mask<256;mask++){for(String type:new String[]{"Narrow","Wide","Protected","Sibling"}){compare(type,type.equals("Sibling")?"sibling":"make",mask,null);count++;}for(int n:new int[]{0,1,3}){compare("Loop","loop",mask,n);count++;}for(boolean b:new boolean[]{false,true}){compare("Branch","branch",mask,b);count++;}}System.out.print("ok:"+count);}}"#).unwrap();
    let mut javac = Command::new(format!("{home}/bin/javac"));
    javac.arg("-d").arg(&dir);
    for e in fs::read_dir(dir.join("sample")).unwrap() {
        javac.arg(e.unwrap().path());
    }
    javac.arg(dir.join("Runner.java"));
    let result = javac.output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let result = Command::new(format!("{home}/bin/java"))
        .arg(format!("-Dclasses={}", dir.display()))
        .arg("-cp")
        .arg(&dir)
        .arg("Runner")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, b"ok:2304");
    fs::remove_dir_all(dir).unwrap();
}
fn projection_sibling_fixture() -> DexClass {
    let mut c = projection_nested_fixture(false);
    let second = c.methods[0].code.as_ref().unwrap().instructions.clone();
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions.pop();
    code.instructions.extend(second);
    c
}
#[test]
fn two_sibling_allocations_restore_projection_scope() {
    let c = projection_sibling_fixture();
    let rendered = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
    assert_eq!(rendered.source.matches("new sample.A").count(), 2);
    assert_eq!(rendered.source.matches("new sample.C").count(), 2);
    assert_eq!(rendered.source.matches("sample.Source.value()").count(), 2);
}
fn projection_protected_fixture(unrelated: bool) -> DexClass {
    let mut c = projection_nested_fixture(false);
    let s = Arc::get_mut(&mut c.symbols).unwrap();
    s.strings.push("handled".into());
    s.protos
        .push(("V".into(), vec!["Ljava/lang/Throwable;".into()]));
    s.methods.push((1, 2, 2));
    c.methods[0].thrown_types = vec!["Ljava/lang/Throwable;".into()];
    let code = c.methods[0].code.as_mut().unwrap();
    code.tries = if unrelated { 2 } else { 1 };
    code.try_regions = vec![DexTryRegion {
        start: 0,
        end: if unrelated { 24 } else { 25 },
        catches: vec![(Some("Ljava/lang/Throwable;".into()), 25)].into(),
    }];
    if unrelated {
        code.try_regions.push(DexTryRegion {
            start: 24,
            end: 25,
            catches: vec![(Some("Ljava/lang/Throwable;".into()), 25)].into(),
        });
    }
    code.instructions.extend([0x010d, 0x1071, 4, 1, 0x0127]);
    c
}
#[test]
fn projection_try_metadata_erases_only_whole_parent_enclosures() {
    for unrelated in [false, true] {
        let mut c = projection_protected_fixture(unrelated);
        let plan = proof::prove(&c, &c.methods[0], 0, 21).unwrap();
        let projection = proof::Projection::extend(&c, &c.methods[0], None, &plan).unwrap();
        let code = c.methods[0].code.as_mut().unwrap();
        code.instructions[6..8].fill(0);
        code.try_regions.remove(0);
        assert!(projection.checked_ir(&c, &c.methods[0]).is_some());
        if unrelated {
            let code = c.methods[0].code.as_mut().unwrap();
            let original = code.try_regions.clone();
            code.try_regions[0].start = 23;
            assert!(projection.checked_ir(&c, &c.methods[0]).is_none());
            c.methods[0].code.as_mut().unwrap().try_regions = original.clone();
            c.methods[0].code.as_mut().unwrap().try_regions[0].catches =
                vec![(Some("Ljava/lang/Throwable;".into()), 26)].into();
            assert!(projection.checked_ir(&c, &c.methods[0]).is_none());
            c.methods[0].code.as_mut().unwrap().try_regions = original;
            c.methods[0].code.as_mut().unwrap().try_regions.clear();
            assert!(projection.checked_ir(&c, &c.methods[0]).is_none());
        } else {
            let rendered = native_java::render_method(
                "sample.Test",
                &projection_protected_fixture(false),
                &projection_protected_fixture(false).methods[0],
            )
            .unwrap();
            assert!(rendered.source.contains("catch"), "{}", rendered.source);
        }
    }
}
#[test]
fn projection_revalidation_rejects_fabricated_cached_ir_and_call_bindings() {
    let original = projection_nested_fixture(false);
    let mut staged = projection_nested_fixture(false);
    let parent = proof::prove(&original, &original.methods[0], 0, 21).unwrap();
    let projection =
        proof::Projection::extend(&original, &original.methods[0], None, &parent).unwrap();
    staged.methods[0].code.as_mut().unwrap().instructions[6..8].fill(0);
    let analysis = native_method::MethodAnalysis::build(&original, &original.methods[0]).unwrap();
    let constructors = analysis.constructors().unwrap();
    let raw_ir =
        native_ir::DecodedMethod::decode(original.methods[0].code.as_ref().unwrap()).unwrap();
    let bound = native_calls::BoundCalls::bind(
        original.methods[0].code.as_ref().unwrap(),
        &raw_ir,
        &original.symbols,
    )
    .unwrap();
    let cfg =
        native_cfg::ControlFlowGraph::build(original.methods[0].code.as_ref().unwrap()).unwrap();
    let mut ir = projection.checked_ir(&staged, &staged.methods[0]).unwrap();
    let i = ir.instructions.iter_mut().find(|i| i.pc == 6).unwrap();
    i.opcode = 0x12;
    i.writes.push(native_ir::RegisterOperand {
        register: 4,
        kind: native_ir::ValueKind::Bits32,
    });
    let fake = proof::prove_cached(
        &staged,
        &staged.methods[0],
        2,
        18,
        proof::ProofInputs {
            ir: &ir,
            cfg: &cfg,
            bound: &bound,
            bindings: &constructors.bindings,
        },
    )
    .unwrap();
    assert!(!projection.validate(&staged, &staged.methods[0], &fake));
    let ir = projection.checked_ir(&staged, &staged.methods[0]).unwrap();
    let mut forged = bound.clone();
    if let native_calls::CallTarget::Method { name, .. } = &mut forged
        .calls
        .iter_mut()
        .find(|call| call.pc == 11)
        .unwrap()
        .target
    {
        *name = "fabricated".into();
    } else {
        panic!("method call");
    }
    let fake = proof::prove_cached(
        &staged,
        &staged.methods[0],
        2,
        18,
        proof::ProofInputs {
            ir: &ir,
            cfg: &cfg,
            bound: &forged,
            bindings: &constructors.bindings,
        },
    )
    .unwrap();
    assert!(!projection.validate(&staged, &staged.methods[0], &fake));
    let inner = projection
        .prove(&staged, &staged.methods[0], 2, 18)
        .unwrap();
    assert!(projection.validate(&staged, &staged.methods[0], &inner));
    assert!(
        proof::Projection::extend(&staged, &staged.methods[0], Some(&projection), &parent)
            .is_none()
    );
}

fn internal_handler_fixture() -> DexClass {
    let mut c = fixture();
    let symbols = Arc::get_mut(&mut c.symbols).unwrap();
    symbols.strings.push("handled".into());
    symbols
        .protos
        .push(("I".into(), vec!["Ljava/lang/Throwable;".into()]));
    symbols.types.push("Lsample/Handler;".into());
    symbols.methods.push((2, 2, 2));
    let code = c.methods[0].code.as_mut().unwrap();
    code.registers = 3;
    code.tries = 1;
    code.try_regions = vec![DexTryRegion {
        start: 2,
        end: 6,
        catches: vec![(None, 7)].into(),
    }];
    code.instructions = vec![
        0x0022, 0, 0x0071, 0, 0, 0x010a, 0x0628, 0x020d, 0x1071, 2, 2, 0x010a, 0x2070, 1, 0x0010,
        0x0011,
    ];
    c
}
#[test]
fn private_internal_handler_first_closed_shape() {
    let c = internal_handler_fixture();
    let p = proof::prove(&c, &c.methods[0], 0, 12).expect("original closed handler proof");
    assert!(p.owned.contains(&7));
    assert_eq!(p.exception_edges, vec![(2, vec![7]), (5, vec![7])]);
    let result = native_java::render_method("sample.Test", &c, &c.methods[0]);
    let result = result.unwrap();
    for label in [
        "sample.A.<init>(I)V",
        "sample.Source.value()I",
        "sample.Handler.handled(Ljava/lang/Throwable;)I",
    ] {
        let link = result
            .links
            .iter()
            .find(|link| link.label == label)
            .expect(label);
        assert!(link.start < link.end && link.end <= result.source.len());
    }
}

#[test]
fn private_internal_handler_refuses_crossing_shared_reentry_alias_and_frames() {
    let baseline = internal_handler_fixture();
    assert!(proof::prove(&baseline, &baseline.methods[0], 0, 12).is_some());
    let code = baseline.methods[0].code.as_ref().unwrap();
    for (start, end, handler) in [(0, 6, 7), (2, 13, 7), (2, 6, 2), (2, 6, 12)] {
        let mut c = internal_handler_fixture();
        c.methods[0].code.as_mut().unwrap().try_regions = vec![DexTryRegion {
            start,
            end,
            catches: vec![(None, handler)].into(),
        }];
        assert!(
            proof::prove(&c, &c.methods[0], 0, 12).is_none(),
            "crossed {start}..{end}->{handler}"
        );
    }
    let mut c = internal_handler_fixture();
    c.methods[0]
        .code
        .as_mut()
        .unwrap()
        .try_regions
        .push(DexTryRegion {
            start: 15,
            end: 16,
            catches: vec![(None, 7)].into(),
        });
    assert!(proof::prove(&c, &c.methods[0], 0, 12).is_none());
    let mut c = internal_handler_fixture();
    c.methods[0].code.as_mut().unwrap().instructions[15] = 0xf828;
    assert!(
        proof::prove(&c, &c.methods[0], 0, 12).is_none(),
        "foreign normal entry into handler"
    );
    let mut c = internal_handler_fixture();
    c.methods[0].code.as_mut().unwrap().try_regions[0].catches =
        vec![(Some("Lsample/Source;".into()), 7)].into();
    assert!(proof::prove(&c, &c.methods[0], 0, 12).is_none());
    let mut c = internal_handler_fixture();
    Arc::get_mut(&mut c.symbols).unwrap().protos[2].1 = vec!["Lsample/A;".into()];
    c.methods[0].code.as_mut().unwrap().instructions[10] = 0;
    assert!(
        proof::prove(&c, &c.methods[0], 0, 12).is_none(),
        "uninitialized receiver escapes through handler"
    );
    let mut c = internal_handler_fixture();
    Arc::get_mut(&mut c.symbols).unwrap().protos[2].0 = "Ljava/lang/Object;".into();
    c.methods[0].code.as_mut().unwrap().instructions[11] = 0x010c;
    assert!(
        native_java::render_method("sample.Test", &c, &c.methods[0]).is_err(),
        "caught Object cannot merge with int constructor argument"
    );
    assert_eq!(code.instructions.len(), 16);
}
fn internal_prior_write_fixture() -> DexClass {
    let mut c = internal_handler_fixture();
    let symbols = Arc::get_mut(&mut c.symbols).unwrap();
    symbols.types.push("I".into());
    symbols.strings.push("number".into());
    symbols.fields.push((1, 3, 3));
    symbols.protos[2].1 = vec!["I".into()];
    c.methods[0].parameters = vec!["Lsample/Source;".into()];
    let code = c.methods[0].code.as_mut().unwrap();
    code.try_regions = vec![DexTryRegion {
        start: 3,
        end: 5,
        catches: vec![(None, 6)].into(),
    }];
    code.instructions = vec![
        0x0022, 0, 0x7112, 0x2152, 0, 0x0628, 0x020d, 0x1071, 2, 1, 0x010a, 0x2070, 1, 0x0010,
        0x0011,
    ];
    c
}
#[test]
fn private_internal_handler_reads_prior_state_of_failed_field_write() {
    let c = internal_prior_write_fixture();
    let p = proof::prove(&c, &c.methods[0], 0, 11).unwrap();
    assert_eq!(p.exception_edges, vec![(3, vec![6])]);
    assert!(!p.first_reads.iter().any(|r| r.pc == 7 && r.register == 1));
    let rendered = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
    println!("{}", rendered.source);
    assert!(rendered.source.contains("sample.Handler.handled"));
    let mut c = internal_prior_write_fixture();
    c.methods[0].code.as_mut().unwrap().instructions[2] = 0;
    let p = proof::prove(&c, &c.methods[0], 0, 11).unwrap();
    assert!(p.first_reads.iter().any(|r| r.pc == 7 && r.register == 1));
    assert!(
        native_java::render_method("sample.Test", &c, &c.methods[0]).is_err(),
        "failed field read cannot define old destination"
    );
}
#[test]
#[ignore = "requires Java 25 via RDX_JAVA25_HOME"]
fn private_internal_handler_java_fault_order_identity_and_failed_writes() {
    let home = std::env::var("RDX_JAVA25_HOME").unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-internal-handler-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    for (name, c) in [
        ("Call", internal_handler_fixture()),
        ("Prior", internal_prior_write_fixture()),
    ] {
        let rendered =
            native_java::render_method(&format!("sample.{name}"), &c, &c.methods[0]).unwrap();
        fs::write(
            dir.join(format!("sample/{name}.java")),
            format!("package sample;public class {name} {{{}}}", rendered.source),
        )
        .unwrap();
    }
    fs::write(dir.join("sample/State.java"),r#"package sample;public class State {public static int mask;public static boolean error;public static Throwable problem,caught,outer;public static String events="";public static void step(String n,int bit){events+=n+",";if((mask&bit)!=0){if(problem==null)problem=error?new Error("fault"):new RuntimeException("fault");if(problem instanceof Error e)throw e;throw (RuntimeException)problem;}}}"#).unwrap();
    fs::write(dir.join("sample/A.java"),r#"package sample;public class A{static{State.step("Ainit",1);}public int value;public A(int n){State.step("Actor",32);value=n;}}"#).unwrap();
    fs::write(dir.join("sample/Source.java"),r#"package sample;public class Source{static{State.step("Sinit",2);}public int number=19;public static int value(){State.step("value",4);return 19;}}"#).unwrap();
    fs::write(dir.join("sample/Handler.java"),r#"package sample;public class Handler{static{State.step("Hinit",8);}public static int handled(Throwable e){State.caught=e;State.step("handledThrowable",16);return -7;}public static int handled(int n){State.step("handledInt:"+n,16);return n;}}"#).unwrap();
    fs::write(dir.join("sample/ReferenceCall.java"),r#"package sample;public class ReferenceCall{public static A make(boolean ignored){return new A(switch(0){default->{int value;try{value=Source.value();}catch(Throwable caught){value=Handler.handled(caught);}yield value;}});}}"#).unwrap();
    fs::write(dir.join("sample/ReferencePrior.java"),r#"package sample;public class ReferencePrior{public static A make(Source input){return new A(switch(0){default->{int destination=7;try{destination=input.number;}catch(Throwable caught){destination=Handler.handled(destination);}yield destination;}});}}"#).unwrap();
    for (name, target) in [
        ("OuterCall", "Call"),
        ("ReferenceOuterCall", "ReferenceCall"),
    ] {
        fs::write(dir.join(format!("sample/{name}.java")), format!("package sample;public class {name}{{public static A make(boolean flag)throws Throwable{{try{{return {target}.make(flag);}}catch(Throwable fault){{State.outer=fault;State.events+=\"outer,\";throw fault;}}}}}}" )).unwrap();
    }
    fs::write(dir.join("Runner.java"),r#"import java.net.*;import java.nio.file.*;import java.lang.reflect.*;public class Runner{
 static String identity(Throwable t,Object problem){return t==null?"none":t.getClass().getName()+":"+(t==problem)+":"+(t.getCause()==problem);}
 static String call(String name,int mask,boolean error,boolean nullInput)throws Exception{try(URLClassLoader loader=new URLClassLoader(new URL[]{Path.of(System.getProperty("classes")).toUri().toURL()},null)){Class<?> state=Class.forName("sample.State",true,loader);state.getField("mask").setInt(null,mask);state.getField("error").setBoolean(null,error);String result;try{Class<?> type=Class.forName("sample."+name,true,loader);Object value;if(name.endsWith("Prior")){Class<?> source=Class.forName("sample.Source",false,loader);Object input=nullInput?null:source.getConstructor().newInstance();value=type.getMethod("make",source).invoke(null,input);}else{value=type.getMethod("make",boolean.class).invoke(null,true);}result="value="+value.getClass().getField("value").get(value);}catch(InvocationTargetException e){result=identity(e.getCause(),state.getField("problem").get(null));}catch(Throwable e){result=identity(e,state.getField("problem").get(null));}return state.getField("events").get(null)+":"+result+":caught="+identity((Throwable)state.getField("caught").get(null),state.getField("problem").get(null))+":outer="+identity((Throwable)state.getField("outer").get(null),state.getField("problem").get(null));}}
 public static void main(String[]args)throws Exception{int n=0;for(int mask=0;mask<64;mask++)for(boolean error:new boolean[]{false,true})for(boolean nil:new boolean[]{false,true})for(String name:new String[]{"Call","Prior","OuterCall"}){String actual=call(name,mask,error,nil),expected=call("Reference"+name,mask,error,nil);if(!actual.equals(expected))throw new AssertionError(name+":"+mask+":"+error+":"+nil+":"+actual+" != "+expected);n++;}System.out.print("ok:"+n);}}
"#).unwrap();
    let files = fs::read_dir(dir.join("sample"))
        .unwrap()
        .map(|f| f.unwrap().path())
        .chain(std::iter::once(dir.join("Runner.java")))
        .collect::<Vec<_>>();
    let compile = Command::new(format!("{home}/bin/javac"))
        .arg("-d")
        .arg(&dir)
        .args(files)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(format!("{home}/bin/java"))
        .arg(format!("-Dclasses={}", dir.display()))
        .arg("-cp")
        .arg(&dir)
        .arg("Runner")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "ok:768");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn private_internal_handler_certificate_rejects_fabricated_cached_calls() {
    let c = internal_handler_fixture();
    let m = &c.methods[0];
    let analysis = native_method::MethodAnalysis::build(&c, m).unwrap();
    let constructors = analysis.constructors().unwrap();
    let front = native_method::MethodFrontEnd::build(&c, m).unwrap();
    let mut bound = front.bound.clone();
    let call = bound.calls.iter_mut().find(|call| call.pc == 8).unwrap();
    if let native_calls::CallTarget::Method { name, .. } = &mut call.target {
        *name = "fabricated".into();
    }
    let fake = proof::prove_cached(
        &c,
        m,
        0,
        12,
        proof::ProofInputs {
            ir: analysis.instructions(),
            cfg: analysis.blocks(),
            bound: &bound,
            bindings: &constructors.bindings,
        },
    )
    .unwrap();
    assert!(!proof::validate(&c, m, &fake));
    let canonical = proof::prove(&c, m, 0, 12).unwrap();
    assert!(proof::validate(&c, m, &canonical));
    let mut changed = c.methods[0].code.as_ref().unwrap().try_regions[0].clone();
    changed.end -= 1;
    let mut c = internal_handler_fixture();
    c.methods[0].code.as_mut().unwrap().try_regions[0] = changed;
    assert!(!proof::validate(&c, &c.methods[0], &canonical));
}

fn internal_outer_handler_fixture() -> DexClass {
    let mut c = internal_handler_fixture();
    let symbols = Arc::get_mut(&mut c.symbols).unwrap();
    symbols.types.push("Lsample/Outer;".into());
    symbols
        .protos
        .push(("Lsample/A;".into(), vec!["Ljava/lang/Throwable;".into()]));
    symbols.methods.push((3, 3, 2));
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions
        .extend([0x020d, 0x1071, 3, 2, 0x000c, 0x0011]);
    code.tries = 2;
    code.try_regions.insert(
        0,
        DexTryRegion {
            start: 0,
            end: 16,
            catches: vec![(Some("Ljava/lang/Throwable;".into()), 16)].into(),
        },
    );
    c
}
#[test]
fn private_internal_handler_enclosing_dispatch_boundary() {
    let c = internal_outer_handler_fixture();
    let error = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap_err();
    assert!(format!("{error:#}").contains("cleanup lacks outer prefix catch"));
}

#[test]
fn private_internal_handler_refuses_owned_normal_reentry_and_fallthrough() {
    let baseline = internal_handler_fixture();
    assert!(proof::prove(&baseline, &baseline.methods[0], 0, 12).is_some());
    for (pc, word) in [(11, 0xfc28), (6, 0x0000), (5, 0x0228)] {
        let mut c = internal_handler_fixture();
        c.methods[0].code.as_mut().unwrap().instructions[pc] = word;
        assert!(
            proof::prove(&c, &c.methods[0], 0, 12).is_none(),
            "normal handler entry from {pc}"
        );
    }
    // The unchanged handler tail falls through to the constructor join, which
    // is a normal endpoint rather than an exceptional handler head.
    assert!(native_java::render_method("sample.Test", &baseline, &baseline.methods[0]).is_ok());
}
