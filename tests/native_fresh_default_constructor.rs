use rdx::{
    native_dex::{DexClass, DexCode, DexField, DexMethod, DexSymbols},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;
fn method(
    owner: &str,
    name: &str,
    ret: &str,
    registers: u16,
    ins: u16,
    words: Vec<u16>,
) -> DexMethod {
    DexMethod {
        declaring_type: owner.into(),
        name: name.into(),
        return_type: ret.into(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: if name == "<init>" { 0x10001 } else { 9 },
        code: Some(DexCode {
            registers,
            ins,
            outs: 1,
            tries: 0,
            try_regions: vec![],
            instructions: words,
            offset: 0,
        }),
    }
}
fn fixture() -> Vec<DexClass> {
    let symbols = Arc::new(DexSymbols {
        types: vec![
            "Ljava/lang/Object;".into(),
            "Lsample/Leaf;".into(),
            "I".into(),
        ],
        strings: vec!["<init>".into(), "a".into()],
        protos: vec![("V".into(), vec![])],
        methods: vec![(0, 0, 0)],
        fields: vec![(1, 0, 1)],
        ..Default::default()
    });
    let class = |name: &str, methods| DexClass {
        descriptor: name.into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods,
        symbols: symbols.clone(),
    };
    let caller = class(
        "Lsample/Caller;",
        vec![method(
            "Lsample/Caller;",
            "run",
            "Lsample/Leaf;",
            1,
            0,
            vec![0x0022, 1, 0x1070, 0, 0, 0x0011],
        )],
    );
    let mut leaf = class(
        "Lsample/Leaf;",
        vec![method(
            "Lsample/Leaf;",
            "<init>",
            "V",
            2,
            1,
            vec![0x1070, 0, 1, 0x0012, 0x105b, 0, 0x000e],
        )],
    );
    leaf.fields.push(DexField {
        declaring_type: leaf.descriptor.clone(),
        name: "a".into(),
        field_type: "Ljava/lang/Object;".into(),
        access_flags: 1,
        is_static: false,
    });
    vec![caller, leaf]
}
fn install(c: &[DexClass]) -> Arc<TypeHierarchy> {
    let h = Arc::new(TypeHierarchy::from_classes(c).unwrap());
    for k in c {
        let _ = k.symbols.hierarchy.set(h.clone());
    }
    h
}
fn render(c: &[DexClass]) -> anyhow::Result<rdx::engine::DecompiledCode> {
    install(c);
    native_java::render_method("sample.Caller", &c[0], &c[0].methods[0])
}
#[test]
fn fresh_allocation_only() {
    let c = fixture();
    let h = install(&c);
    assert!(!h.equivalent_noarg_constructor("Lsample/Leaf;", "Ljava/lang/Object;"));
    let source = native_java::render_method("sample.Caller", &c[0], &c[0].methods[0])
        .unwrap()
        .source;
    assert!(source.contains("new sample.Leaf()"), "{source}");
    println!("{source}");
}
#[test]
fn incomplete_or_observable_constructor_bodies_decline() {
    for kind in 0..15 {
        let mut c = fixture();
        match kind {
            0 => c[1].methods[0].code.as_mut().unwrap().instructions[3] = 0x1012,
            1 => c[1].fields[0].access_flags |= 0x40,
            2 => c[1].fields[0].access_flags |= 0x10,
            3 => {
                c[1].fields[0].is_static = true;
                c[1].fields[0].access_flags |= 8;
            }
            4 => c[1].fields.clear(),
            5 => {
                let f = fixture().pop().unwrap().fields.pop().unwrap();
                c[1].fields.push(f);
            }
            6 => c[1].fields[0].declaring_type = "Lother/Leaf;".into(),
            7 => c[1].fields[0].field_type = "I".into(),
            8 => c[1].methods[0].access_flags = 0x10002,
            9 => c[1].methods[0]
                .thrown_types
                .push("Ljava/io/IOException;".into()),
            10 => c[1].methods[0].code.as_mut().unwrap().instructions[4] = 0x015b,
            11 => {
                let m = fixture().pop().unwrap().methods.pop().unwrap();
                c[1].methods.push(m);
            }
            12 => {
                let clone = fixture().pop().unwrap();
                c.push(clone);
            }
            13 => c[1].superclass = Some("Lsample/Parent;".into()),
            14 => c[1].methods[0].code.as_mut().unwrap().tries = 1,
            _ => unreachable!(),
        }
        assert!(render(&c).is_err(), "mutation {kind} must decline");
    }
}
#[test]
fn intervening_instructions_and_super_calls_remain_outside_proof() {
    let mut c = fixture();
    c[0].methods[0]
        .code
        .as_mut()
        .unwrap()
        .instructions
        .insert(2, 0);
    assert!(render(&c).is_err());
    let mut c = fixture();
    c[0].superclass = Some("Lsample/Leaf;".into());
    c[0].methods[0] = method(
        "Lsample/Caller;",
        "<init>",
        "V",
        1,
        1,
        vec![0x1070, 0, 0, 0xe],
    );
    let h = install(&c);
    assert!(!h.equivalent_noarg_super_constructor("Lsample/Caller;", "Ljava/lang/Object;"));
    assert!(native_java::render_method("sample.Caller", &c[0], &c[0].methods[0]).is_err());
}
fn many_fields(n: usize, ty: &str) -> Vec<DexClass> {
    let mut c = fixture();
    let mut strings = vec!["<init>".to_string()];
    let mut fields = vec![];
    let mut declared = vec![];
    let mut words = vec![0x1070, 0, 1, 0x0012];
    for i in 0..n {
        let name = format!("field{i}");
        strings.push(name.clone());
        fields.push((1, 2, (i + 1) as u32));
        declared.push(DexField {
            declaring_type: "Lsample/Leaf;".into(),
            name: name.into(),
            field_type: ty.into(),
            access_flags: 1,
            is_static: false,
        });
        words.extend([0x105b, i as u16]);
    }
    words.push(0xe);
    let s = Arc::new(DexSymbols {
        types: vec![
            "Ljava/lang/Object;".into(),
            "Lsample/Leaf;".into(),
            ty.into(),
        ],
        strings,
        protos: vec![("V".into(), vec![])],
        methods: vec![(0, 0, 0)],
        fields,
        ..Default::default()
    });
    for k in &mut c {
        k.symbols = s.clone();
    }
    c[1].fields = declared;
    c[1].methods[0].code.as_mut().unwrap().instructions = words;
    c
}
#[test]
fn bounded_default_reference_field_family() {
    for ty in [
        "Ljava/lang/Object;",
        "Ljava/lang/String;",
        "[Ljava/lang/Object;",
        "[I",
        "[[J",
    ] {
        for count in [1, 2, 5, 64] {
            let c = many_fields(count, ty);
            assert!(render(&c).is_ok(), "{ty}/{count}");
        }
    }
    for ty in ["I", "J", "V", "[V", "Lbad.name;"] {
        assert!(render(&many_fields(1, ty)).is_err(), "{ty}");
    }
    assert!(render(&many_fields(65, "Ljava/lang/Object;")).is_err());
}
#[test]
fn owner_metadata_and_class_scope_are_exact() {
    for kind in 0..8 {
        let mut c = fixture();
        match kind {
            0 => c[1].access_flags = 0,
            1 => c[1].access_flags |= 0x400,
            2 => c[1].access_flags |= 0x200,
            3 => c[1].methods[0].code = None,
            4 => c[1].methods[0].access_flags |= 0x20,
            5 => c[1].methods[0].code.as_mut().unwrap().outs = 0,
            6 => c[1].methods[0].declaring_type = "Lother/Leaf;".into(),
            7 => {
                let mut object = fixture().pop().unwrap();
                object.descriptor = "Ljava/lang/Object;".into();
                object.superclass = None;
                object.methods.clear();
                object.fields.clear();
                c.push(object);
            }
            _ => unreachable!(),
        };
        assert!(render(&c).is_err(), "{kind}");
    }
}

#[test]
fn conflicting_duplicate_owner_declines_fresh_certificate() {
    let mut classes = fixture();
    let mut conflicting = fixture().pop().unwrap();
    conflicting.methods[0].code.as_mut().unwrap().instructions[3] = 0x1012;
    classes.push(conflicting);
    assert!(render(&classes).is_err());
}

#[test]
fn missing_constructor_access_flag_declines() {
    let mut classes = fixture();
    classes[1].methods[0].access_flags = 1;
    assert!(render(&classes).is_err());
}

#[test]
#[ignore = "requires a local Java 22 or newer runtime"]
fn fresh_default_constructor_preserves_raw_allocation_and_initialization() {
    let classes = fixture();
    install(&classes);
    let caller =
        native_java::render_method("sample.Caller", &classes[0], &classes[0].methods[0]).unwrap();
    let constructor =
        native_java::render_method("sample.Leaf", &classes[1], &classes[1].methods[0]).unwrap();
    let root = std::env::temp_dir().join(format!(
        "rdx-fresh-default-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join("sample")).unwrap();
    std::fs::write(
        root.join("sample/Caller.java"),
        format!(
            "package sample; public class Caller {{ {} }}",
            caller.source
        ),
    )
    .unwrap();
    std::fs::write(root.join("sample/Leaf.java"), format!("package sample; public class Leaf {{ public Object a; static {{ State.init(); }} {} }}", constructor.source)).unwrap();
    std::fs::write(root.join("sample/State.java"), r#"package sample; public class State {
 public static int mode; public static String trace=""; public static Throwable token;
 public static void init(){trace+="init;";if(mode==1)throw(RuntimeException)token;if(mode==2)throw(Error)token;}
}"#).unwrap();
    let words = classes[0].methods[0]
        .code
        .as_ref()
        .unwrap()
        .instructions
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let oracle = r#"package sample; public class Oracle {
 static final int[] W={WORDS}; public static Object run() throws Throwable {
 Object[] r=new Object[1]; for(int pc=0;pc<W.length;) {int word=W[pc],op=word&255,a=word>>>8;
 switch(op) {
 case 0x22: if(W[pc+1]!=1)throw new AssertionError();
 Class<?> t=Class.forName("sample.Leaf",false,Oracle.class.getClassLoader());
 Class<?> u=Class.forName("sun.misc.Unsafe");java.lang.reflect.Field f=u.getDeclaredField("theUnsafe");f.setAccessible(true);
 try {r[a]=u.getMethod("allocateInstance",Class.class).invoke(f.get(null),t);}catch(java.lang.reflect.InvocationTargetException x){throw x.getCause();}pc+=2;break;
 case 0x70: if(a!=16||W[pc+1]!=0||W[pc+2]!=0)throw new AssertionError();pc+=3;break;
 case 0x11: return r[a]; default:throw new AssertionError("unknown opcode "+op);
 }}throw new AssertionError();}}
"#.replace("WORDS",&words);
    std::fs::write(root.join("sample/Oracle.java"), oracle).unwrap();
    std::fs::write(root.join("Driver.java"), r#"import java.net.*;import java.nio.file.*;import java.lang.reflect.*;
public class Driver {
 static String run(boolean raw,int mode,int times)throws Throwable{
 try(URLClassLoader l=new URLClassLoader(new URL[]{Path.of(".").toUri().toURL()},ClassLoader.getPlatformClassLoader())){
 Class<?> state=Class.forName("sample.State",true,l);Throwable token=mode==2?new AssertionError("token"):new RuntimeException("token");
 state.getField("mode").setInt(null,mode);state.getField("token").set(null,token);
 Class<?> owner=Class.forName(raw?"sample.Oracle":"sample.Caller",true,l);Object previous=null;String result="";
 for(int i=0;i<times;i++)try{Object value=owner.getMethod("run").invoke(null);
 if(value==previous)throw new AssertionError("allocation reused");previous=value;Field a=value.getClass().getField("a");
 if(a.get(value)!=null)throw new AssertionError("nondefault field");a.set(value,new Object());result+="value;";
 }catch(InvocationTargetException x){Throwable t=x.getCause();if(i==0&&t!=token&&!(t instanceof ExceptionInInitializerError&&t.getCause()==token))throw new AssertionError("fault identity",t);result+=t.getClass().getName()+";";}
 return result+state.getField("trace").get(null);}}
 public static void main(String[]args)throws Throwable{int n=0;for(int mode=0;mode<3;mode++)for(int count:new int[]{1,2,16}){
 String a=run(true,mode,count),b=run(false,mode,count);if(!a.equals(b))throw new AssertionError(a+" != "+b);n++;}System.out.println("checked="+n);}}
"#).unwrap();
    let java_home = std::env::var_os("RDX_JAVA25_HOME").map(std::path::PathBuf::from);
    let binary = |name: &str| {
        java_home
            .as_ref()
            .map(|p| p.join("bin").join(name))
            .unwrap_or_else(|| name.into())
    };
    let compile = std::process::Command::new(binary("javac"))
        .current_dir(&root)
        .args([
            "sample/Caller.java",
            "sample/Leaf.java",
            "sample/State.java",
            "sample/Oracle.java",
            "Driver.java",
        ])
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = std::process::Command::new(binary("java"))
        .current_dir(&root)
        .args(["-Xverify:all", "-cp", ".", "Driver"])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "checked=9");
}
