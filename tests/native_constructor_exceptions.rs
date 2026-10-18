use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_java,
};
use std::sync::Arc;
fn fixture(start: u32) -> DexClass {
    DexClass {
        descriptor: "Lsample/Child;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            strings: vec!["<init>".into(), "touch".into()],
            types: vec!["Ljava/lang/Object;".into(), "Lsample/Effects;".into()],
            protos: vec![("V".into(), vec![])],
            methods: vec![(0, 0, 0), (1, 0, 1)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/Child;".into(),
            name: "<init>".into(),
            return_type: "V".into(),
            parameters: vec![],
            thrown_types: vec![],
            access_flags: 0x10001,
            code: Some(DexCode {
                registers: 2,
                ins: 1,
                outs: 1,
                tries: 1,
                offset: 0,
                instructions: vec![0x1070, 0, 1, 0x0071, 1, 0, 0x000e, 0x000d, 0x000e],
                try_regions: vec![DexTryRegion {
                    start,
                    end: 6,
                    catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 7)].into(),
                }],
            }),
        }],
    }
}
#[test]
fn initialized_constructor_can_catch_body_exception() {
    let class = fixture(3);
    let result = native_java::render_method("sample.Child", &class, &class.methods[0]).unwrap();
    assert!(
        result.source.find("super();").unwrap() < result.source.find("try {").unwrap(),
        "{}",
        result.source
    );
    assert!(
        result.source.contains("sample.Effects.touch()"),
        "{}",
        result.source
    );
    assert!(
        result.source.contains("catch (java.lang.RuntimeException"),
        "{}",
        result.source
    );
}
#[test]
fn constructor_handler_covering_super_is_rejected() {
    let class = fixture(0);
    assert!(native_java::render_method("sample.Child", &class, &class.methods[0]).is_err());
}

