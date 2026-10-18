pub use rdx::{
    native_calls, native_cfg, native_dex, native_ir, native_method, native_ssa, native_types,
};
#[path = "../src/native_java/constructor_switch_prefix.rs"]
mod prefix;
use native_dex::{DexClass, DexCode, DexMethod, DexSymbols};
use std::sync::Arc;
fn fixture(sparse: bool) -> DexClass {
    let mut words = vec![0x082b, 0, 0];
    if sparse {
        words[0] = 0x082c;
    }
    let mut starts = vec![];
    for n in [3u16, 7] {
        starts.push(words.len());
        words.extend([
            0x0013, n, 0x2071, 1, 0x0005, 0x050c, 0x0801, 0x0576, 0, 4, 0x000e,
        ]);
    }
    if !words.len().is_multiple_of(2) {
        words.push(0);
    }
    let payload = words.len();
    words[1] = payload as u16;
    let target = starts[1] as u16;
    if sparse {
        words.extend([0x0200, 1, 0xfff9, 0xffff, target, 0]);
    } else {
        words.extend([0x0100, 1, 1, 0, target, 0]);
    }
    let c = DexClass {
        descriptor: "Lsample/Child;".into(),
        superclass: Some("Lsample/Base;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            types: vec!["Lsample/Base;".into(), "Lsample/Source;".into()],
            strings: vec!["<init>".into(), "value".into()],
            protos: vec![
                (
                    "V".into(),
                    vec!["Ljava/lang/Object;".into(), "J".into(), "I".into()],
                ),
                (
                    "Ljava/lang/Object;".into(),
                    vec!["Ljava/lang/Object;".into(), "I".into()],
                ),
            ],
            methods: vec![(0, 0, 0), (1, 1, 1)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/Child;".into(),
            name: "<init>".into(),
            parameters: vec!["Ljava/lang/Object;".into(), "J".into(), "I".into()],
            return_type: "V".into(),
            thrown_types: vec![],
            access_flags: 0x10001,
            code: Some(DexCode {
                registers: 9,
                ins: 5,
                outs: 5,
                tries: 0,
                try_regions: vec![],
                instructions: words,
                offset: 0,
            }),
        }],
    };
    c.symbols
        .hierarchy
        .set(Arc::new(
            rdx::native_hierarchy::TypeHierarchy::from_classes([&c]).unwrap(),
        ))
        .unwrap();
    c
}

fn floating_fixture(sparse: bool) -> DexClass {
    let mut class = fixture(sparse);
    class.methods[0].parameters = vec![
        "Ljava/lang/Object;".into(),
        "D".into(),
        "F".into(),
        "I".into(),
    ];
    let code = class.methods[0].code.as_mut().unwrap();
    code.registers = 10;
    code.ins = 6;
    code.instructions[0] = if sparse { 0x092c } else { 0x092b };
    for pc in [9, 20] {
        code.instructions[pc] = 0x0901;
    }
    let arguments = &mut Arc::get_mut(&mut class.symbols).unwrap().protos[0].1;
    arguments[1] = "D".into();
    arguments[2] = "F".into();
    class
}

#[test]
fn floating_carriers_preserve_raw_domains() {
    for sparse in [false, true] {
        let class = floating_fixture(sparse);
        let plan = prefix::select(&class, &class.methods[0], 0).unwrap();
        assert_eq!(plan.arms[0].arguments[1].descriptor.as_ref(), "D");
        assert_eq!(plan.arms[0].arguments[2].descriptor.as_ref(), "F");
        let out =
            rdx::native_java::render_method("sample.Child", &class, &class.methods[0]).unwrap();
        assert!(out.source.contains("double"));
        assert!(out.source.contains("float"));
    }
}

