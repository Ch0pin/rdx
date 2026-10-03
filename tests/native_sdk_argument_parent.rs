use rdx::{
    native_dex::{DexClass, DexCode, DexField, DexMethod, DexSymbols},
    native_hierarchy::TypeHierarchy,
    native_java,
    native_method::MethodAnalysis,
};
use std::{fs, process::Command, sync::Arc};
fn class(owner: &str, parent: &str) -> DexClass {
    DexClass {
        descriptor: owner.into(),
        superclass: Some(parent.into()),
        access_flags: 1,
        interfaces: vec![],
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![],
        symbols: Arc::new(DexSymbols::default()),
    }
}
fn method(
    owner: &str,
    name: &str,
    ret: &str,
    args: &[&str],
    registers: u16,
    ins: u16,
    words: &[u16],
) -> DexMethod {
    DexMethod {
        declaring_type: owner.into(),
        name: name.into(),
        return_type: ret.into(),
        parameters: args.iter().map(|p| Arc::from(*p)).collect(),
        thrown_types: vec![],
        access_flags: if name == "<init>" { 0x10001 } else { 9 },
        code: Some(DexCode {
            registers,
            ins,
            outs: registers,
            tries: 0,
            try_regions: vec![],
            instructions: words.to_vec(),
            offset: 0,
        }),
    }
}
fn fixture() -> Vec<DexClass> {
    let mut leaf = class("Lsample/Leaf;", "Landroid/view/View;");
    leaf.symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Hooks;".into()],
        strings: vec!["leaf".into()],
        protos: vec![("V".into(), vec![])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    leaf.methods.push(method(
        "Lsample/Leaf;",
        "<clinit>",
        "V",
        &[],
        0,
        0,
        &[0x0071, 0, 0, 0x000e],
    ));
    let mut factory = class("Lsample/Factory;", "Ljava/lang/Object;");
    factory.symbols = Arc::new(DexSymbols {
        types: vec![
            "Lsample/Leaf;".into(),
            "Landroid/view/View;".into(),
            "Lsample/Hooks;".into(),
        ],
        strings: vec!["arg".into(), "<init>".into(), "after".into()],
        protos: vec![
            (
                "Landroid/content/Context;".into(),
                vec!["Landroid/content/Context;".into()],
            ),
            ("V".into(), vec!["Landroid/content/Context;".into()]),
            ("V".into(), vec![]),
        ],
        methods: vec![(2, 0, 0), (1, 1, 1), (2, 2, 2)],
        ..Default::default()
    });
    factory.methods.push(method(
        "Lsample/Factory;",
        "run",
        "Lsample/Leaf;",
        &["Landroid/content/Context;"],
        3,
        1,
        &[
            0x0022, 0, 0x1071, 0, 2, 0x010c, 0x2070, 1, 0x0010, 0x0071, 2, 0, 0x0011,
        ],
    ));
    vec![leaf, factory]
}
fn bind(classes: &[DexClass]) -> Arc<TypeHierarchy> {
    let hierarchy = Arc::new(TypeHierarchy::from_classes(classes.iter()).unwrap());
    for c in classes {
        c.symbols.hierarchy.set(hierarchy.clone()).unwrap();
    }
    hierarchy
}
#[test]
fn argument_only_sdk_parent_restores_only_observed_exact_constructor() {
    let classes = fixture();
    let h = bind(&classes);
    let ctors = h.recovered_constructors("Lsample/Leaf;");
    assert_eq!(ctors.len(), 1);
    assert_eq!(
        ctors[0].parameters,
        [Arc::<str>::from("Landroid/content/Context;")]
    );
    assert!(ctors[0].thrown_types.is_empty());
    assert!(ctors[0].observed_allocation);
    let leaf = native_java::render("sample.Leaf", &classes[0]).unwrap();
    assert!(leaf.source.contains("Leaf(Context p0)"), "{}", leaf.source);
    assert!(!leaf.source.contains("Leaf()"));
    let body =
        native_java::render_method("sample.Factory", &classes[1], &classes[1].methods[0]).unwrap();
    assert!(
        body.source
            .contains("new sample.Leaf(sample.Hooks.arg(p0))"),
        "{}",
        body.source
    );
    assert!(
        body.links
            .iter()
            .any(|link| link.label == "android.view.View.<init>(Landroid/content/Context;)V"),
        "{:?}",
        body.links
    );
}
#[test]
fn missing_signature_final_fields_duplicate_owners_and_loaded_access_decline() {
    for poison in 0..6 {
        let mut classes = fixture();
        match poison {
            0 => {
                let symbols = Arc::get_mut(&mut classes[1].symbols).unwrap();
                symbols.protos[1].1[0] = "Ljava/lang/String;".into();
            }
            1 => classes[0].fields.push(DexField {
                declaring_type: "Lsample/Leaf;".into(),
                name: "finalState".into(),
                field_type: "I".into(),
                access_flags: 0x11,
                is_static: false,
            }),
            2 => classes.push(class("Lsample/Leaf;", "Landroid/view/View;")),
            3 => {
                let mut view = class("Landroid/view/View;", "Ljava/lang/Object;");
                let mut ctor = method(
                    "Landroid/view/View;",
                    "<init>",
                    "V",
                    &["Landroid/content/Context;"],
                    2,
                    2,
                    &[0x000e],
                );
                ctor.access_flags = 0x10002;
                view.methods.push(ctor);
                classes.push(view);
            }
            4 => {
                classes[1].methods[0].parameters.clear();
                classes[1].methods[0].code.as_mut().unwrap().ins = 0;
                classes[1].methods[0].code.as_mut().unwrap().instructions =
                    vec![0x0022, 0, 0x1070, 1, 0, 0x0011];
                Arc::get_mut(&mut classes[1].symbols).unwrap().protos[1]
                    .1
                    .clear();
            }
            5 => {
                let symbols = Arc::get_mut(&mut classes[0].symbols).unwrap();
                symbols.types.push("Landroid/view/View;".into());
                symbols.strings.push("<init>".into());
                symbols
                    .protos
                    .push(("V".into(), vec!["Landroid/content/Context;".into()]));
                symbols.methods.push((1, 1, 1));
                classes[0].methods.push(method(
                    "Lsample/Leaf;",
                    "<init>",
                    "V",
                    &["Landroid/content/Context;"],
                    2,
                    2,
                    &[0x2070, 1, 0x0010, 0x0071, 0, 0, 0x000e],
                ));
            }
            _ => unreachable!(),
        }
        let h = bind(&classes);
        assert!(
            h.recovered_constructors("Lsample/Leaf;").is_empty(),
            "poison{poison}"
        );
        assert!(
            native_java::render_method("sample.Factory", &classes[1], &classes[1].methods[0])
                .is_err(),
            "poison{poison}"
        );
    }
}
#[test]
fn exact_argument_only_sdk_checked_throws_are_retained() {
    let leaf = class("Lsample/Leaf;", "Ljava/io/InputStreamReader;");
    let mut factory = class("Lsample/Factory;", "Ljava/lang/Object;");
    factory.symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Leaf;".into(), "Ljava/io/InputStreamReader;".into()],
        strings: vec!["<init>".into()],
        protos: vec![(
            "V".into(),
            vec!["Ljava/io/InputStream;".into(), "Ljava/lang/String;".into()],
        )],
        methods: vec![(1, 0, 0)],
        ..Default::default()
    });
    factory.methods.push(method(
        "Lsample/Factory;",
        "run",
        "Lsample/Leaf;",
        &["Ljava/io/InputStream;", "Ljava/lang/String;"],
        3,
        2,
        &[0x0022, 0, 0x3070, 0, 0x0210, 0x0011],
    ));
    let classes = vec![leaf, factory];
    let h = bind(&classes);
    assert_eq!(h.recovered_constructors("Lsample/Leaf;").len(), 1);
    assert_eq!(
        h.recovered_constructors("Lsample/Leaf;")[0].thrown_types,
        [Arc::<str>::from("Ljava/io/UnsupportedEncodingException;")]
    );
    let code = native_java::render("sample.Leaf", &classes[0]).unwrap();
    assert!(
        code.source
            .contains("throws java.io.UnsupportedEncodingException"),
        "{}",
        code.source
    );
}
#[test]
#[ignore = "requires RDX_JAVA25_HOME"]
fn unchanged_argument_only_parent_java_preserves_init_argument_and_constructor_faults() {
    let home = std::env::var("RDX_JAVA25_HOME").unwrap();
    let classes = fixture();
    bind(&classes);
    MethodAnalysis::build(&classes[1], &classes[1].methods[0]).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-sdk-argument-parent-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::create_dir_all(dir.join("android/view")).unwrap();
    fs::create_dir_all(dir.join("android/content")).unwrap();
    for (name, c) in ["Leaf", "Factory"].iter().zip(&classes) {
        fs::write(
            dir.join(format!("sample/{name}.java")),
            native_java::render(&format!("sample.{name}"), c)
                .unwrap()
                .source,
        )
        .unwrap();
    }
    fs::write(
        dir.join("android/content/Context.java"),
        "package android.content; public class Context {}",
    )
    .unwrap();
    fs::write(dir.join("android/view/View.java"),"package android.view; public class View { static {sample.Hooks.parent();} public final android.content.Context context; public View(android.content.Context value) {sample.Hooks.base();context=value;} }").unwrap();
    fs::write(dir.join("sample/Hooks.java"),r#"package sample; public class Hooks {public static final RuntimeException fault=new RuntimeException();public static int flags;public static final StringBuilder trace=new StringBuilder();static void event(String name,int flag){if(trace.length()>0)trace.append(',');trace.append(name);if((flags&flag)!=0)throw fault;}public static void parent(){event("parent",1);}public static void leaf(){event("leaf",2);}public static android.content.Context arg(android.content.Context value){event("arg",4);return value;}public static void base(){event("base",8);}public static void after(){event("after",0);}}"#).unwrap();
    fs::write(dir.join("Harness.java"),r#"import java.net.*;import java.lang.reflect.*;public class Harness {public static void main(String[] args)throws Exception{int count=0;for(int flags=0;flags<16;flags++)for(boolean nil:new boolean[]{false,true}){try(URLClassLoader loader=new URLClassLoader(new URL[]{new java.io.File(".").toURI().toURL()},null)){Class<?> hooks=Class.forName("sample.Hooks",true,loader);hooks.getField("flags").setInt(null,flags);Object fault=hooks.getField("fault").get(null);Class<?> context=Class.forName("android.content.Context",true,loader);Object value=nil?null:context.getConstructor().newInstance();Class<?> factory=Class.forName("sample.Factory",true,loader);Object result=null;Throwable failure=null;try{result=factory.getMethod("run",context).invoke(null,value);}catch(InvocationTargetException e){failure=e.getCause();}String expected=(flags&1)!=0?"parent":(flags&2)!=0?"parent,leaf":(flags&4)!=0?"parent,leaf,arg":(flags&8)!=0?"parent,leaf,arg,base":"parent,leaf,arg,base,after";if(!hooks.getField("trace").get(null).toString().equals(expected))throw new AssertionError("flags"+flags+" trace"+hooks.getField("trace").get(null));if(flags==0){if(failure!=null||result.getClass().getField("context").get(result)!=value)throw new AssertionError();}else{Throwable identity=(flags&3)!=0?failure.getCause():failure;if(identity!=fault)throw new AssertionError("fault identity"+flags);}count++;}}if(count!=32)throw new AssertionError(count);}}"#).unwrap();
    for (program, args) in [
        (
            "javac",
            vec![
                "Harness.java",
                "sample/Hooks.java",
                "sample/Leaf.java",
                "sample/Factory.java",
                "android/view/View.java",
                "android/content/Context.java",
            ],
        ),
        ("java", vec!["-Xverify:all", "-cp", ".", "Harness"]),
    ] {
        let output = Command::new(format!("{home}/bin/{program}"))
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
