use rdx::{
    native_call_values::{CallValues, SsaCalls, TypedValue},
    native_cfg::ControlFlowGraph,
    native_dex::{DexCode, DexMethod, DexSymbols},
    native_ir::DecodedMethod,
    native_ssa::SsaMethod,
    native_types::{AssignmentBound, InferredTypes},
};
use std::sync::Arc;
fn method(words: &[u16], regs: u16, params: &[&str], ret: &str) -> DexMethod {
    DexMethod {
        declaring_type: Arc::from("LTest;"),
        name: Arc::from("m"),
        return_type: Arc::from(ret),
        parameters: params.iter().map(|s| Arc::from(*s)).collect(),
        thrown_types: vec![],
        access_flags: 8,
        code: Some(DexCode {
            registers: regs,
            ins: params
                .iter()
                .map(|s| if matches!(*s, "J" | "D") { 2 } else { 1 })
                .sum(),
            outs: 0,
            tries: 0,
            try_regions: vec![],
            instructions: words.to_vec(),
            offset: 0,
        }),
    }
}
fn run(m: &DexMethod, symbols: &DexSymbols) -> (SsaMethod, InferredTypes) {
    let c = m.code.as_ref().unwrap();
    let ir = DecodedMethod::decode(c).unwrap();
    let ssa = SsaMethod::build(c, &ir, &ControlFlowGraph::build(c).unwrap()).unwrap();
    let types = InferredTypes::infer(m, &ir, &ssa, &SsaCalls::default(), symbols).unwrap();
    (ssa, types)
}
fn written(s: &SsaMethod, pc: usize) -> usize {
    s.instructions.iter().find(|i| i.pc == pc).unwrap().writes[0].words[0]
}