fn floating_suffix_fixture() -> DexClass {
    let mut class = floating_fixture(false);
    class.fields.push(native_dex::DexField {
        declaring_type: class.descriptor.clone(),
        name: "result".into(),
        field_type: "I".into(),
        access_flags: 0x11,
        is_static: false,
    });
    let symbols = Arc::get_mut(&mut class.symbols).unwrap();
    symbols.types.extend([class.descriptor.clone(), "I".into()]);
    symbols.strings.push("result".into());
    symbols.fields.push((2, 3, 2));
    let old = class.methods[0].code.as_ref().unwrap().instructions.clone();
    let mut words = old[..3].to_vec();
    let mut starts = vec![];
    for start in [3, 14] {
        starts.push(words.len());
        words.extend_from_slice(&old[start..start + 10]);
        words.extend([0x4959, 0, 0x000e]);
    }
    if !words.len().is_multiple_of(2) {
        words.push(0);
    }
    words[1] = words.len() as u16;
    words.extend([0x0100, 1, 1, 0, starts[1] as u16, 0]);
    class.methods[0].code.as_mut().unwrap().instructions = words;
    class
}

#[test]
fn suffix_keeps_original_integer_after_float_argument_capture() {
    let class = floating_suffix_fixture();
    let plan = prefix::select(&class, &class.methods[0], 0).unwrap();
    assert!(plan.separate_suffixes);
    let out = rdx::native_java::render_method("sample.Child", &class, &class.methods[0]).unwrap();
    assert!(out.source.contains("this.result = 3;"));
    assert!(out.source.contains("this.result = 7;"));
}

#[test]
fn repeated_raw_register_with_distinct_domains_is_rejected() {
    let mut class = fixture(false);
    Arc::get_mut(&mut class.symbols).unwrap().protos[0].1 = vec!["I".into(), "F".into()];
    let code = class.methods[0].code.as_mut().unwrap();
    for pc in [10, 21] {
        code.instructions[pc..pc + 3].copy_from_slice(&[0x3070, 0, 0x0004]);
    }
    let analysis = native_method::MethodAnalysis::build(&class, &class.methods[0]).unwrap();
    assert_eq!(
        analysis
            .calls()
            .calls
            .iter()
            .filter(|c| matches!(c.pc, 10 | 21))
            .count(),
        2
    );
    assert!(prefix::select(&class, &class.methods[0], 0).is_none());
    assert!(rdx::native_java::render_method("sample.Child", &class, &class.methods[0]).is_err());
}

#[test]
fn repeated_same_type_argument_register_is_conservatively_rejected() {
    let mut class = fixture(false);
    Arc::get_mut(&mut class.symbols).unwrap().protos[0].1 = vec!["I".into(), "I".into()];
    for pc in [10, 21] {
        class.methods[0].code.as_mut().unwrap().instructions[pc..pc + 3]
            .copy_from_slice(&[0x3070, 0, 0x0004]);
    }
    let analysis = native_method::MethodAnalysis::build(&class, &class.methods[0]).unwrap();
    assert_eq!(
        analysis
            .calls()
            .calls
            .iter()
            .filter(|call| matches!(call.pc, 10 | 21))
            .count(),
        2
    );
    assert!(prefix::select(&class, &class.methods[0], 0).is_none());
    assert!(rdx::native_java::render_method("sample.Child", &class, &class.methods[0]).is_err());
}