fn throwing_stub() -> DexClass {
    let mut class = fixture(3);
    let code = class.methods[0].code.as_mut().unwrap();
    code.try_regions.clear();
    code.tries = 0;
    code.instructions = vec![0x0012, 0x0027];
    class
}
#[test]
fn always_throwing_constructor_uses_explicit_unreachable_delegation() {
    let c = throwing_stub();
    let out = native_java::render_method("sample.Child", &c, &c.methods[0]).unwrap();
    assert!(out.source.contains("if (true)"), "{}", out.source);
    assert!(out.source.find("throw null;").unwrap() < out.source.find("super();").unwrap());
    let mut c = throwing_stub();
    c.superclass = Some("Lunknown/Base;".into());
    assert!(native_java::render_method("sample.Child", &c, &c.methods[0]).is_err());
}
#[test]
#[ignore = "requires JDK25 via RDX_JAVA25_HOME"]
fn java25_throwing_constructor_does_not_initialize_super() {
    use std::{fs, process::Command};
    let home = std::env::var("RDX_JAVA25_HOME").expect("RDX_JAVA25_HOME");
    let c = throwing_stub();
    let method = native_java::render_method("sample.Child", &c, &c.methods[0]).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-throw25-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let source = format!(
        "public class Child {{final int x; {} public static void main(String[] a){{try{{new Child();throw new AssertionError();}}catch(NullPointerException expected){{}}}}}}",
        method.source
    );
    fs::write(dir.join("Child.java"), source).unwrap();
    let result = Command::new(format!("{home}/bin/javac"))
        .arg("Child.java")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let result = Command::new(format!("{home}/bin/java"))
        .args(["-cp", ".", "Child"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let result = Command::new(format!("{home}/bin/javap"))
        .args(["-c", "-p", "Child.class"])
        .current_dir(&dir)
        .output()
        .unwrap();
    let disasm = String::from_utf8(result.stdout).unwrap();
    let constructor = disasm
        .split("public Child();")
        .nth(1)
        .unwrap()
        .split("public static")
        .next()
        .unwrap();
    assert!(constructor.contains("athrow"), "{disasm}");
    assert!(!constructor.contains("invokespecial"), "{disasm}");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn always_throwing_constructor_retains_prologue_effect_before_throw() {
    let mut c = throwing_stub();
    c.methods[0].code.as_mut().unwrap().instructions = vec![0x0071, 1, 0, 0x0012, 0x0027];
    let out = native_java::render_method("sample.Child", &c, &c.methods[0]).unwrap();
    assert_eq!(
        out.source.matches("sample.Effects.touch()").count(),
        1,
        "{}",
        out.source
    );
    assert!(
        out.source.find("sample.Effects.touch()").unwrap()
            < out.source.find("throw null;").unwrap()
    );
}

#[test]
fn throwing_constructor_requires_exact_accessible_loaded_parent() {
    let mut child = throwing_stub();
    child.superclass = Some("Lsample/Parent;".into());
    let mut parent = fixture(3);
    parent.descriptor = "Lsample/Parent;".into();
    parent.methods[0].declaring_type = parent.descriptor.clone();
    let parent_code = parent.methods[0].code.as_mut().unwrap();
    parent_code.tries = 0;
    parent_code.try_regions.clear();
    parent_code.instructions = vec![0x1070, 0, 1, 0x000e];
    let h =
        Arc::new(rdx::native_hierarchy::TypeHierarchy::from_classes([&child, &parent]).unwrap());
    assert!(h.has_accessible_noarg_super(&child.descriptor));
    child.symbols.hierarchy.set(h).unwrap();
    assert!(
        native_java::render_method("sample.Child", &child, &child.methods[0])
            .unwrap()
            .source
            .contains("if (true)")
    );
    parent.methods[0].access_flags = 2;
    let h = rdx::native_hierarchy::TypeHierarchy::from_classes([&child, &parent]).unwrap();
    assert!(!h.has_accessible_noarg_super(&child.descriptor));
    parent.methods[0].access_flags = 1;
    parent.methods[0].thrown_types = vec!["Ljava/lang/Exception;".into()];
    assert!(
        !rdx::native_hierarchy::TypeHierarchy::from_classes([&child, &parent])
            .unwrap()
            .has_accessible_noarg_super(&child.descriptor)
    );
}

#[test]
fn post_initializer_catchall_body_is_allowed() {
    let mut c = fixture(3);
    c.methods[0].code.as_mut().unwrap().try_regions[0].catches = vec![(None, 7)].into();
    let out = native_java::render_method("sample.Child", &c, &c.methods[0]).unwrap();
    println!("{}", out.source);
    assert!(out.source.find("super();").unwrap() < out.source.find("try {").unwrap());
}
#[test]
fn catchall_covering_initializer_stays_rejected() {
    let mut c = fixture(0);
    c.methods[0].code.as_mut().unwrap().try_regions[0].catches = vec![(None, 7)].into();
    assert!(native_java::render_method("sample.Child", &c, &c.methods[0]).is_err());
}
#[test]
fn post_initializer_catchall_handler_reentering_delegation_is_rejected() {
    let mut c = fixture(3);
    let code = c.methods[0].code.as_mut().unwrap();
    code.try_regions[0].catches = vec![(None, 7)].into();
    code.instructions[8] = 0xf828;
    assert!(native_java::render_method("sample.Child", &c, &c.methods[0]).is_err());
}
fn final_field_fixture(prefix: bool, alias: bool) -> DexClass {
    let mut c = fixture(3);
    c.superclass = Some("Lsample/Parent;".into());
    c.symbols = Arc::new(DexSymbols {
        strings: ["<init>", "read", "cleanup", "prefix", "x"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        types: [
            "Lsample/Parent;",
            "Lsample/Effects;",
            "Lsample/Child;",
            "I",
            "Ljava/lang/Throwable;",
        ]
        .into_iter()
        .map(Arc::from)
        .collect(),
        protos: vec![
            ("V".into(), vec!["I".into()]),
            ("I".into(), vec![]),
            ("V".into(), vec![]),
            ("V".into(), vec!["Ljava/lang/Throwable;".into(), "I".into()]),
        ],
        methods: vec![(0, 0, 0), (1, 1, 1), (1, 3, 2), (1, 2, 3)],
        fields: vec![(2, 3, 4)],
        ..Default::default()
    });
    c.fields = vec![rdx::native_dex::DexField {
        declaring_type: c.descriptor.clone(),
        name: "x".into(),
        field_type: "I".into(),
        access_flags: 0x11,
        is_static: false,
    }];
    let m = &mut c.methods[0];
    m.parameters = vec!["I".into()];
    let code = m.code.as_mut().unwrap();
    code.registers = 5;
    code.ins = 2;
    code.outs = 2;
    let mut words = vec![];
    if prefix {
        words.extend([0x0071, 3, 0]);
    }
    if alias {
        words.push(0x3207);
    }
    words.extend([0x2070, 0, if alias { 0x0042 } else { 0x0043 }]);
    if alias {
        words.extend([0x0213, 99]);
    }
    words.push(0x3012);
    let start = words.len();
    words.extend([0x0071, 1, 0, 0x000a]);
    let end = words.len();
    words.push(0x0628);
    let handler = words.len();
    words.extend([0x010d, 0x2071, 2, 1, 0x9012, 0x3059, 0, 0x000e]);
    // Normal field assignment jumps past the handler to the shared return.
    words[end] = (((words.len() - 3 - end) as u16) << 8) | 0x28;
    code.instructions = words;
    code.try_regions = vec![DexTryRegion {
        start: start as u32,
        end: end as u32,
        catches: vec![(None, handler as u32)].into(),
    }];
    c
}

#[test]
fn post_initializer_final_fields_prefix_and_alias_lifetimes_render() {
    for prefix in [false, true] {
        for alias in [false, true] {
            let c = final_field_fixture(prefix, alias);
            let out = native_java::render_method("sample.Child", &c, &c.methods[0]).unwrap();
            println!("PREFIX {prefix} ALIAS {alias}\n{}", out.source);
            assert!(out.source.find("super(").unwrap() < out.source.find("try {").unwrap());
            assert!(out.source.contains("this.x"));
        }
    }
}

fn delegation_fixture(prefix: bool) -> DexClass {
    let mut c = final_field_fixture(prefix, false);
    let mut symbols = DexSymbols {
        strings: c.symbols.strings.clone(),
        types: c.symbols.types.clone(),
        protos: c.symbols.protos.clone(),
        methods: c.symbols.methods.clone(),
        fields: c.symbols.fields.clone(),
        ..Default::default()
    };
    symbols.strings.push("y".into());
    symbols.fields[0] = (2, 3, 5);
    symbols.methods.push((2, 2, 0));
    c.symbols = Arc::new(symbols);
    c.fields.push(rdx::native_dex::DexField {
        declaring_type: c.descriptor.clone(),
        name: "y".into(),
        field_type: "I".into(),
        access_flags: 1,
        is_static: false,
    });
    let code = c.methods[0].code.as_mut().unwrap();
    let pc = if prefix { 3 } else { 0 };
    code.instructions[pc] = 0x1070;
    code.instructions[pc + 1] = 4;
    code.instructions[pc + 2] = 3;
    let delegated = DexMethod {
        declaring_type: c.descriptor.clone(),
        name: "<init>".into(),
        return_type: "V".into(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 0x10001,
        code: None,
    };
    c.methods.push(delegated);
    c
}

#[test]
fn post_initializer_delegation_retains_target_final_assignment_authority() {
    for prefix in [false, true] {
        let c = delegation_fixture(prefix);
        let out = native_java::render_method("sample.Child", &c, &c.methods[0]).unwrap();
        println!("DELEGATION {prefix}\n{}", out.source);
        assert!(out.source.contains("this();"));
        assert!(out.source.contains("this.y ="));
        let mut c = c;
        Arc::get_mut(&mut c.symbols).unwrap().fields[0] = (2, 3, 4);
        assert!(native_java::render_method("sample.Child", &c, &c.methods[0]).is_err());
    }
}

#[test]
fn post_initializer_final_field_invalid_assignment_paths_are_rejected() {
    // Protected store before a possible failure, followed by another store
    // from the catch, may write the same blank final twice.
    let mut c = final_field_fixture(false, false);
    let code = c.methods[0].code.as_mut().unwrap();
    let start = code.try_regions[0].start as usize;
    code.instructions.splice(start..start, [0x3059, 0]);
    code.try_regions[0].end += 2;
    code.try_regions[0].catches = vec![(None, code.try_regions[0].catches[0].1 + 2)].into();
    assert!(native_java::render_method("sample.Child", &c, &c.methods[0]).is_err());

    let mut c = final_field_fixture(false, false);
    let code = c.methods[0].code.as_mut().unwrap();
    let before = code.try_regions[0].start as usize;
    code.instructions.splice(before..before, [0x3052, 0]);
    code.try_regions[0].start += 2;
    code.try_regions[0].end += 2;
    code.try_regions[0].catches = vec![(None, code.try_regions[0].catches[0].1 + 2)].into();
    assert!(native_java::render_method("sample.Child", &c, &c.methods[0]).is_err());

    let mut c = final_field_fixture(false, false);
    let code = c.methods[0].code.as_mut().unwrap();
    let write = code.instructions.len() - 3;
    code.instructions.pop();
    code.instructions.extend([0x0439, (-2i16) as u16, 0x000e]);
    assert_eq!(code.instructions[write] as u8, 0x59);
    assert!(native_java::render_method("sample.Child", &c, &c.methods[0]).is_err());

    let mut c = final_field_fixture(false, false);
    let code = c.methods[0].code.as_mut().unwrap();
    let handler = code.try_regions[0].catches[0].1 as usize;
    code.instructions[handler + 1] =
        (((code.instructions.len() - 1 - (handler + 1)) as u16) << 8) | 0x28;
    assert!(native_java::render_method("sample.Child", &c, &c.methods[0]).is_err());
}

#[test]
fn post_initializer_proof_rejects_excessive_handler_metadata() {
    let mut c = fixture(3);
    let code = c.methods[0].code.as_mut().unwrap();
    code.try_regions[0].catches = vec![(None, 7)].into();
    code.try_regions = vec![code.try_regions[0].clone(); 17];
    code.tries = 17;
    assert!(native_java::render_method("sample.Child", &c, &c.methods[0]).is_err());
}

#[test]
fn post_initializer_field_authority_and_initializer_identity_are_exact() {
    let rejected = |c: DexClass| {
        assert!(native_java::render_method("sample.Child", &c, &c.methods[0]).is_err())
    };
    let mut c = final_field_fixture(false, false);
    c.fields.push(rdx::native_dex::DexField {
        declaring_type: c.descriptor.clone(),
        name: "x".into(),
        field_type: "J".into(),
        access_flags: 0x11,
        is_static: false,
    });
    rejected(c);
    let mut c = final_field_fixture(false, false);
    c.fields.clear();
    rejected(c);
    let mut c = final_field_fixture(false, false);
    Arc::get_mut(&mut c.symbols).unwrap().types[3] = "J".into();
    rejected(c);
    let mut c = final_field_fixture(false, false);
    let code = c.methods[0].code.as_mut().unwrap();
    let write = code.instructions.len() - 3;
    code.instructions[write] = 0x305b;
    rejected(c);
    let mut c = final_field_fixture(false, false);
    Arc::get_mut(&mut c.symbols).unwrap().fields[0].0 = 0;
    rejected(c);
    let mut c = final_field_fixture(false, false);
    let code = c.methods[0].code.as_mut().unwrap();
    let end = code.instructions.len() - 1;
    code.instructions.splice(end..end, [0x2070, 0, 0x0043]);
    rejected(c);
    let mut c = final_field_fixture(false, false);
    c.methods[0].access_flags |= 8;
    rejected(c);
    let mut c = final_field_fixture(false, false);
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions.splice(0..0, [0x0439, 6]);
    code.try_regions[0].start += 2;
    code.try_regions[0].end += 2;
    code.try_regions[0].catches = vec![(None, code.try_regions[0].catches[0].1 + 2)].into();
    rejected(c);
}

#[test]
#[ignore = "requires JDK25 via RDX_JAVA25_HOME"]
fn post_initializer_java_final_fields_delegation_and_fault_identity() {
    use std::{fs, process::Command};
    let home = std::env::var("RDX_JAVA25_HOME").expect("RDX_JAVA25_HOME");
    let dir = std::env::temp_dir().join(format!("rdx-post-init-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let mut source = String::from(
        r#"package sample;
class State {
 static int mask;static boolean error,fresh;static StringBuilder events=new StringBuilder();
 static Throwable fault,caught;static RuntimeException rt=new RuntimeException("fault");static Error er=new Error("fault");
 static void event(int bit,String name){events.append(name).append(';');if((mask&bit)!=0){fault=fresh?(error?new Error(name):new RuntimeException(name)):(error?er:rt);if(fault instanceof Error)throw (Error)fault;throw (RuntimeException)fault;}}
}
class Parent {static{State.event(1,"parent-init");}Parent(int p){State.event(16,"parent:"+p);}}
class Effects {
 static{State.event(4,"effects-init");}static void prefix(){State.event(8,"prefix");}
 static int read(){State.event(32,"read");return 11;}
 static void cleanup(Throwable e,int previous){State.caught=e;State.events.append("handled:").append(previous).append(':').append(e.getClass().getName()).append(':').append(e==State.fault).append(':').append(e.getCause()==State.fault).append(';');State.event(64,"handler");}
}
"#,
    );
    for index in 0..6 {
        let prefix = index == 2 || index == 3 || index == 5;
        let delegate = index >= 4;
        let c = if delegate {
            delegation_fixture(prefix)
        } else {
            final_field_fixture(prefix, index % 2 == 1)
        };
        let rendered = native_java::render_method("sample.Child", &c, &c.methods[0])
            .unwrap()
            .source
            .replace("public Child(", &format!("public A{index}("));
        let extra = if delegate {
            format!("A{index}(){{super(9);State.event(128,\"delegate\");x=5;}}int y;")
        } else {
            String::new()
        };
        source.push_str(&format!("class A{index} extends Parent{{static{{State.event(2,\"child-init\");}}final int x;{extra}{rendered}}}\n"));
        let prefix_source = if prefix { "Effects.prefix();" } else { "" };
        let initializer = if delegate { "this();" } else { "super(p0);" };
        let field = if delegate { "y" } else { "x" };
        let extra = if delegate {
            format!("R{index}(){{super(9);State.event(128,\"delegate\");x=5;}}int y;")
        } else {
            String::new()
        };
        source.push_str(&format!("class R{index} extends Parent{{static{{State.event(2,\"child-init\");}}final int x;{extra}R{index}(int p0){{{prefix_source}{initializer}int previous=3;int result;try{{result=Effects.read();previous=result;}}catch(Throwable caught){{Effects.cleanup(caught,previous);result=-7;}}this.{field}=result;}}}}\n"));
    }
    source.push_str(r#"public class Oracle {
 static String run(String name,int mask,boolean error,boolean fresh)throws Exception{
  try(java.net.URLClassLoader loader=new java.net.URLClassLoader(new java.net.URL[]{new java.io.File(".").toURI().toURL()},null)){
   Class<?> state=Class.forName("sample.State",true,loader);
   for(String key:new String[]{"mask","error","fresh"}){var field=state.getDeclaredField(key);field.setAccessible(true);field.set(null,key.equals("mask")?Integer.valueOf(mask):Boolean.valueOf(key.equals("error")?error:fresh));}
   Throwable thrown=null;Object value=null;
   try{Class<?> actor=Class.forName(name,false,loader);var ctor=actor.getDeclaredConstructor(int.class);ctor.setAccessible(true);value=ctor.newInstance(4);}catch(java.lang.reflect.InvocationTargetException e){thrown=e.getCause();}catch(Throwable e){thrown=e;}
   var events=state.getDeclaredField("events");events.setAccessible(true);String result=events.get(null).toString();
   var fault=state.getDeclaredField("fault");fault.setAccessible(true);var caught=state.getDeclaredField("caught");caught.setAccessible(true);
   if(value!=null){var x=value.getClass().getDeclaredField("x");x.setAccessible(true);result+="x="+x.get(value);try{var y=value.getClass().getDeclaredField("y");y.setAccessible(true);result+=",y="+y.get(value);}catch(NoSuchFieldException ignored){}}
   if(thrown!=null)result+="throw="+thrown.getClass().getName()+":"+(thrown==fault.get(null))+":"+(thrown.getCause()==fault.get(null))+":"+(thrown==caught.get(null));
   return result;
  }
 }
 public static void main(String[] args)throws Exception{
  int count=0;for(int index=0;index<6;index++)for(int mask=0;mask<256;mask++)for(boolean error:new boolean[]{false,true})for(boolean fresh:new boolean[]{false,true}){
   String a=run("sample.A"+index,mask,error,fresh),r=run("sample.R"+index,mask,error,fresh);if(!a.equals(r))throw new AssertionError(index+"/"+mask+"/"+error+"/"+fresh+"\n"+a+"\n"+r);count++;
  }System.out.println("comparisons="+count);
 }
}
"#);
    fs::write(dir.join("sample/Oracle.java"), source).unwrap();
    let compile = Command::new(format!("{home}/bin/javac"))
        .args(["sample/Oracle.java"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(format!("{home}/bin/java"))
        .args(["-cp", ".", "sample.Oracle"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(String::from_utf8_lossy(&run.stdout).contains("comparisons=6144"));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn post_initializer_catchall_uses_existing_retarget_certificate() {
    let mut child = fixture(3);
    child.superclass = Some("Lsample/Middle;".into());
    child.methods[0].code.as_mut().unwrap().try_regions[0].catches = vec![(None, 7)].into();
    let mut middle = fixture(3);
    middle.descriptor = "Lsample/Middle;".into();
    middle.methods.clear();
    middle.symbols = Arc::new(DexSymbols::default());
    let hierarchy = rdx::native_hierarchy::TypeHierarchy::from_classes([&child, &middle]).unwrap();
    assert!(hierarchy.equivalent_noarg_super_constructor(&child.descriptor, "Ljava/lang/Object;"));
    child.symbols.hierarchy.set(Arc::new(hierarchy)).unwrap();
    let output = native_java::render_method("sample.Child", &child, &child.methods[0]).unwrap();
    assert!(output.source.find("super();").unwrap() < output.source.find("try {").unwrap());
    assert!(
        output.source.contains("catch (java.lang.Throwable"),
        "{}",
        output.source
    );
}

#[test]
fn post_initializer_retarget_rejects_uncertified_forwarding_parents() {
    for shape in 0..7 {
        let mut child = fixture(3);
        child.superclass = Some("Lsample/Middle;".into());
        child.methods[0].code.as_mut().unwrap().try_regions[0].catches = vec![(None, 7)].into();
        let mut middle = fixture(3);
        middle.descriptor = "Lsample/Middle;".into();
        middle.methods[0].declaring_type = middle.descriptor.clone();
        middle.methods[0].code.as_mut().unwrap().instructions = vec![0x1070, 0, 1, 0x000e];
        middle.methods[0].code.as_mut().unwrap().try_regions.clear();
        middle.methods[0].code.as_mut().unwrap().tries = 0;
        match shape {
            0 => middle.methods[0]
                .code
                .as_mut()
                .unwrap()
                .instructions
                .insert(3, 0x0012),
            1 => middle.methods[0].access_flags = 0x10002,
            2 => middle.methods[0]
                .thrown_types
                .push("Ljava/lang/Exception;".into()),
            3 => middle.superclass = Some("Lmissing/Ancestor;".into()),
            4 => {}
            5 => middle.methods[0].code = None,
            6 => {
                middle.methods[0].code.as_mut().unwrap().instructions =
                    vec![0x1070, 0, 1, 0x0012, 0x0027];
            }
            _ => unreachable!(),
        }
        let classes = if shape == 4 {
            vec![&child, &middle, &middle]
        } else {
            vec![&child, &middle]
        };
        let h = rdx::native_hierarchy::TypeHierarchy::from_classes(classes).unwrap();
        assert!(
            !h.equivalent_noarg_super_constructor(&child.descriptor, "Ljava/lang/Object;"),
            "shape {shape}"
        );
        child.symbols.hierarchy.set(Arc::new(h)).unwrap();
        assert!(
            native_java::render_method("sample.Child", &child, &child.methods[0]).is_err(),
            "shape {shape}"
        );
    }
}

fn retargeted_final_fixture(prefix: bool, alias: bool) -> DexClass {
    let mut child = final_field_fixture(prefix, alias);
    child.superclass = Some("Lsample/Middle;".into());
    let symbols = Arc::get_mut(&mut child.symbols).unwrap();
    symbols.types[0] = "Ljava/lang/Object;".into();
    symbols.protos[0].1.clear();
    let words = &mut child.methods[0].code.as_mut().unwrap().instructions;
    let at = words.iter().position(|word| *word == 0x2070).unwrap();
    words[at] = 0x1070;
    words[at + 2] &= 0xf;
    let mut middle = fixture(3);
    middle.descriptor = "Lsample/Middle;".into();
    middle.methods.clear();
    middle.symbols = Arc::new(DexSymbols::default());
    let hierarchy = rdx::native_hierarchy::TypeHierarchy::from_classes([&child, &middle]).unwrap();
    assert!(hierarchy.equivalent_noarg_super_constructor(&child.descriptor, "Ljava/lang/Object;"));
    child.symbols.hierarchy.set(Arc::new(hierarchy)).unwrap();
    child
}
#[test]
fn post_initializer_noarg_retarget_keeps_final_assignment() {
    let child = retargeted_final_fixture(false, false);
    let source = native_java::render_method("sample.Child", &child, &child.methods[0])
        .unwrap()
        .source;
    assert!(source.contains("super();"), "{source}");
}
#[test]
#[ignore = "requires JDK25 via RDX_JAVA25_HOME"]
fn post_initializer_retarget_java_fault_identity() {
    use std::{fs, process::Command};
    let home = std::env::var("RDX_JAVA25_HOME").expect("RDX_JAVA25_HOME");
    let dir = std::env::temp_dir().join(format!("rdx-post-init-retarget-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let mut source = String::from(
        r#"package sample;
class State {
 static int mask;static boolean error,fresh;static StringBuilder events=new StringBuilder();
 static Throwable fault,caught;static RuntimeException rt=new RuntimeException("fault");static Error er=new Error("fault");
 static void event(int bit,String name){events.append(name).append(';');if((mask&bit)!=0){fault=fresh?(error?new Error(name):new RuntimeException(name)):(error?er:rt);if(fault instanceof Error)throw (Error)fault;throw (RuntimeException)fault;}}
}
class Parent {static{State.event(1,"parent-init");}Parent(){this(4);}Parent(int p){State.event(16,"parent:"+p);}}
class Middle {static{State.event(1,"middle-init");} Middle(){super();} }
class Effects {
 static{State.event(4,"effects-init");}static void prefix(){State.event(8,"prefix");}
 static int read(){State.event(32,"read");return 11;}
 static void cleanup(Throwable e,int previous){State.caught=e;State.events.append("handled:").append(previous).append(':').append(e.getClass().getName()).append(':').append(e==State.fault).append(':').append(e.getCause()==State.fault).append(';');State.event(64,"handler");}
}
"#,
    );
    for index in 0..6 {
        let prefix = index == 2 || index == 3 || index == 5;
        let delegate = index >= 4;
        let c = if delegate {
            delegation_fixture(prefix)
        } else {
            retargeted_final_fixture(prefix, index % 2 == 1)
        };
        let rendered = native_java::render_method("sample.Child", &c, &c.methods[0])
            .unwrap()
            .source
            .replace("public Child(", &format!("public A{index}("));
        let superclass = if delegate { "Parent" } else { "Middle" };
        let extra = if delegate {
            format!("A{index}(){{super(9);State.event(128,\"delegate\");x=5;}}int y;")
        } else {
            String::new()
        };
        source.push_str(&format!("class A{index} extends {superclass}{{static{{State.event(2,\"child-init\");}}final int x;{extra}{rendered}}}\n"));
        let prefix_source = if prefix { "Effects.prefix();" } else { "" };
        let initializer = if delegate { "this();" } else { "super();" };
        let field = if delegate { "y" } else { "x" };
        let extra = if delegate {
            format!("R{index}(){{super(9);State.event(128,\"delegate\");x=5;}}int y;")
        } else {
            String::new()
        };
        source.push_str(&format!("class R{index} extends {superclass}{{static{{State.event(2,\"child-init\");}}final int x;{extra}R{index}(int p0){{{prefix_source}{initializer}int previous=3;int result;try{{result=Effects.read();previous=result;}}catch(Throwable caught){{Effects.cleanup(caught,previous);result=-7;}}this.{field}=result;}}}}\n"));
    }
    source.push_str(r#"public class Oracle {
 static String run(String name,int mask,boolean error,boolean fresh)throws Exception{
  try(java.net.URLClassLoader loader=new java.net.URLClassLoader(new java.net.URL[]{new java.io.File(".").toURI().toURL()},null)){
   Class<?> state=Class.forName("sample.State",true,loader);
   for(String key:new String[]{"mask","error","fresh"}){var field=state.getDeclaredField(key);field.setAccessible(true);field.set(null,key.equals("mask")?Integer.valueOf(mask):Boolean.valueOf(key.equals("error")?error:fresh));}
   Throwable thrown=null;Object value=null;
   try{Class<?> actor=Class.forName(name,false,loader);var ctor=actor.getDeclaredConstructor(int.class);ctor.setAccessible(true);value=ctor.newInstance(4);}catch(java.lang.reflect.InvocationTargetException e){thrown=e.getCause();}catch(Throwable e){thrown=e;}
   var events=state.getDeclaredField("events");events.setAccessible(true);String result=events.get(null).toString();
   var fault=state.getDeclaredField("fault");fault.setAccessible(true);var caught=state.getDeclaredField("caught");caught.setAccessible(true);
   if(value!=null){var x=value.getClass().getDeclaredField("x");x.setAccessible(true);result+="x="+x.get(value);try{var y=value.getClass().getDeclaredField("y");y.setAccessible(true);result+=",y="+y.get(value);}catch(NoSuchFieldException ignored){}}
   if(thrown!=null)result+="throw="+thrown.getClass().getName()+":"+(thrown==fault.get(null))+":"+(thrown.getCause()==fault.get(null))+":"+(thrown==caught.get(null));
   return result;
  }
 }
 public static void main(String[] args)throws Exception{
  int count=0;for(int index=0;index<6;index++)for(int mask=0;mask<256;mask++)for(boolean error:new boolean[]{false,true})for(boolean fresh:new boolean[]{false,true}){
   String a=run("sample.A"+index,mask,error,fresh),r=run("sample.R"+index,mask,error,fresh);if(!a.equals(r))throw new AssertionError(index+"/"+mask+"/"+error+"/"+fresh+"\n"+a+"\n"+r);count++;
  }System.out.println("comparisons="+count);
 }
}
"#);
    fs::write(dir.join("sample/Oracle.java"), source).unwrap();
    let compile = Command::new(format!("{home}/bin/javac"))
        .args(["sample/Oracle.java"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(format!("{home}/bin/java"))
        .args(["-cp", ".", "sample.Oracle"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(String::from_utf8_lossy(&run.stdout).contains("comparisons=6144"));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn post_initializer_typed_retarget_requires_exact_forwarding_signature() {
    let mut child = final_field_fixture(false, false);
    child.superclass = Some("Lsample/Middle;".into());
    let mut middle = fixture(3);
    middle.descriptor = "Lsample/Middle;".into();
    middle.superclass = Some("Lsample/Parent;".into());
    middle.methods.clear();
    middle.symbols = Arc::new(DexSymbols::default());
    let mut parent = fixture(3);
    parent.descriptor = "Lsample/Parent;".into();
    parent.methods[0].declaring_type = parent.descriptor.clone();
    parent.methods[0].parameters = vec!["I".into()];
    parent.methods[0].code = None;
    let h = rdx::native_hierarchy::TypeHierarchy::from_classes([&child, &middle, &parent]).unwrap();
    assert!(!h.equivalent_super_constructor(&child.descriptor, "Lsample/Parent;", &["I".into()]));
    child.symbols.hierarchy.set(Arc::new(h)).unwrap();
    assert!(native_java::render_method("sample.Child", &child, &child.methods[0]).is_err());
}

fn external_field_fixture() -> (DexClass, DexClass) {
    let mut child = final_field_fixture(false, false);
    child.methods[0].parameters.push("Lsample/Holder;".into());
    let symbols = Arc::get_mut(&mut child.symbols).unwrap();
    symbols.types.push("Lsample/Holder;".into());
    symbols.fields.push((5, 3, 4));
    let code = child.methods[0].code.as_mut().unwrap();
    code.registers = 6;
    code.ins = 3;
    let at = code.try_regions[0].start as usize;
    code.instructions.splice(at..at, [0x5059, 1]);
    code.try_regions[0].end += 2;
    code.try_regions[0].catches = vec![(None, code.try_regions[0].catches[0].1 + 2)].into();
    let mut holder = fixture(3);
    holder.descriptor = "Lsample/Holder;".into();
    holder.methods.clear();
    holder.symbols = Arc::new(DexSymbols::default());
    holder.fields = vec![rdx::native_dex::DexField {
        declaring_type: holder.descriptor.clone(),
        name: "x".into(),
        field_type: "I".into(),
        access_flags: 1,
        is_static: false,
    }];
    (child, holder)
}
#[test]
fn post_initializer_external_public_field_keeps_original_receiver_and_order() {
    let (child, holder) = external_field_fixture();
    let h = rdx::native_hierarchy::TypeHierarchy::from_classes([&child, &holder]).unwrap();
    child.symbols.hierarchy.set(Arc::new(h)).unwrap();
    let rendered = native_java::render_method("sample.Child", &child, &child.methods[0]).unwrap();
    assert!(
        rendered
            .links
            .iter()
            .any(|link| link.label == "sample.Holder.x:I")
    );
    let source = rendered.source;
    assert!(source.contains("p1.x = 3;"), "{source}");
    assert!(
        source.find("p1.x = 3;").unwrap() < source.find("sample.Effects.read()").unwrap(),
        "{source}"
    );
}
#[test]
fn post_initializer_external_field_requires_exact_loaded_declaration() {
    for shape in 0..16 {
        let (mut child, mut holder) = external_field_fixture();
        match shape {
            0 => holder.fields[0].access_flags |= 0x10,
            1 => holder.fields[0].access_flags = 2,
            2 => holder.fields[0].is_static = true,
            3 => holder.fields[0].field_type = "J".into(),
            4 => holder.fields.push(rdx::native_dex::DexField {
                declaring_type: holder.descriptor.clone(),
                name: "x".into(),
                field_type: "J".into(),
                access_flags: 1,
                is_static: false,
            }),
            5 => holder.access_flags = 0,
            6 => holder.fields[0].declaring_type = "Lsample/Other;".into(),
            7 => holder.fields.clear(),
            8 => {}
            9 => {
                let code = child.methods[0].code.as_mut().unwrap();
                let pc = code.try_regions[0].start as usize;
                code.instructions[pc] = 0x505b;
            }
            10 => {}
            11 => holder.access_flags = 3,
            12 => {
                for i in 0..256 {
                    holder.fields.push(rdx::native_dex::DexField {
                        declaring_type: holder.descriptor.clone(),
                        name: format!("f{i}").into(),
                        field_type: "I".into(),
                        access_flags: 1,
                        is_static: false,
                    });
                }
            }
            13 => holder.fields[0].access_flags = 3,
            14 => holder.fields[0].access_flags |= 0x2000,
            15 => holder.access_flags |= 0x8000,
            _ => unreachable!(),
        }
        let classes = if shape == 10 {
            vec![&child]
        } else if shape == 8 {
            vec![&child, &holder, &holder]
        } else {
            vec![&child, &holder]
        };
        let h = rdx::native_hierarchy::TypeHierarchy::from_classes(classes).unwrap();
        child.symbols.hierarchy.set(Arc::new(h)).unwrap();
        assert!(
            native_java::render_method("sample.Child", &child, &child.methods[0]).is_err(),
            "shape {shape}"
        );
    }
}
#[test]
#[ignore = "requires JDK25 via RDX_JAVA25_HOME"]
fn post_initializer_external_field_java_identity_and_fault_order() {
    use std::{fs, process::Command};
    let home = std::env::var("RDX_JAVA25_HOME").expect("RDX_JAVA25_HOME");
    let dir = std::env::temp_dir().join(format!("rdx-post-init-field-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let mut source = String::from(
        r#"package sample;
class State {
 static int mask;static boolean error,fresh;static StringBuilder events=new StringBuilder();
 static Throwable fault,caught;static RuntimeException rt=new RuntimeException("fault");static Error er=new Error("fault");
 static void event(int bit,String name){events.append(name).append(';');if((mask&bit)!=0){fault=fresh?(error?new Error(name):new RuntimeException(name)):(error?er:rt);if(fault instanceof Error)throw (Error)fault;throw (RuntimeException)fault;}}
}
class Parent {static{State.event(1,"parent-init");}Parent(int p){State.event(16,"parent:"+p);}}
class Holder {public int x=9;static{State.event(128,"holder-init");}}
class Effects {
 static{State.event(4,"effects-init");}static void prefix(){State.event(8,"prefix");}
 static int read(){State.event(32,"read");return 11;}
 static void cleanup(Throwable e,int previous){State.caught=e;State.events.append("handled:").append(previous).append(':').append(e.getClass().getName()).append(':').append(e==State.fault).append(':').append(e.getCause()==State.fault).append(';');State.event(64,"handler");}
}
"#,
    );
    for index in 0..2 {
        let prefix = index == 2 || index == 3 || index == 5;
        let delegate = index >= 4;
        let (mut c, holder) = external_field_fixture();
        if index == 1 {
            let code = c.methods[0].code.as_mut().unwrap();
            let at = code.try_regions[0].start as usize;
            code.instructions[at] = 0x2059;
            code.instructions.insert(at, 0x5207);
            code.try_regions[0].end += 1;
            code.try_regions[0].catches = vec![(None, code.try_regions[0].catches[0].1 + 1)].into();
        }
        let hierarchy = rdx::native_hierarchy::TypeHierarchy::from_classes([&c, &holder]).unwrap();
        c.symbols.hierarchy.set(Arc::new(hierarchy)).unwrap();
        let rendered = native_java::render_method("sample.Child", &c, &c.methods[0])
            .unwrap()
            .source
            .replace("public Child(", &format!("public A{index}("));
        let extra = if delegate {
            format!("A{index}(){{super(9);State.event(128,\"delegate\");x=5;}}int y;")
        } else {
            String::new()
        };
        source.push_str(&format!("class A{index} extends Parent{{static{{State.event(2,\"child-init\");}}final int x;{extra}{rendered}}}\n"));
        let prefix_source = if prefix { "Effects.prefix();" } else { "" };
        let initializer = if delegate { "this();" } else { "super(p0);" };
        let field = if delegate { "y" } else { "x" };
        let extra = if delegate {
            format!("R{index}(){{super(9);State.event(128,\"delegate\");x=5;}}int y;")
        } else {
            String::new()
        };
        source.push_str(&format!("class R{index} extends Parent{{static{{State.event(2,\"child-init\");}}final int x;{extra}R{index}(int p0,Holder p1){{{prefix_source}{initializer}int previous=3;int result;try{{p1.x=3;result=Effects.read();previous=result;}}catch(Throwable caught){{Effects.cleanup(caught,previous);result=-7;}}this.{field}=result;}}}}\n"));
    }
    source.push_str(r#"public class Oracle {
 static String run(String name,int mask,boolean error,boolean fresh,boolean nullHolder)throws Exception{
  try(java.net.URLClassLoader loader=new java.net.URLClassLoader(new java.net.URL[]{new java.io.File(".").toURI().toURL()},null)){
   Class<?> state=Class.forName("sample.State",true,loader);
   for(String key:new String[]{"mask","error","fresh"}){var field=state.getDeclaredField(key);field.setAccessible(true);field.set(null,key.equals("mask")?Integer.valueOf(mask):Boolean.valueOf(key.equals("error")?error:fresh));}
   Throwable thrown=null;Object value=null,holder=null;
   try{Class<?> holderClass=Class.forName("sample.Holder",false,loader);if(!nullHolder){var hc=holderClass.getDeclaredConstructor();hc.setAccessible(true);holder=hc.newInstance();}Class<?> actor=Class.forName(name,false,loader);var ctor=actor.getDeclaredConstructor(int.class,holderClass);ctor.setAccessible(true);value=ctor.newInstance(4,holder);}catch(java.lang.reflect.InvocationTargetException e){thrown=e.getCause();}catch(Throwable e){thrown=e;}
   var events=state.getDeclaredField("events");events.setAccessible(true);String result=events.get(null).toString();
   var fault=state.getDeclaredField("fault");fault.setAccessible(true);var caught=state.getDeclaredField("caught");caught.setAccessible(true);
   if(value!=null){var x=value.getClass().getDeclaredField("x");x.setAccessible(true);result+="x="+x.get(value);try{var y=value.getClass().getDeclaredField("y");y.setAccessible(true);result+=",y="+y.get(value);}catch(NoSuchFieldException ignored){}}
   if(holder!=null){var hf=holder.getClass().getDeclaredField("x");hf.setAccessible(true);result+="holder="+hf.get(holder);}
   if(thrown!=null)result+="throw="+thrown.getClass().getName()+":"+(thrown==fault.get(null))+":"+(thrown.getCause()==fault.get(null))+":"+(thrown==caught.get(null));
   return result;
  }
 }
 public static void main(String[] args)throws Exception{
  int count=0;for(int index=0;index<2;index++)for(int mask=0;mask<256;mask++)for(boolean error:new boolean[]{false,true})for(boolean fresh:new boolean[]{false,true})for(boolean nullHolder:new boolean[]{false,true}){
   String a=run("sample.A"+index,mask,error,fresh,nullHolder),r=run("sample.R"+index,mask,error,fresh,nullHolder);if(!a.equals(r))throw new AssertionError(index+"/"+mask+"/"+error+"/"+fresh+"\n"+a+"\n"+r);count++;
  }System.out.println("comparisons="+count);
 }
}
"#);
    fs::write(dir.join("sample/Oracle.java"), source).unwrap();
    let compile = Command::new(format!("{home}/bin/javac"))
        .args(["sample/Oracle.java"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(format!("{home}/bin/java"))
        .args(["-cp", ".", "sample.Oracle"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(String::from_utf8_lossy(&run.stdout).contains("comparisons=4096"));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn external_field_certificate_retains_post_initializer_store_boundary() {
    let (mut child, holder) = external_field_fixture();
    let code = child.methods[0].code.as_mut().unwrap();
    code.instructions.splice(0..0, [0x3012, 0x5059, 1]);
    code.try_regions[0].start += 3;
    code.try_regions[0].end += 3;
    code.try_regions[0].catches = vec![(None, code.try_regions[0].catches[0].1 + 3)].into();
    let h = rdx::native_hierarchy::TypeHierarchy::from_classes([&child, &holder]).unwrap();
    child.symbols.hierarchy.set(Arc::new(h)).unwrap();
    assert!(native_java::render_method("sample.Child", &child, &child.methods[0]).is_err());
}
