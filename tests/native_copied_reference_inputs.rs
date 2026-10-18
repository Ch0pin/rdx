use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_java,
};
use std::sync::Arc;
fn fixture(field: bool) -> (DexClass, DexMethod) {
    let class = DexClass {
        descriptor: "Lsample/Example;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![],
        symbols: Arc::new(DexSymbols {
            types: vec![
                "Lsample/Effects;".into(),
                "Lsample/Target;".into(),
                "Ljava/lang/String;".into(),
                "Ljava/lang/Object;".into(),
                "Lsample/Box;".into(),
            ],
            strings: vec![
                "touch".into(),
                "accept".into(),
                "<init>".into(),
                "item".into(),
            ],
            protos: vec![
                (
                    "I".into(),
                    if field {
                        vec!["I".into()]
                    } else {
                        vec!["I".into(), "Ljava/lang/String;".into()]
                    },
                ),
                ("V".into(), vec!["Ljava/lang/String;".into()]),
            ],
            methods: vec![(0, 0, 0), (0, 1, 1), (1, 1, 2)],
            fields: vec![(4, 2, 3)],
            ..Default::default()
        }),
    };
    let mut words = vec![
        0x0012, 0x0112, 0x0338, 4, 0x4001, 0x0528, 0x2035, 9, 0x00d8, 0x0100, 0x2071, 0, 0x0050,
        0x010a, 0xf828,
    ];
    // The copied body intentionally retains an integer-only callback. The
    // fresh post-loop field/input read is outside all copied instructions.
    if field {
        words[10] = 0x1071;
        words[12] = 0;
        words.extend([
            0x5454, 0, 0x0022, 1, 0x1071, 1, 4, 0x2070, 2, 0x0040, 0x0011,
        ]);
    } else {
        words.extend([0x0022, 1, 0x1071, 1, 5, 0x2070, 2, 0x0050, 0x0011]);
    }
    let method = DexMethod {
        declaring_type: class.descriptor.clone(),
        name: "run".into(),
        parameters: vec![
            "I".into(),
            "I".into(),
            "I".into(),
            if field {
                "Lsample/Box;".into()
            } else {
                "Ljava/lang/String;".into()
            },
        ],
        return_type: "Lsample/Target;".into(),
        thrown_types: vec![],
        access_flags: 9,
        code: Some(DexCode {
            registers: 6,
            ins: 4,
            outs: 2,
            tries: 0,
            try_regions: vec![],
            instructions: words,
            offset: 0,
        }),
    };
    (class, method)
}

