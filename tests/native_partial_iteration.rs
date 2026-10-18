use rdx::native_ssa;
use rdx::{
    native_dex::{self, DexClass, DexField, DexSymbols},
    native_java,
};
use std::sync::Arc;
fn fixture() -> DexClass {
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
fn conditional_entry_and_latch_reconstruct() {
    let c = fixture();
    let r = native_java::render_method("sample.Reader", &c, &c.methods[0]).unwrap();
    assert!(r.source.contains("while (true)"), "{}", r.source);
    println!("{}", r.source);
}
#[test]
#[ignore = "requires JDK25 via RDX_JAVA25_HOME"]
fn partial_first_iteration_jvm_matches_original_stream_loop() {
    use std::{fs, process::Command};
    let home = std::env::var("RDX_JAVA25_HOME").unwrap();
    for close in [false, true] {
        let c = if close {
            fixture_with_close()
        } else {
            fixture()
        };
        let r = native_java::render_method("sample.Reader", &c, &c.methods[0]).unwrap();
        let dir = std::env::temp_dir().join(
            format!("rdx-partial-iteration-{}", std::process::id()).to_string()
                + if close { "-close" } else { "-read" },
        );
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
        fs::remove_dir_all(dir).unwrap();
    }
}

fn fixture_with_close() -> DexClass {
    let mut c = fixture();
    let symbols = Arc::get_mut(&mut c.symbols).unwrap();
    symbols.strings.push("close".into());
    symbols.protos.push(("V".into(), vec![]));
    symbols.methods.push((2, 3, 4));
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions.splice(35..35, [0x3254, 1, 0x106e, 3, 2]);
    code.instructions[45] = 0xffe8;
    c
}
#[test]
fn undefined_first_entry_protected_overlap_and_foreign_interior_entries_decline() {
    let mut c = fixture();
    c.methods[0].code.as_mut().unwrap().instructions[2] = 0;
    assert!(native_java::render_method("sample.Reader", &c, &c.methods[0]).is_err());
    let mut c = fixture();
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions.extend([0x020d, 0x0227]);
    code.tries = 1;
    code.try_regions = vec![native_dex::DexTryRegion {
        start: 26,
        end: 32,
        catches: Arc::from(vec![(None, 45)]),
    }];
    assert!(native_java::render_method("sample.Reader", &c, &c.methods[0]).is_err());
    let mut c = fixture();
    c.methods[0].code.as_mut().unwrap().instructions[4] = 28; // enters move-result at31
    assert!(native_java::render_method("sample.Reader", &c, &c.methods[0]).is_err());
}

pub use rdx::{native_cfg, native_dominators, native_ir};
#[path = "../src/native_java/forward_iteration_entry.rs"]
mod iteration;
#[test]
fn canonical_partial_plan_retains_original_operands_and_exact_both_conditional_edges() {
    let c = fixture();
    let code = c.methods[0].code.as_ref().unwrap();
    let p = iteration::select(code).unwrap();
    assert_eq!((p.header, p.latch), (20, 39));
    let copy = p.copies.iter().find(|c| c.from == 3).unwrap();
    assert_eq!(copy.partial_latch, Some(39));
    assert_eq!(copy.target, 26);
    assert!(copy.instructions.contains(&34) && copy.instructions.contains(&44));
    assert!(p.edges.contains(&(3, 26, iteration::EdgeOwner::EntryCopy)));
    assert!(p.edges.contains(&(3, 5, iteration::EdgeOwner::Original)));
    assert!(
        p.edges
            .contains(&(39, 20, iteration::EdgeOwner::HeaderContinue))
    );
    assert!(p.edges.contains(&(39, 41, iteration::EdgeOwner::Terminal)));
    let mut bad = p.clone();
    bad.edges.retain(|&(f, t, _)| f != 3 || t != 5);
    assert!(!iteration::validate(code, &bad));
    let mut bad = p.clone();
    bad.copies[0].partial_latch = None;
    assert!(!iteration::validate(code, &bad));
    let mut bad = p.clone();
    bad.words[30] = 1;
    assert!(!iteration::validate(code, &bad));
}
