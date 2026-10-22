//! Shared predicates retain reference identity, null, and boolean domains.
use rdx::{
    native_dex::{self, DexClass, DexSymbols},
    native_java,
};
use std::{process::Command, sync::Arc};

fn fixture(boolean: bool, opcode: u8) -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class
        .methods
        .retain(|method| method.name.as_ref() == "answer");
    class.symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Effects;".into()],
        strings: ["effect", "fallback", "step", "finish"]
            .into_iter()
            .map(String::from)
            .collect(),
        protos: vec![
            ("V".into(), vec![]),
            ("V".into(), vec!["I".into()]),
            ("I".into(), vec![]),
        ],
        methods: vec![(0, 0, 0), (0, 0, 1), (0, 1, 2), (0, 2, 3)],
        ..Default::default()
    });
    let method = &mut class.methods[0];
    method.name = format!("{}_{opcode:02x}", if boolean { "bools" } else { "refs" }).into();
    method.access_flags = 9;
    method.return_type = "I".into();
    let domain = if boolean { "Z" } else { "Ljava/lang/Object;" };
    method.parameters = vec!["I".into(), domain.into(), domain.into(), domain.into()];
    let code = method.code.as_mut().unwrap();
    code.registers = 8;
    code.ins = 4;
    code.outs = 1;
    code.tries = 0;
    code.try_regions.clear();
    let predicates: [u16; 3] = if opcode <= 0x37 {
        [0x6500, 0x7500, 0x7600]
    } else {
        [0x0500, 0x0600, 0x0700]
    };
    code.instructions = vec![
        0,
        0,
        0,
        predicates[0] | u16::from(opcode),
        10,
        predicates[1] | u16::from(opcode),
        8,
        predicates[2] | u16::from(opcode),
        6,
        0x0071,
        1,
        0,
        0x0d28,
        0x0071,
        0,
        0,
        0x0312,
        0x4335,
        8,
        0x1071,
        2,
        3,
        0x03d8,
        0x0103,
        0xf928,
        0x0071,
        3,
        0,
        0x000a,
        0x000f,
    ];
    class
}

fn source(class: &DexClass) -> String {
    native_java::render_method("sample.Entry", class, &class.methods[0])
        .unwrap()
        .source
}

#[test]
fn identity_null_and_boolean_predicates_share_one_original_body() {
    for boolean in [false, true] {
        for opcode in [0x32, 0x33, 0x38, 0x39] {
            let class = fixture(boolean, opcode);
            let original = class.methods[0].code.as_ref().unwrap().instructions.clone();
            let text = source(&class);
            assert_eq!(text.matches("Effects.effect(").count(), 1, "{text}");
            assert_eq!(text.matches("Effects.fallback(").count(), 1, "{text}");
            assert_eq!(text.matches("while (true)").count(), 1, "{text}");
            assert_eq!(
                class.methods[0].code.as_ref().unwrap().instructions,
                original
            );
        }
    }
}

#[test]
#[ignore = "requires javac/java; emitted methods compiled unchanged"]
fn jvm_reference_alias_null_and_boolean_domains_preserve_exact_paths() {
    let mut methods = String::new();
    let mut references = String::new();
    let mut booleans = String::new();
    for opcode in [0x32, 0x33, 0x38, 0x39] {
        methods.push_str(&source(&fixture(false, opcode)));
        methods.push_str(&source(&fixture(true, opcode)));
        references.push_str(&format!(
            "case {opcode}:return refs_{opcode:02x}(n,a,b,c);\n"
        ));
        booleans.push_str(&format!(
            "case {opcode}:return bools_{opcode:02x}(n,a,b,c);\n"
        ));
    }
    let java = HARNESS
        .replace("METHODS", &methods)
        .replace("REFERENCES", &references)
        .replace("BOOLEANS", &booleans);
    let dir = std::env::temp_dir().join(format!("rdx-condition-chain-refs-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    let file = dir.join("sample/Entry.java");
    std::fs::write(&file, java).unwrap();
    let home = std::env::var("RDX_JAVA25_HOME").ok();
    let executable = |name: &str| {
        home.as_ref()
            .map_or_else(|| name.to_string(), |home| format!("{home}/bin/{name}"))
    };
    let compile = Command::new(executable("javac"))
        .arg(&file)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let result = Command::new(executable("java"))
        .args(["-cp", dir.to_str().unwrap(), "sample.Entry"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

const HARNESS: &str = r#"package sample;
public class Entry {
METHODS
static int refs(int opcode,int n,Object a,Object b,Object c){switch(opcode){REFERENCES default:throw new AssertionError();}}
static int bools(int opcode,int n,boolean a,boolean b,boolean c){switch(opcode){BOOLEANS default:throw new AssertionError();}}
static int expected(boolean selected,int n){if(selected){Effects.effect();for(int i=0;i<n;i++)Effects.step(i);}else Effects.fallback();return Effects.finish();}
static boolean selected(int opcode,Object a,Object b,Object c){switch(opcode){
 case 50:return a==b||a==c||b==c;case 51:return a!=b||a!=c||b!=c;
 case 56:return a==null||b==null||c==null;case 57:return a!=null||b!=null||c!=null;
 default:throw new AssertionError();}}
static boolean selected(int opcode,boolean a,boolean b,boolean c){switch(opcode){
 case 50:return a==b||a==c||b==c;case 51:return a!=b||a!=c||b!=c;
 case 56:return !a||!b||!c;case 57:return a||b||c;
 default:throw new AssertionError();}}
static void compare(int opcode,int n,int fail,Object a,Object b,Object c,boolean primitive){
 int[] result={-1,-1};Throwable[] fault=new Throwable[2];String[] trace=new String[2];
 for(int side=0;side<2;side++){
  Effects.reset(fail);
  try{if(primitive){boolean x=(Boolean)a,y=(Boolean)b,z=(Boolean)c;result[side]=side==0?bools(opcode,n,x,y,z):expected(selected(opcode,x,y,z),n);}
      else result[side]=side==0?refs(opcode,n,a,b,c):expected(selected(opcode,a,b,c),n);}
  catch(RuntimeException caught){fault[side]=caught;}
  trace[side]=Effects.trace;
 }
 if(result[0]!=result[1]||fault[0]!=fault[1]||!trace[0].equals(trace[1]))throw new AssertionError(opcode+":"+n+":"+fail+":"+trace[0]+"/"+trace[1]);
}
public static void main(String[] args){
 Object[] values={null,new String("equal contents"),new String("equal contents")};int checks=0;
 for(int opcode:new int[]{50,51,56,57})for(int n=-1;n<=4;n++)for(int fail=-1;fail<=10;fail++){
  for(Object a:values)for(Object b:values)for(Object c:values){compare(opcode,n,fail,a,b,c,false);checks++;}
  for(boolean a:new boolean[]{false,true})for(boolean b:new boolean[]{false,true})for(boolean c:new boolean[]{false,true}){compare(opcode,n,fail,a,b,c,true);checks++;}
 }
 System.out.println("checks="+checks);
}
}
class Effects {
 static final RuntimeException sentinel=new RuntimeException("identity");
 static int failure,calls,steps,route;static String trace;
 static void reset(int at){failure=at;calls=0;steps=0;route=0;trace="";}
 static void operation(String name){trace+=name+",";if(calls++==failure)throw sentinel;}
 static void effect(){operation("E");route=1;}
 static void fallback(){operation("B");route=2;}
 static void step(int index){if(index!=steps++)throw new AssertionError("counter");operation("S"+index);}
 static int finish(){operation("F");return steps*100+route;}
}
"#;
