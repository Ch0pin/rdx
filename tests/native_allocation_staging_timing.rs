use rdx::{
    native_dex::{DexClass, DexCode, DexField, DexMethod, DexSymbols},
    native_hierarchy::TypeHierarchy,
    native_java,
    native_method::MethodAnalysis,
};
use std::{process::Command, sync::Arc};
#[allow(clippy::too_many_arguments)]
fn method(
    owner: &str,
    name: &str,
    ret: &str,
    args: &[&str],
    flags: u32,
    regs: u16,
    ins: u16,
    words: Vec<u16>,
) -> DexMethod {
    DexMethod {
        declaring_type: owner.into(),
        name: name.into(),
        return_type: ret.into(),
        parameters: args.iter().map(|x| (*x).into()).collect(),
        thrown_types: vec![],
        access_flags: flags,
        code: Some(DexCode {
            registers: regs,
            ins,
            outs: 2,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: words,
        }),
    }
}
fn fixture() -> Vec<DexClass> {
    fixture_with_initializer(true)
}
fn fixture_with_initializer(has_initializer: bool) -> Vec<DexClass> {
    let symbols = Arc::new(DexSymbols {
        types: vec![
            "Lsample/Target;".into(),
            "Lsample/Effects;".into(),
            "Ljava/lang/Object;".into(),
            "I".into(),
        ],
        strings: vec![
            "<init>".into(),
            "one".into(),
            "two".into(),
            "init".into(),
            "a".into(),
            "base".into(),
            "discard".into(),
        ],
        protos: vec![
            ("V".into(), vec!["I".into()]),
            ("I".into(), vec![]),
            ("V".into(), vec![]),
        ],
        methods: vec![
            (0, 0, 0),
            (1, 1, 1),
            (1, 1, 2),
            (2, 2, 0),
            (1, 2, 3),
            (1, 2, 5),
            (1, 2, 6),
        ],
        fields: vec![(0, 3, 4)],
        ..Default::default()
    });
    let c = |descriptor: &str, fields, methods| DexClass {
        descriptor: descriptor.into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields,
        symbols: symbols.clone(),
        methods,
    };
    let caller = c(
        "Lsample/Caller;",
        vec![],
        vec![method(
            "Lsample/Caller;",
            "run",
            "Lsample/Target;",
            &["Z"],
            9,
            3,
            1,
            vec![
                0x0022, 0, 0x0238, 7, 0x0071, 1, 0, 0x010a, 0x0528, 0x0071, 2, 0, 0x010a, 0x2070,
                0, 0x0010, 0x0011,
            ],
        )],
    );
    let target = c(
        "Lsample/Target;",
        vec![DexField {
            name: "a".into(),
            field_type: "I".into(),
            access_flags: 1,
            declaring_type: "Lsample/Target;".into(),
            is_static: false,
        }],
        vec![
            method(
                "Lsample/Target;",
                "<init>",
                "V",
                &["I"],
                1,
                2,
                2,
                vec![0x1070, 3, 0, 0x0071, 5, 0, 0x0159, 0, 0x000e],
            ),
            method(
                "Lsample/Target;",
                "<clinit>",
                "V",
                &[],
                8,
                0,
                0,
                vec![0x0071, 4, 0, 0x000e],
            ),
        ],
    );
    let effects = c(
        "Lsample/Effects;",
        vec![],
        vec![
            method(
                "Lsample/Effects;",
                "one",
                "I",
                &[],
                9,
                1,
                0,
                vec![0x1012, 0x000f],
            ),
            method(
                "Lsample/Effects;",
                "two",
                "I",
                &[],
                9,
                1,
                0,
                vec![0x2012, 0x000f],
            ),
            method("Lsample/Effects;", "init", "V", &[], 9, 0, 0, vec![0x000e]),
        ],
    );
    let _external_effects_declaration = effects;
    let mut classes = vec![caller, target];
    if !has_initializer {
        classes[1].methods.retain(|m| m.name.as_ref() != "<clinit>");
    }
    let h = Arc::new(TypeHierarchy::from_classes(classes.iter()).unwrap());
    symbols.hierarchy.set(h).unwrap();
    classes
}

