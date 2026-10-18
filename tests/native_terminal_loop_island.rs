use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_java,
};
use std::{fs, process::Command, sync::Arc};
#[path = "../src/native_java/terminal_loop_island.rs"]
mod proof;
pub use rdx::{native_cfg, native_dex, native_dominators};
fn fixture() -> DexClass {
    let descriptor: Arc<str> = "Lsample/Island;".into();
    DexClass {
        descriptor: descriptor.clone(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            types: vec![descriptor.clone()],
            strings: vec!["touch".into()],
            protos: vec![("V".into(), vec!["I".into()])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: descriptor,
            name: "run".into(),
            access_flags: 9,
            return_type: "I".into(),
            parameters: vec!["I".into(), "Z".into()],
            thrown_types: vec![],
            code: Some(DexCode {
                registers: 4,
                ins: 2,
                outs: 1,
                tries: 0,
                try_regions: vec![],
                offset: 0,
                instructions: vec![
                    0x0012, // @0 counter=0
                    0x0238, 15, // @1 count==0 -> @16
                    0x0339, 9, // @3 chosen -> terminal island @12 after the latch
                    0x00d8, 0x0100, // @5 counter++
                    0x02d8, 0xff02, // @7 count--
                    0x023c, 0xfff8, // @9 count>0 -> @1
                    0x0528, // @11 fallthrough tail -> @16
                    0x1071, 0, 0,      // @12 touch(counter)
                    0x000f, // @15 return counter
                    0xf012, // @16 guard exit counter=-1
                    0x000f, // @17 return counter
                ],
            }),
        }],
    }
}
fn source(c: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.Island", c, &c.methods[0])?.source)
}
#[test]
fn canonical_cycle_excludes_physically_interleaved_terminal_island() {
    let c = fixture();
    let code = c.methods[0].code.as_ref().unwrap();
    assert!(proof::admits(code, 1..16, 3, 12));
    assert!(!proof::admits(code, 1..16, 3, 5));
    assert!(!proof::admits(code, 1..16, 1, 12));
    assert!(!proof::admits(code, 1..16, 3, 16));
    let text = source(&c).unwrap();
    assert!(
        text.contains("touch(") && text.contains("while ("),
        "{text}"
    );
}
#[test]
fn reentry_nontermination_monitor_and_pending_result_decline() {
    for shape in 0..4 {
        let mut c = fixture();
        let code = c.methods[0].code.as_mut().unwrap();
        match shape {
            0 => code.instructions[15] = 0xf228, // island backedge -> loop header
            1 => code.instructions[15] = 0xfd28, // island backedge -> itself
            2 => {
                code.instructions[12] = 0x001d;
                code.instructions[13] = 0;
                code.instructions[14] = 0;
            }
            _ => {
                code.instructions[12] = 0x000a;
                code.instructions[13] = 0;
                code.instructions[14] = 0;
            }
        }
        assert!(!proof::admits(code, 1..16, 3, 12), "shape {shape}");
    }
}
#[test]
#[ignore = "requires javac and java"]
fn jvm_terminal_island_keeps_iteration_values_effect_count_and_exception_identity() {
    let text = source(&fixture()).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-terminal-island-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let file = dir.join("sample/Island.java");
    fs::write(&file,format!(r#"package sample; public class Island {{
{text}
static int calls; static boolean fail; static final RuntimeException sentinel=new RuntimeException();
static void touch(int value) {{calls++; if(value!=0) throw new AssertionError(value); if(fail)throw sentinel;}}
public static void main(String[] args) {{
 for(int n=0;n<=12;n++) for(boolean chosen:new boolean[]{{false,true}}) {{
  calls=0;fail=false;
  if(run(n,chosen)!=(n>0&&chosen?0:-1)||calls!=(n>0&&chosen?1:0))throw new AssertionError(n+":"+chosen+":"+calls);
 }}
 calls=0;fail=true; try{{run(4,true);throw new AssertionError();}}
 catch(RuntimeException e){{if(e!=sentinel||calls!=1)throw new AssertionError(e);}}
}}
}}"#)).unwrap();
    let c = Command::new("javac").arg(file).output().unwrap();
    assert!(c.status.success(), "{}", String::from_utf8_lossy(&c.stderr));
    let r = Command::new("java")
        .args(["-cp", dir.to_str().unwrap(), "sample.Island"])
        .output()
        .unwrap();
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
}