#[test]
fn reference_inputs_after_copied_loop_preserve_order_and_navigation() {
    let (c, m) = fixture(false);
    let o = native_java::render_method("sample.Example", &c, &m).unwrap();
    println!("{}", o.source);
    assert!(
        o.source.find("new sample.Target(").unwrap()
            < o.source.find("sample.Effects.accept(").unwrap()
    );
    let links = format!("{:?}", o.links);
    assert!(links.contains("sample.Target.<init>(Ljava/lang/String;)V"));
    assert!(links.contains("sample.Effects.accept(Ljava/lang/String;)V"));
}
#[test]
fn arbitrary_object_domain_stays_rejected() {
    let (c, mut m) = fixture(false);
    m.parameters[3] = "Ljava/lang/Object;".into();
    assert!(native_java::render_method("sample.Example", &c, &m).is_err());
}
#[test]
#[ignore = "requires Java25"]
fn fresh_loader_reference_allocation_after_copied_loop_preserves_fault_identity() {
    use std::{fs, process::Command};
    let h = std::env::var("RDX_JAVA25_HOME").expect("RDX_JAVA25_HOME");
    let (c, m) = fixture(false);
    let source = native_java::render_method("sample.Example", &c, &m)
        .unwrap()
        .source;
    let d = std::env::temp_dir().join(format!("rdx-copy-reference-{}", std::process::id()));
    fs::create_dir_all(d.join("sample")).unwrap();
    fs::write(d.join("sample/Example.java"),format!(r#"package sample;public class Example {{{}
public static Target original(int count,int flag,int seed,String text){{int counter=0,result=0;if(flag!=0){{counter=seed;result=Effects.touch(counter,text);}}while(counter<count){{counter++;result=Effects.touch(counter,text);}}Target target=new Target(switch(0){{default->{{Effects.accept(text);yield text;}}}});return target;}}
}}"#,source)).unwrap();
    fs::write(d.join("sample/State.java"),r#"package sample;public class State{public static String trace="";public static Object input;public static int calls;public static Throwable marker=System.getProperty("kind").equals("E")?new AssertionError("marker"):new RuntimeException("marker");public static void hit(String text,int step){trace+=text+";";if(Integer.parseInt(System.getProperty("mode"))==step){if(marker instanceof Error e)throw e;throw(RuntimeException)marker;}}}"#).unwrap();
    fs::write(d.join("sample/Effects.java"),r#"package sample;public class Effects {static{State.hit("E",2);}public static int touch(int n,String text){if(text!=State.input)throw new AssertionError("touch identity");State.calls++;State.hit("P"+n,State.calls==1?3:6);return n;}public static void accept(String text){if(text!=State.input)throw new AssertionError("accept identity");State.hit("A",4);}}"#).unwrap();
    fs::write(d.join("sample/Target.java"),r#"package sample;public class Target {static{State.hit("N",1);}public final String text;public Target(String t){if(t!=State.input)throw new AssertionError("ctor identity");State.hit("T",5);text=t;}}"#).unwrap();
    fs::write(d.join("Driver.java"),r#"import java.net.*;import java.nio.file.*;import java.lang.reflect.*;public class Driver {
static String run(String method,int count,int flag,int seed,String input,int mode,String kind)throws Exception{System.setProperty("mode",""+mode);System.setProperty("kind",kind);try(URLClassLoader l=new URLClassLoader(new URL[]{Path.of(".").toUri().toURL()},null)){Class<?> state=Class.forName("sample.State",true,l);state.getField("input").set(null,input);Object marker=state.getField("marker").get(null);String result;try{Object value=Class.forName("sample.Example",true,l).getMethod(method,int.class,int.class,int.class,String.class).invoke(null,count,flag,seed,input);if(value.getClass().getField("text").get(value)!=input)throw new AssertionError("result identity");result="value";}catch(InvocationTargetException e){Throwable t=e.getCause();if(t!=marker && (!(t instanceof ExceptionInInitializerError)||t.getCause()!=marker))throw new AssertionError("fault identity",t);result="throw:"+t.getClass().getName();}return result+"|"+state.getField("trace").get(null)+"|"+state.getField("calls").getInt(null);}}
public static void main(String[]args)throws Exception {int cases=0;for(int count:new int[]{Integer.MIN_VALUE,-3,0,1,2,4})for(int flag:new int[]{0,1})for(int seed:new int[]{-1,0,3})for(String input:new String[]{null,new String("identity")})for(int mode=0;mode<7;mode++)for(String kind:new String[]{"R","E"}){String a=run("run",count,flag,seed,input,mode,kind),b=run("original",count,flag,seed,input,mode,kind);if(!a.equals(b))throw new AssertionError(a+" != "+b);cases++;}System.out.println("checked="+cases);}}
"#).unwrap();
    for (program, args) in [
        ("javac", vec!["sample/Example.java", "Driver.java"]),
        ("java", vec!["-cp", ".", "Driver"]),
    ] {
        let r = Command::new(format!("{h}/bin/{program}"))
            .args(args)
            .current_dir(&d)
            .output()
            .unwrap();
        assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
        if program == "java" {
            assert!(String::from_utf8_lossy(&r.stdout).contains("checked=1008"));
        }
    }
    fs::remove_dir_all(d).unwrap();
}