fn requirements(value: usize, count: usize) -> SsaCalls {
    SsaCalls {
        calls: (0..count)
            .map(|n| CallValues {
                pc: 0,
                receiver: None,
                arguments: vec![TypedValue {
                    descriptor: format!("Lsample/Use{n};").into(),
                    words: vec![value],
                }],
                result: None,
            })
            .collect(),
        unreachable_calls: 0,
    }
}
fn infer(m: &DexMethod, ssa: &SsaMethod, calls: &SsaCalls, symbols: &DexSymbols) -> InferredTypes {
    InferredTypes::infer(
        m,
        &DecodedMethod::decode(m.code.as_ref().unwrap()).unwrap(),
        ssa,
        calls,
        symbols,
    )
    .unwrap()
}
fn return_value(ssa: &SsaMethod) -> usize {
    ssa.instructions.last().unwrap().reads[0].words[0]
}
fn ref_domain(value: &rdx::native_types::ValueType) -> bool {
    !value.assignment_bounds_truncated
        && !value.assignment.is_empty()
        && value.assignment.iter().all(|bound| {
            matches!(bound,AssignmentBound::Type(t) if t.starts_with('L')||t.starts_with('['))
                || matches!(
                    bound,
                    AssignmentBound::Literal {
                        bits: 0,
                        wide: false
                    }
                )
        })
}
#[test]
fn requirement_overflow_keeps_complete_assignment_fixed_point_in_all_orders() {
    for words in [
        vec![0x1007, 0x0238, 5, 0x0007, 0x0029, 0xfffd, 0x0011],
        vec![0x0238, 5, 0x1007, 0x0029, 3, 0x0012, 0x0011],
    ] {
        let m = method(&words, 3, &["Lsample/Seed;", "Z"], "Ljava/lang/Object;");
        let (mut ssa, _) = run(&m, &DexSymbols::default());
        let value = return_value(&ssa);
        let mut calls = requirements(value, 12);
        let a = infer(&m, &ssa, &calls, &DexSymbols::default());
        assert!(a.values[value].bounds_truncated);
        assert!(!a.values[value].assignment_bounds_truncated);
        assert!(ref_domain(&a.values[value]));
        ssa.instructions.reverse();
        ssa.phis.reverse();
        for phi in &mut ssa.phis {
            phi.incoming.reverse();
        }
        calls.calls.reverse();
        let b = infer(&m, &ssa, &calls, &DexSymbols::default());
        for (a, b) in a.values.iter().zip(&b.values) {
            let mut aa = a.assignment.clone();
            let mut ba = b.assignment.clone();
            aa.sort();
            ba.sort();
            assert_eq!(aa, ba);
            assert_eq!(a.assignment_bounds_truncated, b.assignment_bounds_truncated);
        }
    }
}
#[test]
fn undefined_and_nonzero_phi_roots_remain_explicit_and_nonpositive() {
    for replacement in [0, 0x1012] {
        let m = method(
            &[0x0238, 5, replacement, 0x0029, 3, 0x0012, 0x0011],
            3,
            &["Lsample/Seed;", "Z"],
            "Ljava/lang/Object;",
        );
        let (ssa, _) = run(&m, &DexSymbols::default());
        let value = return_value(&ssa);
        let types = infer(&m, &ssa, &requirements(value, 12), &DexSymbols::default());
        assert!(types.values[value].bounds_truncated);
        assert!(!types.values[value].assignment_bounds_truncated);
        assert!(!ref_domain(&types.values[value]));
    }
}
fn alternatives(count: usize, arrays: bool) -> (DexMethod, DexSymbols) {
    let mut words = vec![];
    let mut gotos = vec![];
    for n in 0..count - 1 {
        words.extend([0x0113, n as u16, 0x3133, 7, 0x2007, 0x001f, n as u16]);
        gotos.push(words.len());
        words.extend([0x0029, 0]);
    }
    words.extend([0x2007, 0x001f, (count - 1) as u16]);
    let join = words.len();
    for go in gotos {
        words[go + 1] = (join - go) as u16;
    }
    if arrays {
        words.extend([0x0146, 0x0300, 0x0111]);
    } else {
        words.push(0x0011);
    }
    let symbols = DexSymbols {
        types: (0..count)
            .map(|n| format!("{}Lsample/Sub{n};", if arrays { "[" } else { "" }).into())
            .collect(),
        ..Default::default()
    };
    (
        method(
            &words,
            4,
            &["Ljava/lang/Object;", "I"],
            "Ljava/lang/Object;",
        ),
        symbols,
    )
}
#[test]
fn assignment_overflow_is_certified_through_phi_moves_and_array_components() {
    for arrays in [false, true] {
        for count in [8, 9] {
            let (m, symbols) = alternatives(count, arrays);
            let (mut ssa, a) = run(&m, &symbols);
            let value = return_value(&ssa);
            assert_eq!(a.values[value].assignment_bounds_truncated, count == 9);
            assert_eq!(a.values[value].assignment.len(), 8);
            let tainted = infer(&m, &ssa, &requirements(value, 12), &symbols);
            assert_eq!(
                tainted.values[value].assignment_bounds_truncated,
                count == 9
            );
            ssa.instructions.reverse();
            ssa.phis.reverse();
            for phi in &mut ssa.phis {
                phi.incoming.reverse();
            }
            let b = infer(&m, &ssa, &SsaCalls::default(), &symbols);
            assert_eq!(b.values[value].assignment_bounds_truncated, count == 9);
            assert_eq!(b.values[value].assignment.len(), 8);
        }
    }
}
#[test]
fn dynamic_array_unknown_and_nonreference_components_cannot_certify_reference_domain() {
    for ty in ["[I", "Ljava/lang/Object;", "[Ljava/lang/String;"] {
        let m = method(
            &[0x0046, 0x0201, 0x0011],
            3,
            &[ty, "I"],
            "Ljava/lang/Object;",
        );
        let (ssa, _) = run(&m, &DexSymbols::default());
        let value = return_value(&ssa);
        let a = infer(&m, &ssa, &requirements(value, 12), &DexSymbols::default());
        assert!(a.values[value].bounds_truncated);
        assert!(!a.values[value].assignment_bounds_truncated);
        assert_eq!(ref_domain(&a.values[value]), ty == "[Ljava/lang/String;");
    }
}
#[test]
fn original_cast_output_keeps_its_target_and_prior_alias_identity() {
    let m = method(
        &[0x1007, 0x001f, 0, 0x0011],
        2,
        &["Ljava/lang/Object;"],
        "Ljava/lang/Object;",
    );
    let symbols = DexSymbols {
        types: vec!["Lsample/Seed;".into()],
        ..Default::default()
    };
    let (ssa, _) = run(&m, &symbols);
    let value = return_value(&ssa);
    let a = infer(&m, &ssa, &requirements(value, 12), &symbols);
    assert_eq!(
        a.values[value].assignment,
        vec![AssignmentBound::Type("Lsample/Seed;".into())]
    );
    assert!(!a.values[value].assignment_bounds_truncated);
    assert_eq!(
        a.values[written(&ssa, 0)].assignment,
        vec![AssignmentBound::Type("Ljava/lang/Object;".into())]
    );
}

