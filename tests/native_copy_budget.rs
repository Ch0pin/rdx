pub use rdx::{native_cfg, native_dominators, native_ir, native_ssa};
#[path = "../src/native_java/forward_iteration_entry.rs"]
mod plan;
use rdx::{
    native_dex::{self, DexClass, DexSymbols, DexTryRegion},
    native_java,
};
use std::{fs, process::Command, sync::Arc};
fn fixture(extra: usize) -> DexClass {
    let mut c = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    c.descriptor = "Lsample/Entry;".into();
    c.methods.retain(|m| m.name.as_ref() == "answer");
    c.symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Effects;".into()],
        strings: vec![
            "prefix".into(),
            "alternate".into(),
            "tick".into(),
            "extra".into(),
        ],
        protos: vec![("V".into(), vec!["I".into()])],
        methods: vec![(0, 0, 0), (0, 0, 1), (0, 0, 2), (0, 0, 3)],
        ..Default::default()
    });
    let m = &mut c.methods[0];
    m.name = "run".into();
    m.access_flags = 9;
    m.return_type = "I".into();
    m.parameters = vec!["I".into(), "I".into()];
    m.declaring_type = c.descriptor.clone();
    let code = m.code.as_mut().unwrap();
    code.registers = 4;
    code.ins = 2;
    code.outs = 1;
    code.instructions = vec![
        0x0012, 0x0238, 6, 0x1071, 0, 2, 0x0828, 0x1071, 1, 2, 0xf928, 0x1071, 2, 0, 0x3034, 3,
        0x000f, 0x00d8, 0x0100, 0xf828,
    ];
    code.instructions.pop();
    code.instructions
        .splice(14..14, (0..extra).flat_map(|_| [0x1071, 3, 0]));
    let latch = code.instructions.len();
    code.instructions
        .extend([0x0029, (11isize - latch as isize) as i16 as u16]);
    c
}
#[test]
fn sixty_effects_fit_exact_copy_boundary_and_reproof() {
    let c = fixture(60);
    let code = c.methods[0].code.as_ref().unwrap();
    let proof = plan::select(code).unwrap();
    assert_eq!(proof.copies[0].instructions.len(), 64);
    assert!(plan::validate(code, &proof));
    let rendered = native_java::render_method("sample.Entry", &c, &c.methods[0]).unwrap();
    assert!(format!("{:?}", rendered.links).contains("sample.Effects.extra(I)V"));
    let mut bad = proof.clone();
    bad.copies[0].instructions.pop();
    assert!(!plan::validate(code, &bad));
    assert!(plan::select(fixture(61).methods[0].code.as_ref().unwrap()).is_none());
}
#[test]
fn actual_prefix_cycle_exception_overlap_and_undefined_entry_reject() {
    for mutation in 0..4 {
        let mut c = fixture(60);
        let code = c.methods[0].code.as_mut().unwrap();
        match mutation {
            0 => code.instructions[10] = 0xfd28,
            1 => {
                code.tries = 1;
                code.try_regions.push(DexTryRegion {
                    start: 3,
                    end: 6,
                    catches: vec![(None, 16)].into(),
                });
            }
            2 => code.instructions[0] = 0x0112,
            3 => code.instructions[2] = 9,
            _ => unreachable!(),
        }
        if mutation == 2 {
            assert!(
                native_java::render_method("sample.Entry", &c, &c.methods[0]).is_err(),
                "undefined entry"
            );
        } else {
            assert!(plan::select(code).is_none(), "mutation {mutation}");
        }
    }
}
struct Temp(std::path::PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
#[ignore = "requires javac/java; emitted source compiled unchanged"]
fn unchanged_java_preserves_prefix_order_first_iteration_and_fault_identity() {
    for extra in [0, 31, 60] {
        let c = fixture(extra);
        let source = native_java::render_method("sample.Entry", &c, &c.methods[0])
            .unwrap()
            .source;
        let dir = Temp(
            std::env::temp_dir().join(format!("rdx-copy-budget-{}-{extra}", std::process::id())),
        );
        fs::create_dir_all(dir.0.join("sample")).unwrap();
        fs::write(
            dir.0.join("sample/Entry.java"),
            format!("package sample; public class Entry {{ {source} }}"),
        )
        .unwrap();
        let words = c.methods[0]
            .code
            .as_ref()
            .unwrap()
            .instructions
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(",");
        fs::write(
            dir.0.join("sample/Main.java"),
            HARNESS.replace("@WORDS@", &words),
        )
        .unwrap();
        let home = std::env::var_os("RDX_JAVA25_HOME").map(std::path::PathBuf::from);
        let tool = |name: &str| {
            home.as_ref()
                .map(|p| p.join("bin").join(name))
                .unwrap_or_else(|| name.into())
        };
        let compile = Command::new(tool("javac"))
            .arg("-d")
            .arg(&dir.0)
            .arg(dir.0.join("sample/Entry.java"))
            .arg(dir.0.join("sample/Main.java"))
            .output()
            .unwrap();
        assert!(
            compile.status.success(),
            "{}\n{source}",
            String::from_utf8_lossy(&compile.stderr)
        );
        let run = Command::new(tool("java"))
            .arg("-Xverify:all")
            .arg("-cp")
            .arg(&dir.0)
            .arg("sample.Main")
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        eprintln!("extra={extra} {}", String::from_utf8_lossy(&run.stdout));
    }
}
const HARNESS: &str = r#"package sample;
class Effects{static final RuntimeException FAULT=new RuntimeException();static StringBuilder trace;static int fault,calls;static void effect(String label,int v){trace.append(label).append(v).append(';');if(++calls==fault)throw FAULT;}static void prefix(int v){effect("p",v);}static void alternate(int v){effect("a",v);}static void tick(int v){effect("t",v);}static void extra(int v){effect("e",v);}static void reset(int f){trace=new StringBuilder();calls=0;fault=f;}}
public class Main{static final int[] W={@WORDS@};
static int oracle(int mode,int limit){int[] r=new int[4];r[2]=mode;r[3]=limit;int pc=0,budget=10000;while(--budget>0){int w=W[pc],a=w>>>8;switch(w&255){case 0x12:{int v=a>>>4;r[a&15]=v>=8?v-16:v;pc++;break;}case 0x38:pc+=r[a]==0?(short)W[pc+1]:2;break;case 0x28:pc+=(byte)a;break;case 0x29:pc+=(short)W[pc+1];break;case 0x71:{int v=r[W[pc+2]&15];switch(W[pc+1]){case 0:Effects.prefix(v);break;case 1:Effects.alternate(v);break;case 2:Effects.tick(v);break;case 3:Effects.extra(v);break;default:throw new AssertionError();}pc+=3;break;}case 0x34:pc+=r[a&15]<r[a>>>4]?(short)W[pc+1]:2;break;case 0xd8:{int x=W[pc+1];r[a]=r[x&255]+(byte)(x>>>8);pc+=2;break;}case 0x0f:return r[a];default:throw new AssertionError();}}throw new AssertionError("budget");}
public static void main(String[]args){int n=0;for(int mode:new int[]{0,1,-1,99})for(int limit:new int[]{-1,0,1,2,5,20})for(int fault=0;fault<128;fault++){Effects.reset(fault);int a=0;Throwable x=null;try{a=Entry.run(mode,limit);}catch(Throwable t){x=t;}String trace=Effects.trace.toString();Effects.reset(fault);int b=0;Throwable y=null;try{b=oracle(mode,limit);}catch(Throwable t){y=t;}if(a!=b||x!=y||!trace.equals(Effects.trace.toString()))throw new AssertionError(mode+":"+limit+":"+fault+":"+trace+":"+Effects.trace);n++;}System.out.println(n+" exact DEX effect/fault comparisons");}}
"#;