#[test]
fn overlapping_wide_argument_pairs_are_rejected() {
    let mut class = fixture(false);
    Arc::get_mut(&mut class.symbols).unwrap().protos[0].1 = vec!["J".into(), "D".into()];
    let code = class.methods[0].code.as_mut().unwrap();
    for pc in [10, 21] {
        code.instructions[pc..pc + 3].copy_from_slice(&[0x5070, 0, 0x6764]);
        // fifth invoke word is the second v7 tail.
        code.instructions[pc] |= 0x0700;
    }
    let analysis = native_method::MethodAnalysis::build(&class, &class.methods[0]).unwrap();
    assert_eq!(
        analysis
            .calls()
            .calls
            .iter()
            .filter(|c| matches!(c.pc, 10 | 21))
            .count(),
        2
    );
    assert!(prefix::select(&class, &class.methods[0], 0).is_none());
    assert!(rdx::native_java::render_method("sample.Child", &class, &class.methods[0]).is_err());
}
#[test]
fn typed_parallel_carriers_keep_exact_reads_and_navigation() {
    for sparse in [false, true] {
        let c = fixture(sparse);
        let p = prefix::select(&c, &c.methods[0], 0).unwrap();
        assert_eq!(p.arms.len(), 2);
        assert_eq!(
            p.arms[0]
                .arguments
                .iter()
                .map(|a| a.register)
                .collect::<Vec<_>>(),
            [5, 6, 8]
        );
        let out = rdx::native_java::render_method("sample.Child", &c, &c.methods[0]).unwrap();
        assert!(out.source.contains("super("));
        assert!(out.source.contains("long"));
        assert!(
            out.links
                .iter()
                .any(|l| l.label == "sample.Base.<init>(Ljava/lang/Object;JI)V")
        );
        assert!(
            out.links
                .iter()
                .any(|l| l.label == "sample.Source.value(Ljava/lang/Object;I)Ljava/lang/Object;")
        );
    }
}
#[test]
fn receiver_argument_is_rejected() {
    let mut c = fixture(false);
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions[8] = 0x040c;
    assert!(prefix::select(&c, &c.methods[0], 0).is_none());
}
#[test]
fn distinct_parent_prototypes_are_rejected() {
    let mut c = fixture(false);
    Arc::get_mut(&mut c.symbols)
        .unwrap()
        .methods
        .push((0, 1, 0));
    c.methods[0].code.as_mut().unwrap().instructions[22] = 2;
    assert!(prefix::select(&c, &c.methods[0], 0).is_none());
}
#[test]
fn size_budget_is_retained() {
    let mut c = fixture(false);
    c.methods[0]
        .code
        .as_mut()
        .unwrap()
        .instructions
        .resize(4097, 0);
    assert!(prefix::select(&c, &c.methods[0], 0).is_none());
}
#[test]
fn foreign_entry_is_rejected() {
    let mut c = fixture(false);
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions.splice(0..0, [0x0838, 12]);
    native_method::MethodAnalysis::build(&c, &c.methods[0]).unwrap();
    assert!(prefix::select(&c, &c.methods[0], 2).is_none());
}
#[test]
fn argument_budget_rejects_a_well_bound_call() {
    let mut c = fixture(false);
    Arc::get_mut(&mut c.symbols).unwrap().protos[0].1 = vec!["I".into(); 17];
    let method = &mut c.methods[0];
    method.parameters = vec!["I".into(); 17];
    let code = method.code.as_mut().unwrap();
    code.registers = 22;
    code.ins = 18;
    code.outs = 18;
    code.instructions[0] = 0x152b;
    for start in [3, 14] {
        code.instructions[start + 2..start + 5].fill(0);
        code.instructions[start + 5] = 0x0501;
        code.instructions[start + 7] = 0x1276;
    }
    let analysis = native_method::MethodAnalysis::build(&c, &c.methods[0]).unwrap();
    assert_eq!(analysis.calls().calls[0].arguments.len(), 17);
    assert!(prefix::select(&c, &c.methods[0], 0).is_none());
}
#[test]
#[ignore = "requires Java 25"]
fn jvm_argument_values_aliases_wide_values_and_fault_order() {
    use std::{fs, process::Command};
    let home = std::env::var("RDX_JAVA25_HOME").unwrap();
    for sparse in [false, true] {
        let c = fixture(sparse);
        let out = rdx::native_java::render_method("sample.Child", &c, &c.methods[0]).unwrap();
        let d = std::env::temp_dir().join(format!(
            "rdx-switch-arguments-{}-{sparse}",
            std::process::id()
        ));
        fs::create_dir_all(d.join("sample")).unwrap();
        let source = r#"package sample;
 class State {static String trace;static int fail;static Object replacement;static final RuntimeException marker=new RuntimeException();}
 class Source {static Object value(Object input,int n){State.trace+="A"+n; if(State.fail==1)throw State.marker;return State.replacement==null?input:State.replacement;}}
 class Base {Object object;long wide;int number;Base(Object o,long j,int n){State.trace+="B"+n;if(State.fail==2)throw State.marker;object=o;wide=j;number=n;}}
 public class Child extends Base {
 METHOD
 static void verify(int selector,long j,int fail,boolean replace,boolean present){Object input=present?new Object():null;State.replacement=replace?new Object():null;State.fail=fail;State.trace="";int n=CASE;try{Child c=new Child(input,j,selector);if(fail!=0||c.object!=(replace?State.replacement:input)||c.wide!=j||c.number!=n)throw new AssertionError();}catch(RuntimeException e){if(fail==0||e!=State.marker)throw new AssertionError();}String want="A"+n+(fail==1?"":"B"+n);if(!State.trace.equals(want))throw new AssertionError(State.trace+" != "+want);}
 public static void main(String[] args){for(int s:new int[]{Integer.MIN_VALUE,-7,0,1,17,Integer.MAX_VALUE})for(long j:new long[]{Long.MIN_VALUE,-1,0,1,Long.MAX_VALUE})for(int f=0;f<3;f++)for(boolean r:new boolean[]{false,true})for(boolean p:new boolean[]{false,true})verify(s,j,f,r,p);}}
 "#;
        fs::write(
            d.join("sample/Child.java"),
            source.replace("METHOD", &out.source).replace(
                "CASE",
                if sparse {
                    "selector==-7?7:3"
                } else {
                    "selector==1?7:3"
                },
            ),
        )
        .unwrap();
        for (program, args) in [
            ("javac", vec!["sample/Child.java"]),
            ("java", vec!["-cp", ".", "sample.Child"]),
        ] {
            let result = Command::new(format!("{home}/bin/{program}"))
                .args(args)
                .current_dir(&d)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
        fs::remove_dir_all(d).unwrap();
    }
}
#[test]
#[ignore = "requires Java 25"]
fn fresh_loader_class_initialization_and_error_identity() {
    use std::{fs, process::Command};
    let home = std::env::var("RDX_JAVA25_HOME").unwrap();
    for sparse in [false, true] {
        let c = fixture(sparse);
        let out = rdx::native_java::render_method("sample.Child", &c, &c.methods[0]).unwrap();
        let d = std::env::temp_dir().join(format!(
            "rdx-switch-argument-loaders-{}-{sparse}",
            std::process::id()
        ));
        fs::create_dir_all(d.join("sample")).unwrap();
        fs::write(d.join("sample/Child.java"),format!("package sample; public class Child extends Base {{ static {{State.event(\"C\",5);}} {} }}",out.source)).unwrap();
        let driver = r#"package sample;
 class State {static String trace="";static int fail;static boolean error;static Object replacement;static final RuntimeException runtime=new RuntimeException();static final Error fatal=new Error();static void event(String s,int n){trace+=s;if(fail==n){if(error)throw fatal;throw runtime;}}}
 class Base {static {State.event("P",3);} Object object;long wide;int number;Base(Object o,long j,int n){State.event("B"+n,2);object=o;wide=j;number=n;}}
 class Source {static {State.event("S",4);} static Object value(Object o,int n){State.event("A"+n,1);return State.replacement==null?o:State.replacement;}}
 public class Driver {public static void run(int selector,long j,int fail,boolean error,boolean replace,boolean present){State.fail=fail;State.error=error;Object input=present?new Object():null;State.replacement=replace?new Object():null;int n=CASE;try {Child c=new Child(input,j,selector);if(fail!=0||c.object!=(replace?State.replacement:input)||c.wide!=j||c.number!=n)throw new AssertionError("value");}catch(Throwable e){Throwable actual=e instanceof ExceptionInInitializerError?e.getCause():e;if(fail==0||actual!=(error?State.fatal:State.runtime))throw new AssertionError("identity",e);}String want="P"+(fail==3?"":"C"+(fail==5?"":"S"+(fail==4?"":"A"+n+(fail==1?"":"B"+n))));if(!State.trace.equals(want))throw new AssertionError(State.trace+" != "+want);}}
 "#;
        fs::write(
            d.join("sample/Driver.java"),
            driver.replace(
                "CASE",
                if sparse {
                    "selector==-7?7:3"
                } else {
                    "selector==1?7:3"
                },
            ),
        )
        .unwrap();
        fs::write(d.join("Host.java"),r#"import java.net.*;import java.nio.file.*;public class Host {public static void main(String[] args)throws Exception{for(int s:new int[]{Integer.MIN_VALUE,-7,0,1,17,Integer.MAX_VALUE})for(long j:new long[]{Long.MIN_VALUE,-1,0,1,Long.MAX_VALUE})for(int f=0;f<6;f++)for(boolean e:new boolean[]{false,true})for(boolean r:new boolean[]{false,true})for(boolean p:new boolean[]{false,true})try(URLClassLoader loader=new URLClassLoader(new URL[]{Path.of(".").toUri().toURL()},ClassLoader.getPlatformClassLoader())){try{Class.forName("sample.Driver",true,loader).getMethod("run",int.class,long.class,int.class,boolean.class,boolean.class,boolean.class).invoke(null,s,j,f,e,r,p);}catch(java.lang.reflect.InvocationTargetException x){throw new AssertionError(x.getCause());}}}}"#).unwrap();
        for (program, args) in [
            (
                "javac",
                vec!["sample/Child.java", "sample/Driver.java", "Host.java"],
            ),
            ("java", vec!["-cp", ".", "Host"]),
        ] {
            let result = Command::new(format!("{home}/bin/{program}"))
                .args(args)
                .current_dir(&d)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
        fs::remove_dir_all(d).unwrap();
    }
}

#[test]
#[ignore = "requires Java 25"]
fn jvm_raw_float_double_and_original_final_suffix() {
    use std::{fs, process::Command};
    let home = std::env::var("RDX_JAVA25_HOME").unwrap();
    let class = floating_suffix_fixture();
    let out = rdx::native_java::render_method("sample.Child", &class, &class.methods[0]).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-switch-floating-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let source = r#"package sample;
 class State {static int fail;static String trace;static final RuntimeException marker=new RuntimeException();}
 class Source {static Object value(Object o,int n){State.trace+="A"+n;if(State.fail==1)throw State.marker;return o;}}
 class Base {Object object;double wide;float number;Base(Object o,double j,float n){State.trace+="B"+Float.floatToRawIntBits(n);if(State.fail==2)throw State.marker;object=o;wide=j;number=n;}}
 public class Child extends Base {public final int result;
 METHOD
 static void verify(int s,long bits,int fail,boolean present){State.trace="";State.fail=fail;Object input=present?new Object():null;int n=s==1?7:3;try{Child c=new Child(input,Double.longBitsToDouble(bits),Float.intBitsToFloat(n),s);if(fail!=0||c.object!=input||Double.doubleToRawLongBits(c.wide)!=bits||Float.floatToRawIntBits(c.number)!=n||c.result!=n)throw new AssertionError("value");}catch(RuntimeException e){if(fail==0||e!=State.marker)throw new AssertionError("identity");}String want="A"+n+(fail==1?"":"B"+n);if(!State.trace.equals(want))throw new AssertionError(State.trace+" != "+want);}
 public static void main(String[]args){for(int s:new int[]{Integer.MIN_VALUE,-7,0,1,17,Integer.MAX_VALUE})for(long b:new long[]{Long.MIN_VALUE,-1,0,1,Long.MAX_VALUE,0x7ff0000000000000L,0xfff0000000000000L,0x7ff8000000001234L})for(int f=0;f<3;f++)for(boolean p:new boolean[]{false,true})verify(s,b,f,p);}}
 "#;
    fs::write(
        dir.join("sample/Child.java"),
        source.replace("METHOD", &out.source),
    )
    .unwrap();
    for (program, args) in [
        ("javac", vec!["sample/Child.java"]),
        ("java", vec!["-cp", ".", "sample.Child"]),
    ] {
        let result = Command::new(format!("{home}/bin/{program}"))
            .args(args)
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

#[test]
fn root_prelude_string_resolution_order_probe() {
    let mut c = fixture(false);
    let symbols = Arc::get_mut(&mut c.symbols).unwrap();
    symbols
        .strings
        .extend(["resolved-before-ping".into(), "ping".into()]);
    symbols.protos.push(("V".into(), vec![]));
    symbols.methods.push((1, 2, 3));
    let code = c.methods[0].code.as_mut().unwrap();
    let mut words = vec![0x031a, 2, 0x0071, 2, 0, 0];
    words.extend_from_slice(&code.instructions);
    code.instructions = words;
    assert!(prefix::select(&c, &c.methods[0], 6).is_some());
    let out = rdx::native_java::render_method("sample.Child", &c, &c.methods[0]).unwrap();
    println!("{}", out.source);
    assert!(
        out.source
            .find("resolved-before-ping")
            .is_some_and(|position| position < out.source.find("sample.Source.ping()").unwrap())
    );
}

#[test]
fn live_and_dead_prelude_strings_survive_full_class_cleanup() {
    for sparse in [false, true] {
        for live in [false, true] {
            let mut c = fixture(sparse);
            let symbols = Arc::get_mut(&mut c.symbols).unwrap();
            symbols
                .strings
                .extend(["resolved-before-ping".into(), "ping".into()]);
            symbols.protos.push(("V".into(), vec![]));
            symbols.methods.push((1, 2, 3));
            let code = c.methods[0].code.as_mut().unwrap();
            let mut words = vec![if live { 0x051a } else { 0x031a }, 2, 0x0071, 2, 0, 0];
            words.extend_from_slice(&code.instructions);
            code.instructions = words;
            for out in [
                rdx::native_java::render_method("sample.Child", &c, &c.methods[0]).unwrap(),
                rdx::native_java::render("sample.Child", &c).unwrap(),
            ] {
                let literal = out.source.find("resolved-before-ping").unwrap();
                assert!(
                    literal < out.source.find(".ping()").unwrap(),
                    "{}",
                    out.source
                );
                assert!(out.source.find(".ping()").unwrap() < out.source.find("switch (").unwrap());
                assert!(out.source.find("switch (").unwrap() < out.source.find("super(").unwrap());
                assert_eq!(out.source.matches("resolved-before-ping").count(), 1);
            }
        }
    }
}

#[test]
fn linear_prelude_strings_and_nested_allocation_keep_producer_order() {
    for live in [false, true] {
        for nested in [false, true] {
            let mut c = fixture(false);
            let sy = Arc::get_mut(&mut c.symbols).unwrap();
            sy.strings
                .extend(["resolved-linear-before-ping".into(), "ping".into()]);
            sy.protos.push(("V".into(), vec![]));
            sy.methods.push((1, 2, 3));
            sy.types.push("Ljava/lang/Object;".into());
            sy.methods.push((2, 2, 0));
            let mut words = vec![if live { 0x051a } else { 0x031a }, 2, 0x0071, 2, 0];
            if nested {
                words.extend([0x0022, 2, 0x1070, 3, 0]);
            }
            words.extend([0x0576, 0, 4, 0x000e]);
            c.methods[0].code.as_mut().unwrap().instructions = words;
            for out in [
                rdx::native_java::render_method("sample.Child", &c, &c.methods[0]).unwrap(),
                rdx::native_java::render("sample.Child", &c).unwrap(),
            ] {
                let a = out.source.find("resolved-linear-before-ping").unwrap();
                let b = out.source.find(".ping()").unwrap();
                let z = out.source.find("super(").unwrap();
                assert!(a < b && b < z, "{}", out.source);
                if nested {
                    let n = out.source.find("new ").unwrap();
                    assert!(b < n && n < z, "{}", out.source);
                }
                assert_eq!(out.source.matches("resolved-linear-before-ping").count(), 1);
            }
        }
    }
}