fn domain_fixture(mode: usize) -> Vec<rdx::native_dex::DexClass> {
    use rdx::native_dex::DexClass;
    let mut symbols = DexSymbols {
        types: vec![
            "Lsample/Target;".into(),
            "Lsample/Effects;".into(),
            "Ljava/lang/Object;".into(),
            "J".into(),
            "Lsample/Seed;".into(),
        ],
        strings: vec!["<init>".into(), "prefix".into()],
        protos: vec![
            ("V".into(), vec!["Ljava/lang/Object;".into(), "J".into()]),
            ("V".into(), vec![]),
        ],
        methods: vec![(0, 0, 0), (1, 1, 1)],
        ..Default::default()
    };
    for n in 0..9 {
        symbols.types.push(format!("Lsample/Use{n};").into());
        symbols.strings.push(format!("mark{n}"));
        symbols
            .protos
            .push(("V".into(), vec![format!("Lsample/Use{n};").into()]));
        symbols.methods.push((1, (n + 2) as u16, (n + 2) as u32));
    }
    symbols.strings.extend(["wide".into(), "observe".into()]);
    symbols.protos.extend([
        ("J".into(), vec![]),
        ("V".into(), vec!["Ljava/lang/Object;".into(), "J".into()]),
    ]);
    symbols.methods.extend([(1, 11, 11), (1, 12, 12)]);
    let symbols = Arc::new(symbols);
    let class = |name: &str, interfaces: Vec<Arc<str>>, flags| DexClass {
        descriptor: name.into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces,
        access_flags: flags,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![],
        symbols: symbols.clone(),
    };
    let mut classes = vec![
        class("Lsample/Caller;", vec![], 1),
        class(
            "Lsample/Seed;",
            (0..9).map(|n| format!("Lsample/Use{n};").into()).collect(),
            1,
        ),
    ];
    for n in 0..9 {
        classes.push(class(&format!("Lsample/Use{n};"), vec![], 0x601));
    }
    symbols
        .hierarchy
        .set(Arc::new(
            rdx::native_hierarchy::TypeHierarchy::from_classes(classes.iter()).unwrap(),
        ))
        .unwrap();
    let mut words = vec![0x5107, 0x0638, 3, 0x0112, 0x0022, 0, 0x0071, 1, 0];
    if mode != 0 {
        words.extend([0x011f, 4]);
    }
    if mode >= 2 {
        words.extend([0x1407, 0x0112]);
    }
    if mode == 3 {
        words.push(0x5407);
    }
    if mode == 4 {
        words.extend([0x0422, 4]);
    }
    let reg = if mode >= 2 { 4 } else { 1 };
    for n in 0..9 {
        words.extend([0x1071, (n + 2) as u16, reg]);
    }
    words.extend([
        0x0071,
        11,
        0,
        0x020b,
        0x4070,
        0,
        if mode >= 2 { 0x3240 } else { 0x3210 },
        0x3071,
        12,
        0x0320,
        0x0011,
    ]);
    let mut m = method(
        &words,
        7,
        &[
            if mode == 0 {
                "Lsample/Seed;"
            } else {
                "Ljava/lang/Object;"
            },
            "Z",
        ],
        "Lsample/Target;",
    );
    m.declaring_type = "Lsample/Caller;".into();
    m.name = "run".into();
    m.access_flags = 9;
    m.code.as_mut().unwrap().outs = 4;
    classes[0].methods.push(m);
    classes
}
#[test]
fn complete_domains_admit_staging_without_removing_original_cast_or_alias_effect() {
    for mode in 0..3 {
        let classes = domain_fixture(mode);
        let analysis =
            rdx::native_method::MethodAnalysis::build(&classes[0], &classes[0].methods[0]).unwrap();
        let types = analysis.infer_types().unwrap();
        let call = analysis
            .calls()
            .calls
            .iter()
            .find(|c| {
                c.arguments
                    .first()
                    .is_some_and(|a| a.descriptor.as_ref() == "Lsample/Use0;")
            })
            .unwrap();
        let value = &types.values[call.arguments[0].words[0]];
        assert!(value.bounds_truncated);
        assert!(!value.assignment_bounds_truncated);
        let output =
            rdx::native_java::render_method("sample.Caller", &classes[0], &classes[0].methods[0])
                .unwrap();
        assert_eq!(output.source.matches(".mark").count(), 9);
        assert!(output.source.contains("switch (0)"));
        if mode != 0 {
            assert!(output.source.contains("sample.Seed"));
        }
    }
    for mode in [3, 4] {
        let classes = domain_fixture(mode);
        assert!(
            rdx::native_java::render_method("sample.Caller", &classes[0], &classes[0].methods[0])
                .is_err(),
            "redefined alias or uninitialized allocation must remain rejected: {mode}"
        );
    }
}
#[test]
fn array_store_requirement_taint_does_not_drop_stored_assignment_and_wide_issue_stays() {
    let (mut m, symbols) = alternatives(9, true);
    m.parameters.push("Lsample/Seed;".into());
    let code = m.code.as_mut().unwrap();
    code.registers = 5;
    code.ins = 3;
    let at = code.instructions.len() - 3;
    code.instructions
        .splice(at.., [0x4107, 0x014d, 0x0300, 0x0111]);
    let (ssa, types) = run(&m, &symbols);
    let value = return_value(&ssa);
    assert!(types.values[value].bounds_truncated);
    assert!(!types.values[value].assignment_bounds_truncated);
    assert_eq!(
        types.values[value].assignment,
        vec![AssignmentBound::Type("Lsample/Seed;".into())]
    );
    let m = method(&[0x0016, 7, 0x1112, 0x0010], 2, &[], "J");
    let (_, types) = run(&m, &DexSymbols::default());
    assert!(types.wide_pair_issues > 0);
}
struct Temp(std::path::PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[test]
#[ignore = "requires JDK25; unchanged source versus independent raw register model"]
fn complete_assignment_staging_matches_effect_fault_alias_cast_null_and_wide_oracle() {
    let home = std::env::var_os("RDX_JAVA25_HOME").map(std::path::PathBuf::from);
    let tool = |n: &str| {
        home.as_ref()
            .map(|p| p.join("bin").join(n))
            .unwrap_or_else(|| n.into())
    };
    for mode in 0..3 {
        let classes = domain_fixture(mode);
        let m = &classes[0].methods[0];
        let source = rdx::native_java::render_method("sample.Caller", &classes[0], m)
            .unwrap()
            .source;
        let dir = Temp(std::env::temp_dir().join(format!(
            "rdx-assignment-complete-{}-{mode}",
            std::process::id()
        )));
        std::fs::create_dir_all(dir.0.join("sample")).unwrap();
        std::fs::write(
            dir.0.join("sample/Caller.java"),
            format!("package sample;public class Caller{{{source}}}"),
        )
        .unwrap();
        let interfaces = (0..9)
            .map(|n| format!("interface Use{n}{{}}\n"))
            .collect::<String>();
        let marks = (0..9)
            .map(|n| format!("static void mark{n}(Use{n} v){{mark({n},v);}}\n"))
            .collect::<String>();
        let implements = (0..9)
            .map(|n| format!("Use{n}"))
            .collect::<Vec<_>>()
            .join(",");
        let harness = DOMAIN_HARNESS
            .replace("@INTERFACES@", &interfaces)
            .replace("@IMPLEMENTS@", &implements)
            .replace("@MARKS@", &marks)
            .replace("@MODE@", &mode.to_string())
            .replace("@INPUT@", if mode == 0 { "(Seed)input" } else { "input" })
            .replace(
                "@WORDS@",
                &m.code
                    .as_ref()
                    .unwrap()
                    .instructions
                    .iter()
                    .map(u16::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
            );
        std::fs::write(dir.0.join("sample/Main.java"), harness).unwrap();
        let c = std::process::Command::new(tool("javac"))
            .arg("-d")
            .arg(&dir.0)
            .arg(dir.0.join("sample/Caller.java"))
            .arg(dir.0.join("sample/Main.java"))
            .output()
            .unwrap();
        assert!(
            c.status.success(),
            "mode{mode}: {}\n{source}",
            String::from_utf8_lossy(&c.stderr)
        );
        let r = std::process::Command::new(tool("java"))
            .arg("-Xverify:all")
            .arg("-cp")
            .arg(&dir.0)
            .arg("sample.Main")
            .output()
            .unwrap();
        assert!(
            r.status.success(),
            "mode{mode}: {}\n{}\n{source}",
            String::from_utf8_lossy(&r.stdout),
            String::from_utf8_lossy(&r.stderr)
        );
        eprintln!("mode{mode}: {}", String::from_utf8_lossy(&r.stdout));
    }
}
const DOMAIN_HARNESS: &str = r#"package sample;
@INTERFACES@
class Seed implements @IMPLEMENTS@ {int id;Seed(int id){this.id=id;}}
class SubSeed extends Seed {SubSeed(int id){super(id);}}
class Effects {static final RuntimeException FAULT=new RuntimeException();static final Error ERROR=new Error();static StringBuilder trace;static int calls,fault;static boolean error,mutate;static Object saved;static long savedWide;static void reset(int f,boolean e,boolean m){trace=new StringBuilder();calls=0;fault=f;error=e;mutate=m;saved=null;savedWide=0;}
static int identity(Object v){return v==null?-99:v instanceof Seed?((Seed)v).id:-88;}static void event(String name,long value){trace.append(name).append(value).append(';');if(++calls==fault){if(error)throw ERROR;throw FAULT;}}static void prefix(){event("p",0);}static void mark(int n,Object v){event("m"+n,identity(v));if(mutate&&v!=null)((Seed)v).id++;}@MARKS@
static long wide(){event("w",0);return 0x1122334455667788L;}static void constructed(Object v,long w){saved=v;savedWide=w;event("t",identity(v));}static void observe(Object v,long w){event("o",w);}}
class Target {Object obj;long wide;Target(Object obj,long wide){this.obj=obj;this.wide=wide;Effects.constructed(obj,wide);}}
class ModelTarget {Object obj;long wide;ModelTarget(Object obj,long wide){this.obj=obj;this.wide=wide;}}
public class Main {static final int[]W={@WORDS@};static final Object TAIL=new Object();
static Object dex(Object input,boolean nil){Object[]r=new Object[7];r[5]=input;r[6]=nil?1:0;Object pending=null;for(int pc=0,work=0;;){if(++work>500)throw new AssertionError("model budget");int w=W[pc],op=w&255,d=w>>>8;switch(op){case 0x07:r[(w>>>8)&15]=r[w>>>12];pc++;break;case 0x38:pc+=((Number)r[d]).intValue()==0?(short)W[pc+1]:2;break;case 0x12:r[(w>>>8)&15]=null;pc++;break;case 0x22:r[d]=new Object();pc+=2;break;case 0x1f:r[d]=(Seed)r[d];pc+=2;break;case 0x71:int index=W[pc+1];if(index==1)Effects.prefix();else if(index==11)pending=Effects.wide();else if(index==12)Effects.observe(r[W[pc+2]&15],((Number)r[(W[pc+2]>>>4)&15]).longValue());else Effects.mark(index-2,r[W[pc+2]&15]);pc+=3;break;case 0x0b:r[d]=pending;r[d+1]=TAIL;pending=null;pc++;break;case 0x70:int args=W[pc+2];Object value=r[(args>>>4)&15];long wide=((Number)r[(args>>>8)&15]).longValue();Effects.constructed(value,wide);r[args&15]=new ModelTarget(value,wide);pc+=3;break;case 0x11:return r[d];default:throw new AssertionError("opcode"+op);}}}
static String outcome(boolean model,int kind,int id,boolean nil,int fault,boolean error,boolean mutate){Object input=kind==0?new Seed(id):kind==1?new SubSeed(id):new Object();Effects.reset(fault,error,mutate);try{Object result=model?dex(input,nil):Caller.run(@INPUT@,nil);Object value=model?((ModelTarget)result).obj:((Target)result).obj;long wide=model?((ModelTarget)result).wide:((Target)result).wide;return (value==input)+","+Effects.identity(value)+","+wide+"|"+Effects.trace+"|"+(Effects.saved==input)+","+Effects.identity(Effects.saved)+","+Effects.savedWide;}catch(Throwable t){return (t==Effects.FAULT||t==Effects.ERROR?"identity":t.getClass().getName())+"|"+Effects.trace+"|"+(Effects.saved==input)+","+Effects.identity(Effects.saved)+","+Effects.savedWide;}}
public static void main(String[] args){int count=0;for(int kind=0;kind<3;kind++){if(@MODE@==0&&kind==2)continue;for(int id:new int[]{-1,0,3})for(boolean nil:new boolean[]{false,true})for(int fault=0;fault<=14;fault++)for(boolean error:new boolean[]{false,true})for(boolean mutate:new boolean[]{false,true}){String want=outcome(true,kind,id,nil,fault,error,mutate),got=outcome(false,kind,id,nil,fault,error,mutate);if(!want.equals(got))throw new AssertionError(kind+","+id+","+nil+","+fault+","+error+","+mutate+" want "+want+" got "+got);count++;}}System.out.println(count+" exact DEX-register comparisons");}}
"#;

#[test]
fn budget_failure_and_hierarchy_output_selection_do_not_claim_missing_assignments() {
    let m = method(
        &[0x0338, 5, 0x1007, 0x0029, 3, 0x2007, 0x0011],
        4,
        &["Lsample/Seed;", "Ljava/lang/Object;", "Z"],
        "Ljava/lang/Object;",
    );
    let classes = domain_fixture(0);
    let symbols = &classes[0].symbols;
    let (ssa, a) = run(&m, symbols);
    let value = return_value(&ssa);
    assert_eq!(a.values[value].assignment.len(), 2);
    assert_eq!(
        a.values[value].resolution,
        rdx::native_types::TypeResolution::Resolved("Ljava/lang/Object;".into())
    );
    assert!(!a.values[value].assignment_bounds_truncated);
    let ir = DecodedMethod::decode(m.code.as_ref().unwrap()).unwrap();
    assert!(
        InferredTypes::infer_with_work_limit(&m, &ir, &ssa, &requirements(value, 12), symbols, 1)
            .is_err()
    );
    assert!(
        InferredTypes::infer_with_work_limit(
            &m,
            &ir,
            &ssa,
            &requirements(value, 12),
            symbols,
            10000
        )
        .is_ok()
    );
}
