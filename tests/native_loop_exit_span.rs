pub use rdx::{native_cfg, native_dex, native_hierarchy, native_ir, native_ssa, native_types};
#[path = "../src/native_java/loop_exit_type.rs"]
mod lifetime;
#[path = "../src/native_java/pure_loop_exit_span.rs"]
mod span;
use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_java,
    native_method::MethodAnalysis,
};
use std::{fs, process::Command, sync::Arc};

fn fixture() -> DexClass {
    DexClass {
        descriptor: "Lsample/ExitSpan;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            types: vec!["Lsample/Effects;".into()],
            strings: vec!["touch".into(), "seed".into()],
            protos: vec![("V".into(), vec!["I".into()]), ("I".into(), vec![])],
            methods: vec![(0, 0, 0), (0, 1, 1)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/ExitSpan;".into(),
            name: "run".into(),
            access_flags: 9,
            return_type: "Ljava/lang/Object;".into(),
            parameters: vec!["I".into(), "Ljava/lang/Object;".into()],
            thrown_types: vec![],
            code: Some(DexCode {
                registers: 5,
                ins: 2,
                outs: 1,
                tries: 0,
                try_regions: vec![],
                offset: 0,
                instructions: vec![
                    0x0012, // @0 counter=0
                    0x0071, 1, 0,      // @1 seed()I
                    0x010a, // @4 dead integer lifetime of r1
                    0x3035, 11, // @5 counter>=limit -> default @16
                    0x1071, 0, 0,      // @7 touch(counter)
                    0x4107, // @10 r1=object
                    0x00d8, 0x0100, // @11 counter++
                    0x0138, 0xfff8, // @13 r1==null -> header @5
                    0x0228, // @15 success skips guard default -> @17
                    0x0112, // @16 guard default r1=null
                    0x0111, // @17 return-object r1
                ],
            }),
        }],
    }
}
fn source(c: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.ExitSpan", c, &c.methods[0])?.source)
}
fn exit_type(c: &DexClass) -> Option<String> {
    let a = MethodAnalysis::build(c, &c.methods[0]).unwrap();
    let t = a.infer_types().unwrap();
    lifetime::exit_type(a.ssa(), &t, 5..17, 17, 1, None, None)
}
#[test]
fn success_goto_and_guard_default_keep_the_exact_exit_lifetime() {
    let c = fixture();
    assert!(span::admits(c.methods[0].code.as_ref().unwrap(), 5, 15, 17));
    assert_eq!(exit_type(&c).as_deref(), Some("Ljava/lang/Object;"));
    let text = source(&c).unwrap();
    assert!(
        text.contains("Effects.seed()") && text.contains("while (true)"),
        "{text}"
    );
    assert!(
        text.lines()
            .any(|l| l.trim().starts_with("java.lang.Object ") && !l.contains('=')),
        "{text}"
    );
}
#[test]
fn backward_reentry_effectful_protected_and_invalid_boundaries_decline() {
    for shape in 0..8 {
        let mut c = fixture();
        let code = c.methods[0].code.as_mut().unwrap();
        let (mut header, mut tail, mut exit) = (5, 15, 17);
        match shape {
            0 => code.instructions[15] = 0xf628, // backedge to header
            1 => code.instructions[16] = 0x0027, // throw
            2 => code.instructions[16] = 0x001d, // monitor-enter
            3 => {
                code.tries = 1;
                code.try_regions.push(DexTryRegion {
                    start: 15,
                    end: 17,
                    catches: vec![(None, 17)].into(),
                });
            }
            4 => tail = 14, // operand, rather than instruction boundary
            5 => header = 6,
            6 => exit = 18,
            // Both paths now merge at @16; requested @17 is an interior
            // instruction, so it cannot serve as the canonical join block.
            _ => code.instructions[15] = 0x0128,
        }
        assert!(!span::admits(code, header, tail, exit), "shape {shape}");
    }
}
#[test]
fn outside_entry_into_default_span_declines() {
    let mut c = fixture();
    let code = c.methods[0].code.as_mut().unwrap();
    // A genuinely reachable pre-loop conditional bypasses seed and the header.
    // Insert two words, then relocate every original target by the same amount.
    code.instructions.splice(0..0, [0x0338, 18]);
    assert!(!span::admits(code, 7, 17, 19));
}
#[test]
fn control_certificate_preserves_nonzero_undefined_and_incomplete_ssa_rejections() {
    for default in [0x1112, 0x2107, 0] {
        let mut c = fixture();
        c.methods[0].code.as_mut().unwrap().instructions[16] = default;
        assert!(span::admits(c.methods[0].code.as_ref().unwrap(), 5, 15, 17));
        assert!(exit_type(&c).is_none());
        assert!(source(&c).is_err());
    }
    let c = fixture();
    let a = MethodAnalysis::build(&c, &c.methods[0]).unwrap();
    let t = a.infer_types().unwrap();
    for duplicate in [false, true] {
        let mut s = native_ssa::SsaMethod::build(
            c.methods[0].code.as_ref().unwrap(),
            a.instructions(),
            a.blocks(),
        )
        .unwrap();
        let p = s
            .phis
            .iter_mut()
            .find(|p| p.register == 1 && s.graph.blocks[p.block].start == 17)
            .unwrap();
        if duplicate {
            p.incoming.push(p.incoming[0]);
        } else {
            p.incoming.pop();
        }
        assert!(lifetime::exit_type(&s, &t, 5..17, 17, 1, None, None).is_none());
    }
}
#[test]
#[ignore = "requires javac/java; root runs serially"]
fn jvm_exit_span_preserves_zero_iterations_identity_and_original_effect_order() {
    let text = source(&fixture()).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-loop-exit-span-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let file = dir.join("sample/ExitSpan.java");
    fs::write(&file,format!(r#"package sample;public class ExitSpan{{
{text}
static Object oracle(int n,Object value){{int counter=0;Effects.seed();Object result;while(counter<n){{Effects.touch(counter);result=value;counter++;if(result==null)continue;return result;}}return null;}}
public static void main(String[] args){{
 Object token=new Object();
 for(int n=-1;n<6;n++)for(Object value:new Object[]{{null,token}})for(int fault=-2;fault<6;fault++){{
  Effects.reset(fault);Object expected=null;RuntimeException thrown=null;
  try{{expected=oracle(n,value);}}catch(RuntimeException e){{thrown=e;}}
  String trace=Effects.trace;Effects.reset(fault);
  try{{Object actual=run(n,value);if(thrown!=null||actual!=expected)throw new AssertionError("result");}}
  catch(RuntimeException e){{if(e!=thrown||e!=Effects.sentinel)throw new AssertionError(e);}}
  if(!trace.equals(Effects.trace))throw new AssertionError(trace+" != "+Effects.trace);
 }}
}}
}}
class Effects{{static String trace;static int fail;static final RuntimeException sentinel=new RuntimeException();
static void reset(int at){{trace="";fail=at;}}
static int seed(){{trace+="S";if(fail==-2)throw sentinel;return 123;}}
static void touch(int n){{trace+="T"+n;if(fail==n)throw sentinel;}}
}}"#)).unwrap();
    let c = Command::new("javac").arg(&file).output().unwrap();
    assert!(c.status.success(), "{}", String::from_utf8_lossy(&c.stderr));
    let r = Command::new("java")
        .args(["-cp", dir.to_str().unwrap(), "sample.ExitSpan"])
        .output()
        .unwrap();
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
}
