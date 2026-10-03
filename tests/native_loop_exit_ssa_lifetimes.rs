use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_java,
    native_method::MethodAnalysis,
};
use std::sync::Arc;
#[path = "../src/native_java/loop_exit_type.rs"]
mod proof;
pub use rdx::{native_cfg, native_ssa, native_types};

fn fixture() -> DexClass {
    DexClass {
        descriptor: "Lsample/Exit;".into(),
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
            declaring_type: "Lsample/Exit;".into(),
            name: "run".into(),
            access_flags: 9,
            return_type: "Ljava/lang/Object;".into(),
            parameters: vec!["I".into(), "Ljava/lang/Object;".into()],
            thrown_types: vec![],
            code: Some(DexCode {
                registers: 4,
                ins: 2,
                outs: 1,
                tries: 0,
                try_regions: vec![],
                offset: 0,
                // Integer r1 before loop; Object r1 at a body exit, zero at the
                // pure guard tail. Header r1 is never read before replacement.
                instructions: vec![
                    0x0012, 0x0071, 1, 0, 0x010a, 0x2035, 11, 0x1071, 0, 0, 0x3107, 0x0139, 6,
                    0x00d8, 0x0100, 0xf628, 0x0112, 0x0111,
                ],
            }),
        }],
    }
}
fn source(class: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.Exit", class, &class.methods[0])?.source)
}
fn check(class: &DexClass) -> Option<String> {
    let a = MethodAnalysis::build(class, &class.methods[0]).unwrap();
    let t = a.infer_types().unwrap();
    proof::exit_type(a.ssa(), &t, 5..17, 17, 1)
}

#[test]
fn reference_and_null_exit_are_independent_of_dead_integer_entry() {
    let class = fixture();
    assert_eq!(check(&class).as_deref(), Some("Ljava/lang/Object;"));
    let text = source(&class).unwrap();
    assert!(text.contains("Effects.seed()"), "{text}");
    assert!(text.contains("while (true)"), "{text}");
    assert!(
        text.lines()
            .any(|line| line.trim().starts_with("java.lang.Object ") && !line.contains('=')),
        "{text}"
    );
}

#[test]
fn absent_old_entry_is_allowed_only_when_every_exit_establishes_a_value() {
    let mut class = fixture();
    class.methods[0].code.as_mut().unwrap().instructions[1..5].fill(0);
    assert_eq!(check(&class).as_deref(), Some("Ljava/lang/Object;"));
    source(&class).unwrap();
    // Valid decode/SSA, but zero iterations now bypass every definition of r1.
    class.methods[0].code.as_mut().unwrap().instructions[16] = 0;
    assert!(check(&class).is_none());
    assert!(source(&class).is_err());
}

#[test]
fn nonzero_and_integer_predecessors_cannot_become_reference_exits() {
    for word in [0x1112, 0x2101, 0x0112] {
        let mut class = fixture();
        class.methods[0].code.as_mut().unwrap().instructions[16] = word;
        if word == 0x0112 {
            class.methods[0].code.as_mut().unwrap().instructions[10] = 0x2101;
        }
        assert!(check(&class).is_none());
        assert!(source(&class).is_err());
    }
}

#[test]
fn undefined_and_incompatible_outside_sources_copied_inside_decline() {
    for undefined in [true, false] {
        let mut class = fixture();
        let code = class.methods[0].code.as_mut().unwrap();
        if undefined {
            // Extra local r2 has no definition; parameters move to r3/r4.
            code.registers = 5;
            code.instructions[5] = 0x3035;
        }
        // r2 is either undefined or the genuine integer parameter. A move
        // inside the loop preserves that assignment domain; it cannot create
        // a reference type from its required uses.
        code.instructions[10] = 0x2107;
        let analysis = MethodAnalysis::build(&class, &class.methods[0]).unwrap();
        let types = analysis.infer_types().unwrap();
        assert_eq!(types.wide_pair_issues, 0);
        let id = analysis
            .ssa()
            .instructions
            .iter()
            .find(|i| i.pc == 10)
            .unwrap()
            .writes[0]
            .words[0];
        let expected = if undefined {
            native_types::AssignmentBound::Undefined
        } else {
            native_types::AssignmentBound::Type("I".into())
        };
        assert!(types.values[id].assignment.contains(&expected));
        assert!(check(&class).is_none());
        let error = source(&class).unwrap_err().to_string();
        assert!(
            error.contains(if undefined {
                "undefined register"
            } else {
                "move type mismatch"
            }),
            "{error}"
        );
    }
}

