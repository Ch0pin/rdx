use rdx::native_ssa;
use rdx::{
    native_dex::{self, DexClass, DexField, DexSymbols},
    native_java,
};
use std::sync::Arc;
fn conditional_fixture() -> DexClass {
    let mut c = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    c.descriptor = "Lsample/Reader;".into();
    c.superclass = Some("Ljava/lang/Object;".into());
    c.interfaces.clear();
    c.fields = vec![
        DexField {
            declaring_type: c.descriptor.clone(),
            name: "b".into(),
            field_type: "Z".into(),
            access_flags: 1,
            is_static: false,
        },
        DexField {
            declaring_type: c.descriptor.clone(),
            name: "c".into(),
            field_type: "Ljava/io/InputStream;".into(),
            access_flags: 1,
            is_static: false,
        },
    ];
    c.methods.retain(|m| m.name.as_ref() == "answer");
    let m = &mut c.methods[0];
    m.name = "read".into();
    m.declaring_type = c.descriptor.clone();
    m.return_type = "I".into();
    m.parameters.clear();
    m.thrown_types = vec!["Ljava/io/IOException;".into()];
    m.access_flags = 1;
    c.symbols = Arc::new(DexSymbols {
        strings: vec!["a".into(), "b".into(), "read".into(), "c".into()],
        types: vec![
            "Lsample/Reader;".into(),
            "Lsample/Node;".into(),
            "Ljava/io/InputStream;".into(),
            "Z".into(),
        ],
        protos: vec![
            ("Lsample/Node;".into(), vec![]),
            ("Ljava/io/InputStream;".into(), vec![]),
            ("I".into(), vec![]),
        ],
        methods: vec![(0, 0, 0), (1, 1, 1), (2, 2, 2)],
        fields: vec![(0, 3, 1), (0, 2, 3)],
        ..Default::default()
    });
    let code = m.code.as_mut().unwrap();
    code.registers = 4;
    code.ins = 1;
    code.outs = 1;
    code.instructions = vec![
        0x3054, 1, 0xf112, 0x0039, 23, 0x3055, 0, 0x0039, 3, 0x0728, 0x106e, 0, 3, 0x000c, 0x0039,
        3, 0x010f, 0x0212, 0x325c, 0, 0x1072, 1, 0, 0x000c, 0x305b, 1, 0x3054, 1, 0x106e, 2, 0,
        0x000a, 0x003a, 3, 0x000f, 0x106e, 0, 3, 0x000c, 0x0039, 0xffed, 0x0012, 0x305b, 1, 0x010f,
    ];
    c
}
#[test]
fn unconditional_entry_copy_retains_terminal_branches_before_feedback() {
    let c = fixture();
    let r = native_java::render_method("sample.Reader", &c, &c.methods[0]).unwrap();
    assert!(r.source.contains("while (true)"), "{}", r.source);
    assert!(
        r.links
            .iter()
            .filter(|link| link.label == "java.io.InputStream.read()I")
            .count()
            >= 2
    );
}
#[test]
#[ignore = "requires JDK25 via RDX_JAVA25_HOME"]
fn terminal_entry_dag_jvm_matches_original_stream_loop() {
    use std::{fs, process::Command};
    struct TempDirectory(std::path::PathBuf);
    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let home = std::env::var("RDX_JAVA25_HOME").unwrap();
    for close in [false, true] {
        let c = if close {
            fixture_with_close()
        } else {
            fixture()
        };
        let r = native_java::render_method("sample.Reader", &c, &c.methods[0]).unwrap();
        let dir = std::env::temp_dir().join(
            format!("rdx-terminal-entry-dag-{}", std::process::id()).to_string()
                + if close { "-close" } else { "-read" },
        );
        let _cleanup = TempDirectory(dir.clone());
        fs::create_dir_all(dir.join("sample")).unwrap();
        let source = r#"package sample;import java.io.InputStream;
interface Node {InputStream b();}
public class Reader {
boolean b;InputStream c;static String trace="";static final RuntimeException fault=new RuntimeException();int[][] scripts;int next;int fail;
Reader(boolean first,int[] initial,int[][] scripts){b=first;this.scripts=scripts;c=initial==null?null:stream(initial,"old");}
InputStream stream(int[] values,String label){return new InputStream(){int index;public int read(){trace+="read"+label+",";int x=index<values.length?values[index++]:-1;if(fail==3||x==Integer.MIN_VALUE)throw fault;return x;}public void close(){trace+="close"+label+",";if(fail==4)throw fault;}};}
Node a(){trace+="next,";if(fail==1)throw fault;if(next==scripts.length)return null;int[] values=scripts[next];String label="n"+(next++);return ()->{trace+="open"+label+",";if(fail==2)throw fault;return stream(values,label);};}
METHOD
int oracle() throws java.io.IOException {if(c==null){if(!b)return -1;Node n=a();if(n==null)return -1;b=false;c=n.b();}while(true){int value=c.read();if(value>=0)return value;if(CLOSE)c.close();Node n=a();if(n==null){c=null;return -1;}c=n.b();}}
static void run(boolean first,int[] initial,int[][] scripts,int fail) throws Exception {Reader actual=new Reader(first,initial,scripts),expected=new Reader(first,initial,scripts);actual.fail=expected.fail=fail;for(int i=0;i<5;i++){int a=0,e=0;boolean ax=false,ex=false;trace="";try{a=actual.read();}catch(RuntimeException x){if(x!=fault)throw new AssertionError();ax=true;}String at=trace;trace="";try{e=expected.oracle();}catch(RuntimeException x){if(x!=fault)throw new AssertionError();ex=true;}if(a!=e||ax!=ex||actual.b!=expected.b||(actual.c==null)!=(expected.c==null)||!at.equals(trace))throw new AssertionError(at+" != "+trace);if(ax||a<0)break;}}
public static void main(String[] args) throws Exception {for(boolean first:new boolean[]{false,true})for(int[] initial:new int[][]{null,new int[]{-1},new int[]{3,-1},new int[]{Integer.MIN_VALUE}})for(int[][] scripts:new int[][][]{new int[][]{},new int[][]{{-1}},new int[][]{{-1},{4,-1}},new int[][]{{1,-1},{2,-1}},new int[][]{{Integer.MIN_VALUE}}})for(int fail=0;fail<=4;fail++)run(first,initial,scripts,fail);}
}"#;
        fs::write(
            dir.join("sample/Reader.java"),
            source
                .replace("METHOD", &r.source)
                .replace("CLOSE", if close { "true" } else { "false" }),
        )
        .unwrap();
        for (program, args) in [
            ("javac", vec!["sample/Reader.java"]),
            ("java", vec!["-cp", ".", "sample.Reader"]),
        ] {
            let o = Command::new(format!("{home}/bin/{program}"))
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(
                o.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&o.stderr),
                r.source
            );
        }
    }
}

