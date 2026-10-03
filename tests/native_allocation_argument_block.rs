use rdx::{
    native_dex::{DexClass, DexCode, DexField, DexMethod, DexSymbols, DexTryRegion},
    native_hierarchy::TypeHierarchy,
    native_java,
    native_method::MethodAnalysis,
};
use std::{process::Command, sync::Arc};

// Effects call bodies are external. Only checkedWide has a loaded native
// declaration to provide its checked contract; no substituted loaded method body
// exists. Java implements exactly the external semantics used by the interpreter.
// Loaded Target bodies retain the
// same initializer, field writes, and constructor call order in Java.
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
            outs: 5,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: words,
        }),
    }
}
#[derive(Clone, Copy, Debug)]
enum Shape {
    ObjectWide,
    WrapperWide,
    ShortWide,
    Checked,
    Unicode,
    Nested,
    Escape,
    AliasEscape,
    WideOverlap,
    WrongReference,
    ExternalEntry,
}
fn fixture(shape: Shape) -> Vec<DexClass> {
    // Method indices: 0 Object/J ctor, 1 prefix(), 2 wide()J, 3 Object ctor,
    // 4 init(), 5 constructed(), 6 observe(Object,J), 7 Short/J ctor,
    // 8 byteValue()B, 9 checkedWide()J, 10 nested() Object, 11 consume(Object),
    // 12 RuntimeException/J ctor, 13 obtain()Object.
    let symbols = Arc::new(DexSymbols {
        types: vec![
            "Lsample/Target;".into(),
            "Lsample/Effects;".into(),
            "Ljava/lang/Object;".into(),
            "J".into(),
            "S".into(),
            "Ljava/lang/RuntimeException;".into(),
        ],
        strings: vec![
            "<init>".into(),
            "prefix".into(),
            "wide".into(),
            "init".into(),
            "constructed".into(),
            "observe".into(),
            "byteValue".into(),
            "checkedWide".into(),
            "nested".into(),
            "consume".into(),
            "obtain".into(),
            "obj".into(),
            "wideField".into(),
            "narrow".into(),
            "wrong".into(),
            "text".into(),
            "é🙂".into(),
        ],
        protos: vec![
            ("V".into(), vec!["Ljava/lang/Object;".into(), "J".into()]),
            ("V".into(), vec![]),
            ("J".into(), vec![]),
            ("V".into(), vec!["S".into(), "J".into()]),
            ("B".into(), vec![]),
            ("Ljava/lang/Object;".into(), vec![]),
            ("V".into(), vec!["Ljava/lang/Object;".into()]),
            (
                "V".into(),
                vec!["Ljava/lang/RuntimeException;".into(), "J".into()],
            ),
            ("V".into(), vec!["Ljava/lang/Integer;".into(), "J".into()]),
            ("V".into(), vec!["Ljava/lang/Object;".into(), "D".into()]),
            ("V".into(), vec!["B".into(), "J".into()]),
            ("V".into(), vec!["Ljava/lang/String;".into()]),
        ],
        methods: vec![
            (0, 0, 0),
            (1, 1, 1),
            (1, 2, 2),
            (2, 1, 0),
            (1, 1, 3),
            (1, 1, 4),
            (1, 0, 5),
            (0, 3, 0),
            (1, 4, 6),
            (1, 2, 7),
            (1, 5, 8),
            (1, 6, 9),
            (0, 7, 0),
            (1, 5, 10),
            (1, 1, 14),
            (1, 11, 15),
        ],
        fields: vec![(0, 2, 11), (0, 3, 12), (0, 4, 13)],
        ..Default::default()
    });
    let c = |desc: &str, fields, methods| DexClass {
        descriptor: desc.into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields,
        methods,
        symbols: symbols.clone(),
    };
    let short = matches!(shape, Shape::ShortWide);
    let mut words = if matches!(shape, Shape::ExternalEntry) {
        vec![0x0538, 4, 0x0022, 0]
    } else {
        vec![0x0022, 0]
    }; // external edge bypasses new only in explicit negative
    if matches!(shape, Shape::Unicode) {
        words.extend([0x041a, 16, 0x1071, 15, 4]);
    }
    if matches!(shape, Shape::Nested) {
        words.extend([0x0122, 0, 0x5407, 0x0071, 2, 0, 0x020b, 0x4070, 0, 0x3241]); // valid nested allocation/initialization; initial candidate excludes nested Allocate
    }
    if matches!(shape, Shape::Escape) {
        words.extend([0x1071, 11, 0]);
    }
    if matches!(shape, Shape::AliasEscape) {
        words.extend([0x0407, 0x1071, 11, 4]);
    }
    words.extend([0x0071, 1, 0]); // ignored VOID effect must remain inside allocation.
    if short {
        words.extend([0x0071, 8, 0, 0x010a]);
    } else if matches!(shape, Shape::WrongReference) {
        words.extend([0x0071, 13, 0, 0x010c]);
    } else if !matches!(shape, Shape::Nested) {
        words.push(0x5107);
    } // move-object v1,p0(v5), preserving null/identity.
    words.extend([
        0x0071,
        if matches!(shape, Shape::Checked) {
            9
        } else {
            2
        },
        0,
        0x020b,
    ]);
    if matches!(shape, Shape::WideOverlap) {
        words.push(0x0312);
    } // breaks only high word after coherent move-result-wide.
    words.extend([
        0x4070,
        if short {
            7
        } else if matches!(shape, Shape::WrongReference) {
            12
        } else {
            0
        },
        0x3210,
    ]);
    if !short {
        words.extend([0x3071, 6, 0x0321]);
    } // actual post-constructor liveouts: v1 + v2/v3.
    words.push(0x0011);
    let parameter = if matches!(shape, Shape::WrapperWide) {
        "Ljava/lang/Integer;"
    } else {
        "Ljava/lang/Object;"
    };
    let mut caller = method(
        "Lsample/Caller;",
        "run",
        "Lsample/Target;",
        &[parameter],
        9,
        6,
        1,
        words,
    );
    if matches!(shape, Shape::Checked) {
        let code = caller.code.as_mut().unwrap();
        let end = code.instructions.len() as u32;
        code.instructions.extend([0x040d, 0x0012, 0x0011]);
        code.tries = 1;
        code.try_regions = vec![DexTryRegion {
            start: 0,
            end,
            catches: vec![(Some("Ljava/io/IOException;".into()), end)].into(),
        }];
    }
    let target = c(
        "Lsample/Target;",
        vec![
            DexField {
                name: "obj".into(),
                field_type: "Ljava/lang/Object;".into(),
                access_flags: 1,
                declaring_type: "Lsample/Target;".into(),
                is_static: false,
            },
            DexField {
                name: "wideField".into(),
                field_type: "J".into(),
                access_flags: 1,
                declaring_type: "Lsample/Target;".into(),
                is_static: false,
            },
            DexField {
                name: "narrow".into(),
                field_type: "S".into(),
                access_flags: 1,
                declaring_type: "Lsample/Target;".into(),
                is_static: false,
            },
        ],
        vec![
            method(
                "Lsample/Target;",
                "<init>",
                "V",
                &["Ljava/lang/Object;", "J"],
                1,
                4,
                4,
                vec![0x1070, 3, 0, 0x015b, 0, 0x025a, 1, 0x0071, 5, 0, 0x000e],
            ),
            method(
                "Lsample/Target;",
                "<init>",
                "V",
                &["S", "J"],
                1,
                4,
                4,
                vec![0x1070, 3, 0, 0x015f, 2, 0x025a, 1, 0x0071, 5, 0, 0x000e],
            ),
            method(
                "Lsample/Target;",
                "<init>",
                "V",
                &["Ljava/lang/RuntimeException;", "J"],
                1,
                4,
                4,
                vec![0x1070, 3, 0, 0x015b, 0, 0x025a, 1, 0x0071, 5, 0, 0x000e],
            ),
            method(
                "Lsample/Target;",
                "<init>",
                "V",
                &["Ljava/lang/Integer;", "J"],
                1,
                4,
                4,
                vec![0x1070, 3, 0, 0x0071, 14, 0, 0x000e],
            ),
            method(
                "Lsample/Target;",
                "<init>",
                "V",
                &["Ljava/lang/Object;", "D"],
                1,
                4,
                4,
                vec![0x1070, 3, 0, 0x0071, 14, 0, 0x000e],
            ),
            method(
                "Lsample/Target;",
                "<init>",
                "V",
                &["B", "J"],
                1,
                4,
                4,
                vec![0x1070, 3, 0, 0x0071, 14, 0, 0x000e],
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
    // Checked declaration is loaded without a body: its external implementation
    // is supplied exactly by Java; source contract remains independently visible.
    let mut checked = method(
        "Lsample/Effects;",
        "checkedWide",
        "J",
        &[],
        0x109,
        0,
        0,
        vec![],
    );
    checked.code = None;
    checked.thrown_types = vec!["Ljava/io/IOException;".into()];
    let effects = c("Lsample/Effects;", vec![], vec![checked]);
    let classes = vec![c("Lsample/Caller;", vec![], vec![caller]), target, effects];
    symbols
        .hierarchy
        .set(Arc::new(
            TypeHierarchy::from_classes(classes.iter()).unwrap(),
        ))
        .unwrap();
    classes
}

#[derive(Clone, Debug, PartialEq)]
enum Val {
    Undefined,
    Int(i64),
    Object(&'static str),
    Target {
        obj: Option<&'static str>,
        narrow: i64,
        wide: i64,
    },
}
// Bounded instruction-level oracle for these fixtures; no emitted-source parsing
// and no handwritten ternary/switch model. Every supported raw word is consumed.
fn oracle(shape: Shape, mode: i32, input: Option<&'static str>, byte: i64) -> String {
    let classes = fixture(shape);
    let method = &classes[0].methods[0];
    let code = method.code.as_ref().unwrap();
    let words = &code.instructions;
    let mut regs = vec![Val::Undefined; code.registers as usize];
    regs[5] = input.map_or(Val::Object("null"), Val::Object);
    let mut result = Val::Undefined;
    let mut pc = 0;
    let mut trace = String::new();
    let mut initialized = false;
    let mut failure: Option<&str> = None;
    let mut caught = false;
    for _ in 0..128 {
        let w = words[pc];
        let op = w & 255;
        let a = (w >> 8) as usize;
        let mut width = 1;
        match op {
            0x22 => {
                width = 2;
                if !initialized {
                    initialized = true;
                    trace.push('C');
                    if mode == 1 {
                        failure = Some("init");
                    }
                }
                regs[a] = Val::Target {
                    obj: None,
                    narrow: 0,
                    wide: 0,
                };
            }
            0x07 => {
                regs[a & 15] = regs[a >> 4].clone();
            }
            0x0a..=0x0c => {
                regs[a] = result.clone();
                if op == 0x0b {
                    regs[a + 1] = result.clone();
                }
            }
            0x0d => {
                regs[a] = Val::Object("checked");
            }
            0x1a => {
                width = 2;
                assert_eq!(classes[0].symbols.strings[words[pc + 1] as usize], "é🙂");
                regs[a] = Val::Object("unicode");
            }
            0x12 => {
                regs[a & 15] = Val::Int(((w as i16) >> 12) as i64);
            }
            0x70 | 0x71 => {
                width = 3;
                let index = words[pc + 1] as usize;
                let list = words[pc + 2];
                let count = ((w >> 12) & 15) as usize;
                let mut args = Vec::new();
                for n in 0..count {
                    let reg = if n == 4 {
                        (w >> 8) & 15
                    } else {
                        (list >> (4 * n)) & 15
                    };
                    args.push(regs[reg as usize].clone());
                }
                match index {
                    1 => {
                        trace.push('V');
                        if mode == 2 {
                            failure = Some("identity");
                        }
                    }
                    2 | 9 => {
                        trace.push('A');
                        if mode == 3 {
                            failure = Some(if index == 9 { "checked" } else { "identity" });
                        }
                        result = Val::Int(9_007_199_254_740_993);
                    }
                    8 => {
                        trace.push('B');
                        result = Val::Int(byte);
                    }
                    0 | 7 => {
                        trace.push('T');
                        if mode == 4 {
                            failure = Some("identity");
                        }
                        let wide = match args[2] {
                            Val::Int(n) => n,
                            _ => panic!("raw wide undefined"),
                        };
                        let (obj, narrow) = if index == 7 {
                            (
                                None,
                                match args[1] {
                                    Val::Int(n) => n,
                                    _ => panic!(),
                                },
                            )
                        } else {
                            (
                                Some(match args[1] {
                                    Val::Object(s) => s,
                                    _ => panic!(),
                                }),
                                0,
                            )
                        };
                        regs[(list & 15) as usize] = Val::Target { obj, narrow, wide };
                    }
                    15 => {
                        trace.push('U');
                        assert_eq!(args[0], Val::Object("unicode"));
                    }
                    6 => {
                        trace.push('O');
                        assert_eq!(args[0], input.map_or(Val::Object("null"), Val::Object));
                        assert_eq!(args[1], Val::Int(9_007_199_254_740_993));
                    }
                    _ => panic!("unsupported raw oracle invoke {index}"),
                }
            }
            0x11 => {
                let value = &regs[a];
                let suffix = match value {
                    Val::Target { obj, narrow, wide } => {
                        format!("{}:{narrow}:{wide}", obj.unwrap_or("null"))
                    }
                    Val::Int(0) => "null".into(),
                    _ => panic!("raw return {value:?}"),
                };
                return format!("{trace}|{}|{suffix}", if caught { "caught" } else { "ok" });
            }
            _ => panic!("unsupported raw oracle opcode {op:x} at {pc}"),
        }
        if let Some(why) = failure.take() {
            if why == "checked"
                && let Some(region) = code
                    .try_regions
                    .iter()
                    .find(|r| r.start as usize <= pc && pc < (r.end as usize))
            {
                pc = region.catches[0].1 as usize;
                caught = true;
                continue;
            }
            return format!("{trace}|{why}|null");
        }
        pc += width;
    }
    panic!("raw oracle exceeded instruction budget")
}

#[test]
fn decoder_builds_linear_multiarg_and_checked_scope_fixtures() {
    for shape in [
        Shape::ObjectWide,
        Shape::WrapperWide,
        Shape::ShortWide,
        Shape::Checked,
        Shape::Unicode,
        Shape::Nested,
        Shape::Escape,
        Shape::AliasEscape,
        Shape::WideOverlap,
        Shape::WrongReference,
        Shape::ExternalEntry,
    ] {
        let c = fixture(shape);
        let analysis = MethodAnalysis::build(&c[0], &c[0].methods[0]).unwrap();
        assert!(!analysis.ssa().instructions.is_empty(), "{shape:?}");
        if matches!(
            shape,
            Shape::ObjectWide
                | Shape::WrapperWide
                | Shape::ShortWide
                | Shape::Checked
                | Shape::Unicode
        ) {
            assert_eq!(
                oracle(shape, 0, Some("integer"), -128),
                match shape {
                    Shape::ObjectWide | Shape::WrapperWide | Shape::Checked =>
                        "CVATO|ok|integer:0:9007199254740993",
                    Shape::Unicode => "CUVATO|ok|integer:0:9007199254740993",
                    _ => "CVBAT|ok|null:-128:9007199254740993",
                }
            );
        }
    }
}
#[test]
fn final_public_renderer_preserves_new_argument_block_and_navigation() {
    for shape in [
        Shape::ObjectWide,
        Shape::WrapperWide,
        Shape::ShortWide,
        Shape::Checked,
        Shape::Unicode,
    ] {
        let c = fixture(shape);
        let emitted = native_java::render_method("sample.Caller", &c[0], &c[0].methods[0]).unwrap();
        assert!(
            emitted.source.contains("switch (0)"),
            "{shape:?}: {}",
            emitted.source
        );
        assert!(
            emitted.source.find("new ").unwrap() < emitted.source.find("Effects.prefix").unwrap(),
            "{}",
            emitted.source
        );
        assert_eq!(emitted.source.matches("Effects.prefix").count(), 1);
        let chars: Vec<char> = emitted.source.chars().collect();
        for link in &emitted.links {
            assert!(
                link.start < link.end && link.end <= chars.len(),
                "invalid span {link:?}"
            );
        }
        let prefix = emitted
            .links
            .iter()
            .find(|l| l.label == "sample.Effects.prefix()V")
            .unwrap();
        assert_eq!(
            chars[prefix.start..prefix.end].iter().collect::<String>(),
            "prefix"
        );
        let ctor = emitted
            .links
            .iter()
            .find(|l| {
                l.label
                    == if matches!(shape, Shape::ShortWide) {
                        "sample.Target.<init>(SJ)V"
                    } else {
                        "sample.Target.<init>(Ljava/lang/Object;J)V"
                    }
            })
            .unwrap();
        assert!(
            chars[ctor.start..ctor.end]
                .iter()
                .collect::<String>()
                .ends_with("Target")
        );
        assert!(emitted.links.iter().any(|l| l.label
            == if matches!(shape, Shape::ShortWide) {
                "sample.Target.<init>(SJ)V"
            } else {
                "sample.Target.<init>(Ljava/lang/Object;J)V"
            }));
    }
}
#[test]
fn unsafe_domains_and_nested_staging_outside_first_tranche_remain_blocked() {
    for shape in [
        Shape::Nested,
        Shape::Escape,
        Shape::AliasEscape,
        Shape::WideOverlap,
        Shape::WrongReference,
        Shape::ExternalEntry,
    ] {
        let c = fixture(shape);
        MethodAnalysis::build(&c[0], &c[0].methods[0]).unwrap();
        assert!(
            native_java::render_method("sample.Caller", &c[0], &c[0].methods[0]).is_err(),
            "{shape:?}"
        );
    }
}
#[test]
fn partial_checked_handler_boundary_is_not_extended_by_argument_block() {
    let mut c = fixture(Shape::Checked);
    let code = c[0].methods[0].code.as_mut().unwrap();
    // Exclude allocation and prefix from the checked handler while retaining the
    // checked call and ctor inside. The first tranche forbids boundary crossing.
    code.try_regions[0].start = 5;
    MethodAnalysis::build(&c[0], &c[0].methods[0]).unwrap();
    assert!(native_java::render_method("sample.Caller", &c[0], &c[0].methods[0]).is_err());
}
#[test]
#[ignore = "requires JDK25; run against candidate snapshot"]
fn jvm_exact_raw_words_preserve_init_void_argument_constructor_and_catch_order() {
    let executable = |name: &str| {
        std::env::var_os("RDX_JAVA25_HOME")
            .map(|home| std::path::PathBuf::from(home).join("bin").join(name))
            .unwrap_or_else(|| std::path::PathBuf::from(name))
    };
    let java_command = executable("java");
    let javac_command = executable("javac");
    let run_id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "rdx-allocation-argument-block-{}-{run_id}",
        std::process::id()
    ));
    for (index, shape) in [
        Shape::ObjectWide,
        Shape::WrapperWide,
        Shape::ShortWide,
        Shape::Checked,
        Shape::Unicode,
    ]
    .into_iter()
    .enumerate()
    {
        let classes = fixture(shape);
        let emitted =
            native_java::render_method("sample.Caller", &classes[0], &classes[0].methods[0])
                .unwrap();
        let dir = root.join(index.to_string());
        std::fs::create_dir_all(dir.join("sample")).unwrap();
        let file = dir.join("sample/Caller.java");
        std::fs::write(&file,format!(r#"package sample;
public class Caller {{
{}
 public static void main(String[] a) {{ Effects.mode=Integer.parseInt(a[0]);Effects.narrow=Byte.parseByte(a[2]);
 Object input=a[1].equals("null")?null:a[1].equals("integer")?(Object)Integer.valueOf(999):(Object)Double.valueOf(3.5);
 Effects.expectedInput=input;Target target=null;String outcome="ok";
 try {{target=run({});if(target==null)outcome="caught";}} catch(Throwable e) {{
 if(e instanceof ExceptionInInitializerError && e.getCause()==Effects.failure) outcome="init";
 else if(e==Effects.failure)outcome="identity";else if(e==Effects.checked)outcome="checked";else throw new AssertionError(e);}}
 String value=target==null?"null":(target.obj==null?"null":target.obj==input?a[1]:"WRONG_IDENTITY")+":"+target.narrow+":"+target.wideField;
 System.out.print(Effects.trace+"|"+outcome+"|"+value);
 }}
}}
class Target {{
 static {{Effects.init();}} Object obj;short narrow;long wideField;
 Target(Object o,long w){{obj=o;wideField=w;Effects.constructed();}}
 Target(short n,long w){{narrow=n;wideField=w;Effects.constructed();}}
 Target(Integer o,long w){{Effects.wrong();}}
 Target(Object o,double w){{Effects.wrong();}}
 Target(byte n,long w){{Effects.wrong();}}
}}
class Effects {{
 static int mode;static byte narrow;static Object expectedInput;static String trace="";
 static final RuntimeException failure=new RuntimeException("identity");
 static final java.io.IOException checked=new java.io.IOException("checked identity");
 static void step(String s,int fault){{trace+=s;if(mode==fault)throw failure;}}
 static void init(){{step("C",1);}}static void prefix(){{step("V",2);}}
 static long wide(){{step("A",3);return 9007199254740993L;}}
 static long checkedWide()throws java.io.IOException{{trace+="A";if(mode==3)throw checked;return 9007199254740993L;}}
 static byte byteValue(){{trace+="B";return narrow;}}static void constructed(){{step("T",4);}}
 static void text(String s){{if(!s.equals("é🙂"))throw new AssertionError("unicode");trace+="U";}}
 static void wrong(){{throw new AssertionError("wrong overload");}}
 static void observe(Object o,long w){{trace+="O";if(o!=expectedInput)throw new AssertionError("reference liveout");if(w!=9007199254740993L)throw new AssertionError("wide liveout");}}
}}
"#,emitted.source,if matches!(shape,Shape::WrapperWide) {"(Integer) input"} else {"input"})).unwrap();
        let compile = Command::new(&javac_command).arg(&file).output().unwrap();
        assert!(
            compile.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&compile.stderr),
            emitted.source
        );
        for mode in 0..=4 {
            for input in [None, Some("integer"), Some("double")] {
                for byte in [-128, -1, 0, 127] {
                    if matches!(shape, Shape::WrapperWide) && input == Some("double") {
                        continue;
                    }
                    let expected = oracle(shape, mode, input, byte);
                    let run = Command::new(&java_command)
                        .args([
                            "-Xverify:all",
                            "-cp",
                            dir.to_str().unwrap(),
                            "sample.Caller",
                            &mode.to_string(),
                            input.unwrap_or("null"),
                            &byte.to_string(),
                        ])
                        .output()
                        .unwrap();
                    assert!(
                        run.status.success(),
                        "{}",
                        String::from_utf8_lossy(&run.stderr)
                    );
                    assert_eq!(
                        String::from_utf8(run.stdout).unwrap(),
                        expected,
                        "{shape:?} mode={mode} input={input:?} byte={byte}"
                    );
                }
            }
        }
    }
}

pub use rdx::native_dex;
pub use rdx::{native_cfg, native_dominators, native_ir};
#[path = "../src/native_java/forward_iteration_entry.rs"]
mod forward_iteration_entry;
#[path = "../src/native_java/loop_entry_copy.rs"]
mod original_loop_entry_copy;
#[path = "../src/native_java/single_guard_entry.rs"]
mod single_guard_entry;

fn clone_symbols(classes: &[DexClass]) -> DexSymbols {
    let s = &classes[0].symbols;
    DexSymbols {
        types: s.types.clone(),
        strings: s.strings.clone(),
        protos: s.protos.clone(),
        methods: s.methods.clone(),
        fields: s.fields.clone(),
        ..Default::default()
    }
}
fn install_symbols(classes: &mut [DexClass], symbols: DexSymbols) {
    let s = Arc::new(symbols);
    for c in classes.iter_mut() {
        c.symbols = s.clone();
    }
    s.hierarchy
        .set(Arc::new(
            TypeHierarchy::from_classes(classes.iter()).unwrap(),
        ))
        .unwrap();
}
fn pad_before_new(classes: &mut [DexClass]) {
    let code = classes[0].methods[0].code.as_mut().unwrap();
    let mut padded = vec![0; 4096];
    padded.append(&mut code.instructions);
    code.instructions = padded;
}
#[test]
fn long_methods_and_outside_object_inputs_cannot_bypass_original_argument_domains() {
    for outside in [false, true] {
        for long in [false, true] {
            let mut c = fixture(Shape::WrongReference);
            if outside {
                let w = &mut c[0].methods[0].code.as_mut().unwrap().instructions;
                let pc = w.iter().position(|x| *x == 0x010c).unwrap() - 3;
                w.splice(pc..pc + 4, [0x5107]);
            }
            if long {
                pad_before_new(&mut c);
            }
            MethodAnalysis::build(&c[0], &c[0].methods[0]).unwrap();
            assert!(
                native_java::render_method("sample.Caller", &c[0], &c[0].methods[0]).is_err(),
                "outside={outside} long={long}"
            );
        }
    }
}
fn receiver_fixture(cast: bool, long: bool) -> Vec<DexClass> {
    let mut c = fixture(Shape::ObjectWide);
    let mut s = clone_symbols(&c);
    let name = s.strings.len() as u32;
    s.strings.push("printStackTrace".into());
    let call = s.methods.len() as u16;
    s.methods.push((5, 1, name));
    let words = &mut c[0].methods[0].code.as_mut().unwrap().instructions;
    let mut prefix = vec![];
    if cast {
        prefix.extend([0x051f, 5]);
    }
    prefix.extend([0x106e, call, 5]);
    words.splice(5..5, prefix);
    install_symbols(&mut c, s);
    if long {
        pad_before_new(&mut c);
    }
    c
}
#[test]
fn input_receivers_require_original_specific_receiver_type() {
    for long in [false, true] {
        let c = receiver_fixture(false, long);
        MethodAnalysis::build(&c[0], &c[0].methods[0]).unwrap();
        let code = native_java::render_method("sample.Caller", &c[0], &c[0].methods[0]);
        assert!(
            code.is_err(),
            "long={long}: {}",
            code.map(|c| c.source).unwrap_or_default()
        );
    }
}
#[test]
fn original_check_cast_preserves_exact_constructor_and_receiver_types() {
    for long in [false, true] {
        let mut c = fixture(Shape::WrongReference);
        let w = &mut c[0].methods[0].code.as_mut().unwrap().instructions;
        let pc = w.iter().position(|x| *x == 0x010c).unwrap() + 1;
        w.splice(pc..pc, [0x011f, 5]);
        if long {
            pad_before_new(&mut c);
        }
        MethodAnalysis::build(&c[0], &c[0].methods[0]).unwrap();
        let code = native_java::render_method("sample.Caller", &c[0], &c[0].methods[0]).unwrap();
        assert!(code.source.contains("RuntimeException"));
        assert!(code.source.find("new ").unwrap() < code.source.find(".obtain()").unwrap());
        let c = receiver_fixture(true, long);
        MethodAnalysis::build(&c[0], &c[0].methods[0]).unwrap();
        let code = native_java::render_method("sample.Caller", &c[0], &c[0].methods[0]).unwrap();
        assert!(code.source.contains("RuntimeException"));
        assert_eq!(code.source.matches(".printStackTrace()").count(), 1);
    }
}
fn marker_storage_fixture(alternatives: usize, copied: bool) -> (Vec<DexClass>, usize) {
    let mut c = fixture(Shape::ObjectWide);
    let mut s = clone_symbols(&c);
    let label = s.strings.len() as u32;
    s.strings.push("log".into());
    let proto = s.protos.len() as u16;
    s.protos.push(("V".into(), vec!["Lsample/Marker;".into()]));
    let log = s.methods.len() as u16;
    s.methods.push((1, proto, label));
    let mut params: Vec<String> = (0..alternatives)
        .map(|n| format!("Lsample/Sub{n};"))
        .collect();
    let mut words = vec![];
    if copied {
        params.push("Z".into());
        // Identical external interior-loop entry shape to the production copy proof.
        words = vec![
            0x0408, 7, 0x0512, 0x3612, 0x0938, 6, 0x0408, 8, 0x0029, 4, 0x6535, 9, 0x05d8, 0x0105,
            0x0071, 1, 0, 0x0029, 0xfff9,
        ];
    } else {
        params.push("I".into());
        let parameter_start = 7;
        let selector = parameter_start + alternatives;
        words.extend([0x0502, selector as u16]);
        let mut gotos = vec![];
        for n in 0..alternatives - 1 {
            words.push(((n as u16) << 12) | 0x0612);
            let branch = words.len();
            words.extend([0x6533, 6]);
            words.extend([0x0408, (parameter_start + n) as u16]);
            let go = words.len();
            words.extend([0x0029, 0]);
            gotos.push(go);
            assert_eq!(words.len() - branch, 6);
        }
        words.extend([0x0408, (parameter_start + alternatives - 1) as u16]);
        let join = words.len();
        for go in gotos {
            words[go + 1] = (join - go) as u16;
        }
    }
    let new = words.len();
    words.extend([
        0x0022, 0, 0x0071, 1, 0, 0x1071, log, 4, 0x4107, 0x0071, 2, 0, 0x020b, 0x4070, 0, 0x3210,
        0x3071, 6, 0x0321, 0x0011,
    ]);
    let log_pc = new + 5;
    c[0].methods[0].parameters = params.iter().map(|s| s.as_str().into()).collect();
    let code = c[0].methods[0].code.as_mut().unwrap();
    code.ins = params.len() as u16;
    code.registers = 7 + code.ins;
    code.instructions = words;
    let cls = |descriptor: String, interfaces, flags| DexClass {
        descriptor: descriptor.into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces,
        access_flags: flags,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![],
        symbols: c[0].symbols.clone(),
    };
    let mut extras = vec![cls("Lsample/Marker;".into(), vec![], 0x601)];
    for n in 0..alternatives {
        extras.push(cls(
            format!("Lsample/Sub{n};"),
            vec!["Lsample/Marker;".into()],
            1,
        ));
    }
    c.extend(extras);
    install_symbols(&mut c, s);
    (c, log_pc)
}
#[test]
fn complete_original_interface_domains_allow_widened_storage_cast() {
    let (c, pc) = marker_storage_fixture(2, false);
    let analysis = MethodAnalysis::build(&c[0], &c[0].methods[0]).unwrap();
    let types = analysis.infer_types().unwrap();
    let arg = &analysis
        .calls()
        .calls
        .iter()
        .find(|call| call.pc == pc)
        .unwrap()
        .arguments[0];
    assert!(!types.values[arg.words[0]].bounds_truncated);
    assert_eq!(types.values[arg.words[0]].assignment.len(), 2);
    let code = native_java::render_method("sample.Caller", &c[0], &c[0].methods[0]).unwrap();
    assert!(code.source.contains("java.lang.Object"));
    assert!(code.source.contains("((sample.Marker)"));
    assert!(code.source.contains("switch (0)"));
}
#[test]
fn truncated_original_domains_and_copied_loop_domains_do_not_authorize_narrowing() {
    let (c, pc) = marker_storage_fixture(9, false);
    let analysis = MethodAnalysis::build(&c[0], &c[0].methods[0]).unwrap();
    let types = analysis.infer_types().unwrap();
    let arg = &analysis
        .calls()
        .calls
        .iter()
        .find(|call| call.pc == pc)
        .unwrap()
        .arguments[0];
    assert!(types.values[arg.words[0]].bounds_truncated);
    assert!(native_java::render_method("sample.Caller", &c[0], &c[0].methods[0]).is_err());
    let (c, _) = marker_storage_fixture(2, true);
    MethodAnalysis::build(&c[0], &c[0].methods[0]).unwrap();
    let code = c[0].methods[0].code.as_ref().unwrap();
    let plan = original_loop_entry_copy::select(code)
        .expect("fixture must actually select copied loop proof");
    assert!(original_loop_entry_copy::validate(code, &plan));
    assert!(native_java::render_method("sample.Caller", &c[0], &c[0].methods[0]).is_err());
}

#[test]
fn undefined_original_phi_path_cannot_be_defaulted_into_a_reference_input() {
    let (mut c, pc) = marker_storage_fixture(2, false);
    let words = &mut c[0].methods[0].code.as_mut().unwrap().instructions;
    let last = words.iter().rposition(|w| *w == 0x0408).unwrap();
    words[last] = 0;
    words[last + 1] = 0;
    let analysis = MethodAnalysis::build(&c[0], &c[0].methods[0]).unwrap();
    let types = analysis.infer_types().unwrap();
    let arg = &analysis
        .calls()
        .calls
        .iter()
        .find(|call| call.pc == pc)
        .unwrap()
        .arguments[0];
    assert!(
        types.values[arg.words[0]]
            .assignment
            .iter()
            .any(|a| matches!(a, rdx::native_types::AssignmentBound::Undefined))
    );
    assert!(native_java::render_method("sample.Caller", &c[0], &c[0].methods[0]).is_err());
}
#[test]
fn before_new_string_resolution_is_materialized_or_declined() {
    let mut c = fixture(Shape::ObjectWide);
    c[0].methods[0]
        .code
        .as_mut()
        .unwrap()
        .instructions
        .splice(0..0, [0x051a, 16]);
    MethodAnalysis::build(&c[0], &c[0].methods[0]).unwrap();
    if let Ok(code) = native_java::render_method("sample.Caller", &c[0], &c[0].methods[0]) {
        let literal = code
            .source
            .find("é🙂")
            .expect("source must retain original string");
        assert!(
            literal < code.source.find("new ").unwrap(),
            "{}",
            code.source
        );
    }
}

fn incoming_wide_fixture(double: bool, range: bool, corrupt: Option<usize>) -> Vec<DexClass> {
    let mut c = fixture(Shape::ObjectWide);
    let mut symbols = clone_symbols(&c);
    let ctor = if double {
        let index = symbols.methods.len() as u16;
        symbols.methods.push((0, 9, 0));
        index
    } else {
        0
    };
    c[0].methods[0].parameters = vec![
        "Ljava/lang/Object;".into(),
        if double { "D" } else { "J" }.into(),
    ];
    let code = c[0].methods[0].code.as_mut().unwrap();
    code.ins = 3;
    code.registers = if range { 4 } else { 8 };
    let mut words = vec![0x0022, 0, 0x0071, 1, 0];
    if let Some(word) = corrupt {
        words.push((word as u16) << 8 | 0x12);
    }
    if range {
        words.extend([0x0476, ctor, 0]);
    } else {
        words.extend([0x5107, 0x4070, ctor, 0x7610]);
    }
    words.push(0x0011);
    code.instructions = words;
    install_symbols(&mut c, symbols);
    c
}
#[test]
fn original_long_double_heads_and_tails_are_coherent_for_35c_and_range() {
    for double in [false, true] {
        for range in [false, true] {
            let c = incoming_wide_fixture(double, range, None);
            let analysis = MethodAnalysis::build(&c[0], &c[0].methods[0]).unwrap();
            assert_eq!(analysis.infer_types().unwrap().wide_pair_issues, 0);
            let code =
                native_java::render_method("sample.Caller", &c[0], &c[0].methods[0]).unwrap();
            assert!(code.source.contains("switch (0)"), "{}", code.source);
            let descriptor = if double {
                "Ljava/lang/Object;D"
            } else {
                "Ljava/lang/Object;J"
            };
            assert!(
                code.links
                    .iter()
                    .any(|l| l.label == format!("sample.Target.<init>({descriptor})V"))
            );
        }
    }
}
#[test]
fn overwriting_either_incoming_wide_word_never_creates_a_scalar_argument() {
    for double in [false, true] {
        for range in [false, true] {
            for tail in [false, true] {
                let head = if range { 2 } else { 6 };
                let c = incoming_wide_fixture(double, range, Some(head + usize::from(tail)));
                MethodAnalysis::build(&c[0], &c[0].methods[0]).unwrap();
                assert!(
                    native_java::render_method("sample.Caller", &c[0], &c[0].methods[0]).is_err(),
                    "double={double} range={range} tail={tail}"
                );
            }
        }
    }
}