#[test]
fn cycles_without_an_established_root_and_truncated_domains_decline() {
    let class = fixture();
    let a = MethodAnalysis::build(&class, &class.methods[0]).unwrap();
    let mut ssa = native_ssa::SsaMethod::build(
        class.methods[0].code.as_ref().unwrap(),
        a.instructions(),
        a.blocks(),
    )
    .unwrap();
    let t = a.infer_types().unwrap();
    let phi = ssa
        .phis
        .iter_mut()
        .find(|phi| phi.register == 1 && ssa.graph.blocks[phi.block].start == 17)
        .unwrap();
    let id = phi.result;
    for (_, input) in &mut phi.incoming {
        *input = id;
    }
    assert!(proof::exit_type(&ssa, &t, 5..17, 17, 1).is_none());
    let mut t = t;
    t.values[id].bounds_truncated = true;
    assert!(proof::exit_type(a.ssa(), &t, 5..17, 17, 1).is_none());
    t.values[id].bounds_truncated = false;
    t.wide_pair_issues = 1;
    assert!(proof::exit_type(a.ssa(), &t, 5..17, 17, 1).is_none());
}

#[test]
fn missing_duplicate_extra_and_virtual_phi_predecessors_decline() {
    let class = fixture();
    let a = MethodAnalysis::build(&class, &class.methods[0]).unwrap();
    let types = a.infer_types().unwrap();
    for shape in 0..5 {
        let mut ssa = native_ssa::SsaMethod::build(
            class.methods[0].code.as_ref().unwrap(),
            a.instructions(),
            a.blocks(),
        )
        .unwrap();
        let phi = ssa
            .phis
            .iter_mut()
            .find(|phi| phi.register == 1 && ssa.graph.blocks[phi.block].start == 17)
            .unwrap();
        let item = phi.incoming[0];
        match shape {
            0 => {
                phi.incoming.pop();
            }
            1 => {
                phi.incoming.push(item);
            }
            2 => {
                phi.incoming.push((Some(0), item.1));
            }
            3 => {
                phi.incoming.push((None, item.1));
            }
            _ => {
                phi.incoming.push((Some(usize::MAX), item.1));
            }
        }
        assert!(
            proof::exit_type(&ssa, &types, 5..17, 17, 1).is_none(),
            "shape {shape}"
        );
    }
}

#[test]
fn primitive_exit_does_not_inherit_an_object_entry_type() {
    let mut class = fixture();
    let m = &mut class.methods[0];
    m.return_type = "I".into();
    let words = &mut m.code.as_mut().unwrap().instructions;
    words[1..5].copy_from_slice(&[0x3107, 0, 0, 0]);
    // Replace touch + move-object with a genuine int calculation and nops.
    words[7..11].copy_from_slice(&[0x0190, 0x0200, 0, 0]);
    words[17] = 0x010f;
    assert_eq!(check(&class).as_deref(), Some("I"));
    let text = source(&class).unwrap();
    assert!(
        text.lines()
            .any(|line| line.trim().starts_with("int ") && !line.contains('=')),
        "{text}"
    );
}

fn scalar_fixture(ty: &str, name: &str) -> DexClass {
    let mut class = fixture();
    let method = &mut class.methods[0];
    method.return_type = ty.into();
    method.name = name.into();
    method.parameters = vec!["I".into(), ty.into(), "Ljava/lang/Object;".into()];
    let code = method.code.as_mut().unwrap();
    code.registers = 5;
    code.ins = 3;
    code.instructions[1..5].copy_from_slice(&[0x4107, 0, 0, 0]);
    code.instructions[10] = 0x3101;
    code.instructions[11] = 0x0039;
    code.instructions[17] = 0x010f;
    class
}
#[test]
fn boolean_and_float_exit_domains_keep_scalar_bits() {
    for (ty, name) in [("Z", "truth"), ("F", "floating")] {
        let class = scalar_fixture(ty, name);
        assert_eq!(check(&class).as_deref(), Some(ty));
        source(&class).unwrap();
    }
}

fn swap_fixture() -> DexClass {
    let mut class = fixture();
    let m = &mut class.methods[0];
    m.name = "swap".into();
    let code = m.code.as_mut().unwrap();
    code.registers = 8;
    code.instructions = vec![
        0x0012, 0x0071, 1, 0, 0x010a, 0x0413, 10, 0x0513, 20, 0x6035, 17, 0x1071, 0, 4, 0x1071, 0,
        5, 0x7107, 0x0139, 9, 0x4201, 0x5401, 0x2501, 0x00d8, 0x0100, 0xf028, 0x0112, 0x0111,
    ];
    class
}
#[test]
fn loop_carried_phi_cycles_and_parallel_swaps_keep_their_existing_values() {
    let class = swap_fixture();
    let a = MethodAnalysis::build(&class, &class.methods[0]).unwrap();
    let t = a.infer_types().unwrap();
    assert_eq!(
        proof::exit_type(a.ssa(), &t, 9..27, 27, 1).as_deref(),
        Some("Ljava/lang/Object;")
    );
    source(&class).unwrap();
}