fn fixture_with_close() -> DexClass {
    let mut c = fixture();
    let symbols = Arc::get_mut(&mut c.symbols).unwrap();
    symbols.strings.push("close".into());
    symbols.protos.push(("V".into(), vec![]));
    symbols.methods.push((2, 3, 4));
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions.splice(37..37, [0x3254, 1, 0x106e, 3, 2]);
    code.instructions[53] = 0xffe2;
    c
}
#[test]
fn undefined_first_entry_protected_overlap_and_foreign_interior_entries_decline() {
    let mut c = conditional_fixture();
    c.methods[0].code.as_mut().unwrap().instructions[2] = 0;
    assert!(native_java::render_method("sample.Reader", &c, &c.methods[0]).is_err());
    let mut c = conditional_fixture();
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions.extend([0x020d, 0x0227]);
    code.tries = 1;
    code.try_regions = vec![native_dex::DexTryRegion {
        start: 26,
        end: 32,
        catches: Arc::from(vec![(None, 45)]),
    }];
    assert!(native_java::render_method("sample.Reader", &c, &c.methods[0]).is_err());
    let mut c = conditional_fixture();
    c.methods[0].code.as_mut().unwrap().instructions[4] = 28; // enters move-result at31
    assert!(native_java::render_method("sample.Reader", &c, &c.methods[0]).is_err());
}

pub use rdx::{native_cfg, native_dominators, native_ir};
#[path = "../src/native_java/forward_iteration_entry.rs"]
mod iteration;
#[test]
fn canonical_terminal_copy_plan_retains_original_operands_and_exact_edges() {
    let c = fixture();
    let code = c.methods[0].code.as_ref().unwrap();
    let p = iteration::select(code).unwrap();
    assert_eq!((p.header, p.latch), (22, 47));
    let copy = p.copies.iter().find(|c| c.from == 5).unwrap();
    assert_eq!(copy.partial_latch, None);
    assert_eq!(copy.target, 28);
    assert!(copy.instructions.contains(&36) && copy.instructions.contains(&46));
    assert!(p.edges.contains(&(5, 28, iteration::EdgeOwner::EntryCopy)));
    assert!(p.edges.contains(&(3, 7, iteration::EdgeOwner::Original)));
    assert!(
        p.edges
            .contains(&(47, 22, iteration::EdgeOwner::HeaderContinue))
    );
    assert!(p.edges.contains(&(41, 43, iteration::EdgeOwner::Terminal)));
    let mut bad = p.clone();
    bad.edges.retain(|&(f, t, _)| f != 3 || t != 7);
    assert!(!iteration::validate(code, &bad));
    let mut bad = p.clone();
    bad.copies[0].partial_latch = Some(47);
    assert!(!iteration::validate(code, &bad));
    let mut bad = p.clone();
    bad.words[30] = 1;
    assert!(!iteration::validate(code, &bad));
}

fn fixture() -> DexClass {
    let mut c = conditional_fixture();
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions[3] = 0x0038;
    code.instructions[4] = 4;
    code.instructions.splice(5..5, [0x0029, 23]);
    code.instructions[41] = 0x0039;
    code.instructions[42] = 6;
    code.instructions.extend([0x0029, 0xffe7]);
    c
}

#[test]
fn a_terminal_leaf_after_feedback_needs_a_separate_copy_boundary_proof() {
    let mut c = fixture();
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions.splice(
        41..49,
        [0x0038, 4, 0x0029, 0xffeb, 0x0012, 0x305b, 1, 0x010f],
    );
    assert!(iteration::select(code).is_none());
    assert!(native_java::render_method("sample.Reader", &c, &c.methods[0]).is_err());
}