fn pure_fixture() -> Vec<DexClass> {
    let mut c = fixture();
    c[0].methods[0].code.as_mut().unwrap().instructions = vec![
        0x0022, 0, 0x0238, 7, 0x1112, 0, 0, 0, 0x0528, 0x2112, 0, 0, 0, 0x2070, 0, 0x0010, 0x0011,
    ];
    c
}
#[test]
fn effectful_branch_arguments_never_run_before_delayed_allocation() {
    let c = fixture();
    MethodAnalysis::build(&c[0], &c[0].methods[0]).unwrap();
    let rendered = native_java::render_method("sample.Caller", &c[0], &c[0].methods[0]).unwrap();
    assert!(rendered.source.contains("switch (0)"));
    let allocation = rendered.source.find("new sample.Target").unwrap();
    for call in ["sample.Effects.one()", "sample.Effects.two()"] {
        assert!(allocation < rendered.source.find(call).unwrap());
    }
    let no_initializer = fixture_with_initializer(false);
    let rendered = native_java::render_method(
        "sample.Caller",
        &no_initializer[0],
        &no_initializer[0].methods[0],
    )
    .unwrap();
    assert!(rendered.source.contains("switch (0)"));
    let allocation = rendered.source.find("new sample.Target").unwrap();
    assert!(allocation < rendered.source.find("sample.Effects.one()").unwrap());
    assert!(allocation < rendered.source.find("sample.Effects.two()").unwrap());
}
#[test]
fn discarded_linear_call_stays_after_allocation() {
    let mut c = fixture();
    c[0].methods[0].code.as_mut().unwrap().instructions =
        vec![0x0022, 0, 0x0071, 6, 0, 0x1112, 0x2070, 0, 0x0010, 0x0011];
    MethodAnalysis::build(&c[0], &c[0].methods[0]).unwrap();
    let rendered = native_java::render_method("sample.Caller", &c[0], &c[0].methods[0]).unwrap();
    assert!(rendered.source.contains("switch (0)"));
    assert!(
        rendered.source.find("new sample.Target").unwrap()
            < rendered.source.find("Effects.discard()").unwrap()
    );
}
#[test]
fn pure_branch_preparation_preserves_values_and_definition_guards() {
    let c = pure_fixture();
    MethodAnalysis::build(&c[0], &c[0].methods[0]).unwrap();
    let output = native_java::render_method("sample.Caller", &c[0], &c[0].methods[0]).unwrap();
    assert!(output.source.contains("new sample.Target") || output.source.contains("new Target"));
    let mut skipped = pure_fixture();
    skipped[0].methods[0].code.as_mut().unwrap().instructions[4] = 0;
    MethodAnalysis::build(&skipped[0], &skipped[0].methods[0]).unwrap();
    assert!(
        native_java::render_method("sample.Caller", &skipped[0], &skipped[0].methods[0]).is_err()
    );
}
#[test]
fn wide_overwrite_cannot_restore_an_allocation_alias() {
    let mut c = fixture();
    let code = c[0].methods[0].code.as_mut().unwrap();
    code.registers = 5;
    // Alias v2 then overwrite its word as the tail of v1; v2 is not a receiver.
    code.instructions = vec![
        0x0022, 0, 0x0207, 0x0116, 0, 0x1312, 0x2070, 0, 0x0032, 0x0211,
    ];
    assert!(native_java::render_method("sample.Caller", &c[0], &c[0].methods[0]).is_err());
}
#[test]
#[ignore = "requires javac/java; fresh loaders validate original initializer/constructor failure identity"]
fn unchanged_source_jvm_checks_pure_preparation_and_already_evaluated_effects() {
    let c = pure_fixture();
    let pure = native_java::render_method("sample.Caller", &c[0], &c[0].methods[0]).unwrap();
    let mut before = fixture();
    before[0].methods[0].name = "before".into();
    before[0].methods[0].code.as_mut().unwrap().instructions =
        vec![0x0071, 1, 0, 0x010a, 0x0022, 0, 0x2070, 0, 0x0010, 0x0011];
    MethodAnalysis::build(&before[0], &before[0].methods[0]).unwrap();
    let before =
        native_java::render_method("sample.Caller", &before[0], &before[0].methods[0]).unwrap();
    let mut discarded = fixture();
    discarded[0].methods[0].name = "discarded".into();
    discarded[0].methods[0].code.as_mut().unwrap().instructions =
        vec![0x0022, 0, 0x0071, 6, 0, 0x1112, 0x2070, 0, 0x0010, 0x0011];
    MethodAnalysis::build(&discarded[0], &discarded[0].methods[0]).unwrap();
    let discarded =
        native_java::render_method("sample.Caller", &discarded[0], &discarded[0].methods[0])
            .unwrap();
    let mut branched = fixture();
    branched[0].methods[0].name = "branched".into();
    MethodAnalysis::build(&branched[0], &branched[0].methods[0]).unwrap();
    let branched =
        native_java::render_method("sample.Caller", &branched[0], &branched[0].methods[0]).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-staging-timing-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    std::fs::write(dir.join("sample/Caller.java"),format!(r#"package sample; public class Caller {{
{}
{}
{}
{}
public static Target originalDiscarded(boolean flag) {{ return new Target(switch (0) {{ default -> {{ Effects.discard(); yield 1; }} }}); }}
public static Target originalBranched(boolean flag) {{ return new Target(flag?Effects.one():Effects.two()); }}
public static Target originalPure(boolean flag) {{ return new Target(flag?1:2); }}
public static Target originalBefore(boolean flag) {{ int n=Effects.one(); return new Target(n); }}
}}
class Target {{ static {{ Effects.init(); }} public int a; public Target(int a) {{ Effects.base(); this.a=a; }} }}
class Effects {{ public static String trace=""; public static int mode; public static final RuntimeException fault=new RuntimeException("sentinel"); static void init() {{ trace+="C"; if(mode==1)throw fault; }} static int one() {{ trace+="A"; if(mode==2)throw fault; return 1; }} static int two() {{ trace+="D"; if(mode==5)throw fault; return 2; }} static void base() {{ trace+="B"; if(mode==3)throw fault; }} static void discard() {{ trace+="V"; if(mode==4)throw fault; }} }}
"#,pure.source,before.source,discarded.source,branched.source)).unwrap();
    std::fs::write(dir.join("Driver.java"),r#"import java.net.*; import java.lang.reflect.*; import java.io.*;
public class Driver {
static String run(String method,int mode,boolean flag)throws Exception {
try(URLClassLoader l=new URLClassLoader(new URL[]{new File(System.getProperty("classes")).toURI().toURL()},null)) {
Class<?> e=Class.forName("sample.Effects",true,l); Field m=e.getField("mode"),t=e.getField("trace"),f=e.getField("fault"); m.setAccessible(true);t.setAccessible(true);f.setAccessible(true);m.setInt(null,mode);Object sentinel=f.get(null);
String outcome; try { Object value=Class.forName("sample.Caller",true,l).getMethod(method,boolean.class).invoke(null,flag); Field a=value.getClass().getField("a");a.setAccessible(true);outcome="value:"+a.getInt(value); }
catch(InvocationTargetException x) { Throwable z=x.getCause(); outcome=z.getClass().getName()+":"+((z==sentinel)||(z.getCause()==sentinel)); }
return t.get(null)+"/"+outcome;
}}
public static void main(String[] args)throws Exception {int count=0;for(int mode=0;mode<6;mode++)for(boolean flag:new boolean[]{false,true})for(String[] p:new String[][]{{"run","originalPure"},{"before","originalBefore"},{"discarded","originalDiscarded"},{"branched","originalBranched"}}){String a=run(p[0],mode,flag),b=run(p[1],mode,flag);if(!a.equals(b))throw new AssertionError(p[0]+" "+mode+" "+flag+" "+a+" != "+b);count++;}System.out.print("checked="+count);}
}
"#).unwrap();
    let home =
        std::env::var("RDX_JAVA25_HOME").expect("set RDX_JAVA25_HOME to a JDK 25 installation");
    let compile = Command::new(format!("{home}/bin/javac"))
        .args(["-d", dir.to_str().unwrap()])
        .arg(dir.join("sample/Caller.java"))
        .arg(dir.join("Driver.java"))
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(format!("{home}/bin/java"))
        .arg("-Xverify:all")
        .arg(format!("-Dclasses={}", dir.display()))
        .args(["-cp", dir.to_str().unwrap(), "Driver"])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8(run.stdout).unwrap(), "checked=48");
}
