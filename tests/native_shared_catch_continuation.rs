//! A branch-local catch joins an effectful constructor continuation exactly once.
use rdx::{
    native_dex::{DexClass, DexCode, DexField, DexMethod, DexSymbols, DexTryRegion},
    native_java,
};
use std::{process::Command, sync::Arc};

fn fixture(terminal_handler: bool) -> DexClass {
    let owner: Arc<str> = "Lsample/SharedChild;".into();
    let mut instructions = vec![
        0x1070, 0, 3, // Object initializer
        0x0438, 10, // if p0 == 0 -> shared tail13
        0x0071, 1, 0, 0x0528, // protected work(); goto shared13
        0x000d, 0x1071, 2, 0, // handler: caught(exact throwable), then shared13
        0x0071, 3, 0, 0x0112, // shared tail(); i=0
        0x4135, 8, 0x1071, 4, 1, // if i>=p0 -> finish25; step(i)
        0x01d8, 0x0101, 0xf928, // i++; goto guard17
        0x0071, 5, 0, 0x000a, 0x3059, 0, 0x000e, // finish(); this.x=result; return
    ];
    if terminal_handler {
        // A separate terminating handler retains its own captured throwable;
        // it never reaches the normal continuation or the final assignment.
        instructions.insert(13, 0x0027);
        instructions[4] = 11;
        instructions[8] = 0x0628;
    }
    DexClass {
        descriptor: owner.clone(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 0x11,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![DexField {
            declaring_type: owner.clone(),
            name: "x".into(),
            field_type: "I".into(),
            access_flags: 0x11,
            is_static: false,
        }],
        symbols: Arc::new(DexSymbols {
            types: [
                "Ljava/lang/Object;",
                "Lsample/Effects;",
                "Lsample/SharedChild;",
                "I",
            ]
            .into_iter()
            .map(Arc::from)
            .collect(),
            strings: ["<init>", "work", "caught", "tail", "step", "finish", "x"]
                .into_iter()
                .map(String::from)
                .collect(),
            protos: vec![
                ("V".into(), vec![]),
                ("V".into(), vec!["Ljava/lang/Throwable;".into()]),
                ("V".into(), vec!["I".into()]),
                ("I".into(), vec![]),
            ],
            methods: vec![
                (0, 0, 0),
                (1, 0, 1),
                (1, 1, 2),
                (1, 0, 3),
                (1, 2, 4),
                (1, 3, 5),
            ],
            fields: vec![(2, 3, 6)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: owner,
            name: "<init>".into(),
            access_flags: 0x10001,
            return_type: "V".into(),
            parameters: vec!["I".into()],
            thrown_types: vec![],
            code: Some(DexCode {
                registers: 5,
                ins: 2,
                outs: 1,
                tries: 1,
                offset: 0,
                instructions,
                try_regions: vec![DexTryRegion {
                    start: 5,
                    end: 8,
                    catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 9)].into(),
                }],
            }),
        }],
    }
}

fn source(class: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.SharedChild", class, &class.methods[0])?.source)
}

#[test]
fn shared_normal_and_catch_tail_is_emitted_once_with_one_final_assignment() {
    for terminal in [false, true] {
        let class = fixture(terminal);
        let original = class.methods[0].code.as_ref().unwrap().instructions.clone();
        let text = source(&class).unwrap_or_else(|error| panic!("terminal={terminal}: {error:#}"));
        for call in [
            "Effects.work(",
            "Effects.caught(",
            "Effects.tail(",
            "Effects.step(",
            "Effects.finish(",
        ] {
            assert_eq!(
                text.matches(call).count(),
                1,
                "terminal={terminal}, call={call}\n{text}"
            );
        }
        assert_eq!(text.matches("this.x =").count(), 1, "{text}");
        assert_eq!(text.matches("while (true)").count(), 1, "{text}");
        assert_eq!(
            class.methods[0].code.as_ref().unwrap().instructions,
            original
        );
    }
}

#[test]
fn handler_reentry_into_original_protection_keeps_its_region_boundary_guard() {
    let mut class = fixture(false);
    class.methods[0].code.as_mut().unwrap().instructions[10..13].copy_from_slice(&[0xfb28, 0, 0]); // handler returns to protected work at5
    assert!(source(&class).is_err());
}

#[test]
#[ignore = "requires javac and java; constructor emitted unchanged"]
fn jvm_shared_tail_preserves_final_value_effect_order_and_exception_identity() {
    let base = std::env::temp_dir().join(format!("rdx-shared-catch-tail-{}", std::process::id()));
    let home = std::env::var("RDX_JAVA25_HOME").ok();
    let executable = |name: &str| {
        home.as_ref()
            .map_or_else(|| name.to_string(), |home| format!("{home}/bin/{name}"))
    };
    for terminal in [false, true] {
        let text = source(&fixture(terminal)).unwrap();
        let dir = base.join(if terminal { "terminal" } else { "shared" });
        std::fs::create_dir_all(dir.join("sample")).unwrap();
        let file = dir.join("sample/SharedChild.java");
        std::fs::write(
            &file,
            HARNESS
                .replace("METHOD", &text)
                .replace("TERMINAL", if terminal { "true" } else { "false" }),
        )
        .unwrap();
        let compile = Command::new(executable("javac"))
            .arg(&file)
            .output()
            .unwrap();
        assert!(
            compile.status.success(),
            "{}\n{text}",
            String::from_utf8_lossy(&compile.stderr)
        );
        let result = Command::new(executable("java"))
            .args(["-cp", dir.to_str().unwrap(), "sample.SharedChild"])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}\n{text}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

const HARNESS: &str = r#"package sample;
public class SharedChild {
public final int x;
METHOD
static int expected(int n) {
 if(n!=0) {
  try {Effects.work();}
  catch(RuntimeException fault) {Effects.caught(fault);if(TERMINAL)throw fault;}
 }
 Effects.tail();
 for(int i=0;i<n;i++)Effects.step(i);
 return Effects.finish();
}
public static void main(String[] args) {
 int cases=0;
 for(int n=-2;n<=8;n++)for(int failure=-1;failure<=16;failure++) {
  int[] result={-1,-1}; Throwable[] thrown=new Throwable[2],captured=new Throwable[2]; String[] trace=new String[2];
  for(int side=0;side<2;side++) {
   Effects.reset(failure);
   try {result[side]=side==0?new SharedChild(n).x:expected(n);}
   catch(RuntimeException fault) {thrown[side]=fault;}
   captured[side]=Effects.captured;trace[side]=Effects.trace;
  }
  if(result[0]!=result[1] || thrown[0]!=thrown[1] || captured[0]!=captured[1] || !trace[0].equals(trace[1]))
   throw new AssertionError(n+":"+failure+":"+trace[0]+"/"+trace[1]);
  cases++;
 }
 System.out.println("cases="+cases);
}
}
class Effects {
 static final RuntimeException original=new RuntimeException("original"),continuation=new RuntimeException("continuation");
 static int failure,calls,steps;static String trace;static Throwable captured;
 static void reset(int at) {failure=at;calls=0;steps=0;trace="";captured=null;}
 static void operation(String name) {trace+=name+",";if(calls++==failure)throw name.equals("W")?original:continuation;}
 static void work() {operation("W");}
 static void caught(Throwable fault) {if(fault!=original)throw new AssertionError("identity");captured=fault;operation("C");}
 static void tail() {operation("T");}
 static void step(int index) {if(index!=steps++)throw new AssertionError("loop");operation("S"+index);}
 static int finish() {operation("F");return 1000+steps;}
}
"#;