#[test]
#[ignore = "requires Java 25"]
fn jvm_exact_exit_values_definite_assignment_and_exception_order() {
    use std::process::Command;
    let class = fixture();
    let text = source(&class).unwrap();
    let mut primitive = fixture();
    let m = &mut primitive.methods[0];
    m.name = "number".into();
    m.return_type = "I".into();
    let w = &mut m.code.as_mut().unwrap().instructions;
    w[1..5].copy_from_slice(&[0x3107, 0, 0, 0]);
    w[7..11].copy_from_slice(&[0x0190, 0x0200, 0, 0]);
    w[17] = 0x010f;
    let primitive_text = source(&primitive).unwrap();
    let swap_text = source(&swap_fixture()).unwrap();
    let float_text = source(&scalar_fixture("F", "floating")).unwrap();
    let bool_text = source(&scalar_fixture("Z", "truth")).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-loop-exit-ssa-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    let file = dir.join("sample/Exit.java");
    std::fs::write(&file, format!(r#"package sample; public class Exit {{
{text}
{primitive_text}
{swap_text}
{float_text}
{bool_text}
public static void main(String[] args) {{
 Object object = new Object();
 for(int n=-2;n<=12;n++) for(Object value:new Object[]{{null,object}}) {{
  Effects.trace=""; Effects.throwAt=-1;
  Object actual=run(n,value); Object expected=n>0?value:null;
  String trace="S,"; int visits=value==null?Math.max(n,0):Math.min(Math.max(n,0),1);
  for(int i=0;i<visits;i++) trace+=i+",";
  if(actual!=expected || !Effects.trace.equals(trace)) throw new AssertionError(n+":"+Effects.trace);
  if(number(n,value)!=(n>0?n:0)) throw new AssertionError("primitive "+n);
 }}
 for(int n=0;n<=12;n++) {{
  Effects.trace=""; Effects.throwAt=-1;
  if(swap(n,null)!=null) throw new AssertionError("swap result");
  String trace="S,"; for(int i=0;i<n;i++) trace+=(i%2==0?"10,20,":"20,10,");
  if(!Effects.trace.equals(trace)) throw new AssertionError("swap "+n+":"+Effects.trace);
 }}
 for(int n=-2;n<=5;n++) {{
  Effects.throwAt=-1;
  for(boolean b:new boolean[]{{false,true}}) if(truth(n,b,object)!=(n>1&&b)) throw new AssertionError("boolean "+n);
  for(int bits:new int[]{{0,0x80000000,0x7fc00001,0xffc00017,0x3f800000}}) {{
   float f=Float.intBitsToFloat(bits), result=floating(n,f,object);
   if(Float.floatToRawIntBits(result)!=(n>1?bits:0)) throw new AssertionError("float "+n+":"+Integer.toHexString(bits));
  }}
 }}
 for(int at:new int[]{{0,2,7}}) {{
  Effects.trace=""; Effects.throwAt=at;
  try {{ run(10,null); throw new AssertionError(); }}
  catch(RuntimeException caught) {{ if(caught!=Effects.sentinel) throw new AssertionError(caught); }}
  String trace="S,"; for(int i=0;i<=at;i++) trace+=i+",";
  if(!Effects.trace.equals(trace)) throw new AssertionError(Effects.trace);
 }}
 Effects.trace=""; Effects.failSeed=true;
 try {{run(0,object); throw new AssertionError();}}
 catch(RuntimeException caught) {{if(caught!=Effects.sentinel || !Effects.trace.equals("S,")) throw new AssertionError(caught);}}
}}
}}
class Effects {{
 static String trace=""; static int throwAt=-1; static boolean failSeed=false;
 static final RuntimeException sentinel=new RuntimeException();
 static int seed() {{ trace+="S,"; if(failSeed) throw sentinel; return 73; }}
 static void touch(int n) {{ trace+=n+","; if(n==throwAt) throw sentinel; }}
}}
"#)).unwrap();
    let home =
        std::env::var("RDX_JAVA25_HOME").expect("set RDX_JAVA25_HOME to a JDK 25 installation");
    let javac = Command::new(format!("{home}/bin/javac"))
        .arg(&file)
        .output()
        .unwrap();
    assert!(
        javac.status.success(),
        "{}",
        String::from_utf8_lossy(&javac.stderr)
    );
    let java = Command::new(format!("{home}/bin/java"))
        .args(["-cp", dir.to_str().unwrap(), "sample.Exit"])
        .output()
        .unwrap();
    assert!(
        java.status.success(),
        "{}",
        String::from_utf8_lossy(&java.stderr)
    );
}
