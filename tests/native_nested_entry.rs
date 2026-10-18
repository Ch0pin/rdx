#[path = "../src/native_java/loop_entry_copy.rs"]
mod loop_entry_copy;
#[path = "../src/native_java/single_guard_entry.rs"]
mod single_guard_entry;
use rdx::{native_cfg, native_dominators, native_ir, native_ssa};
#[path = "../src/native_java/forward_iteration_entry.rs"]
mod forward_iteration_entry;
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
    m.parameters = vec!["[B".into(), "I".into(), "I".into()];
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
            ("I".into(), vec!["[B".into(), "I".into(), "I".into()]),
        ],
        methods: vec![(0, 0, 0), (1, 1, 1), (2, 2, 2)],
        fields: vec![(0, 3, 1), (0, 2, 3)],
        ..Default::default()
    });
    let code = m.code.as_mut().unwrap();
    code.registers = 8;
    code.ins = 4;
    code.outs = 4;
    code.instructions = vec![
        16468, 1, 274, 57, 22, 16469, 0, 57, 3, 11304, 4206, 0, 4, 12, 57, 3, 9512, 16732, 0, 4210,
        1, 0, 12, 16475, 1, 16468, 1, 656, 262, 913, 263, 16494, 2, 12880, 10, 58, 6, 432, 28979,
        65523, 271, 4206, 0, 4, 12, 57, 65510, 1298, 17755, 1, 5138, 16693, 4, 62482, 1039, 271,
    ];
    c
}

#[test]
fn nested_stream_read_renders() {
    let c = fixture();
    let r = native_java::render_method("sample.Reader", &c, &c.methods[0]).unwrap();
    assert!(
        r.links
            .iter()
            .filter(|link| link.label == "java.io.InputStream.read([BII)I")
            .count()
            >= 2
    );
}

#[test]
#[ignore = "requires JDK25 via RDX_JAVA25_HOME"]
fn nested_stream_jvm_matches_register_order_effects_and_fault_identity() {
    use std::{fs, process::Command};
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let c = fixture();
    let r = native_java::render_method("sample.Reader", &c, &c.methods[0]).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-nested-entry-{}", std::process::id()));
    let _cleanup = Cleanup(dir.clone());
    fs::create_dir_all(dir.join("sample")).unwrap();
    let source = r#"package sample;import java.io.InputStream;import java.util.Arrays;
interface Node {InputStream b();}
public class Reader {
boolean b;InputStream c;static String trace="";static final RuntimeException fault=new RuntimeException();int[][] scripts;int next;int fail;
Reader(boolean first,int[] initial,int[][] scripts){b=first;this.scripts=scripts;c=initial==null?null:stream(initial,99);}
InputStream stream(int[] values,int label){return new InputStream(){int index;public int read(){throw new AssertionError();}public int read(byte[] dst,int offset,int len){trace+="read"+label+":"+offset+":"+len+",";if(fail==3)throw fault;int x=index<values.length?values[index++]:-1;if(x==Integer.MIN_VALUE)throw fault;if(x<0)return -1;int n=Math.min(x,len);for(int i=0;i<n;i++)dst[offset+i]=(byte)(label+index+i);return n;}};}
Node a(){trace+="next,";if(fail==1)throw fault;if(next==scripts.length)return null;int label=next;int[] values=scripts[next++];return ()->{trace+="open"+label+",";if(fail==2)throw fault;return stream(values,label);};}
METHOD
int oracle(byte[] dst,int offset,int len) throws java.io.IOException {int count=0;if(c==null){if(!b)return -1;Node n=a();if(n==null)return -1;b=false;c=n.b();}while(true){while(true){int x=c.read(dst,offset+count,len-count);if(x<0)break;count+=x;if(count==len)return count;}Node n=a();if(n==null){c=null;return count<1?-1:count;}c=n.b();}}
static void run(boolean first,int[] initial,int[][] scripts,int fail,int len) throws Exception {Reader actual=new Reader(first,initial,scripts),expected=new Reader(first,initial,scripts);actual.fail=expected.fail=fail;for(int step=0;step<4;step++){byte[] a=new byte[10],e=new byte[10];Arrays.fill(a,(byte)77);Arrays.fill(e,(byte)77);int av=0,ev=0;boolean ax=false,ex=false;trace="";try{av=actual.read(a,2,len);}catch(RuntimeException x){if(x!=fault)throw new AssertionError();ax=true;}String at=trace;trace="";try{ev=expected.oracle(e,2,len);}catch(RuntimeException x){if(x!=fault)throw new AssertionError();ex=true;}if(av!=ev||ax!=ex||actual.b!=expected.b||(actual.c==null)!=(expected.c==null)||actual.next!=expected.next||!Arrays.equals(a,e)||!at.equals(trace))throw new AssertionError(at+" != "+trace+" / "+av+":"+ev);if(ax||av<0)break;}}
public static void main(String[] args) throws Exception {for(boolean first:new boolean[]{false,true})for(int[] initial:new int[][]{null,{-1},{0,1,1,-1},{3,-1},{Integer.MIN_VALUE}})for(int[][] scripts:new int[][][]{new int[][]{},{{-1}},{{0,1,-1},{2,-1}},{{1,1,-1},{3,-1}},{{Integer.MIN_VALUE}}})for(int fail=0;fail<=3;fail++)for(int len:new int[]{0,1,4})run(first,initial,scripts,fail,len);}
}"#;
    fs::write(
        dir.join("sample/Reader.java"),
        source.replace("METHOD", &r.source),
    )
    .unwrap();
    let home = std::env::var("RDX_JAVA25_HOME").unwrap();
    for (program, args) in [
        ("javac", vec!["sample/Reader.java"]),
        ("java", vec!["-cp", ".", "sample.Reader"]),
    ] {
        let output = Command::new(format!("{home}/bin/{program}"))
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stderr),
            r.source
        );
    }
}

#[test]
fn canonical_nested_ownership_and_ssa_reads_revalidate_exactly() {
    let c = fixture();
    let code = c.methods[0].code.as_ref().unwrap();
    let plan = forward_iteration_entry::select(code).unwrap();
    let inner = plan.nested.as_ref().unwrap();
    assert_eq!(
        (plan.header, plan.latch, inner.header, inner.latch),
        (19, 45, 25, 38)
    );
    assert_eq!(inner.instructions, vec![25, 27, 29, 31, 34, 35, 37, 38]);
    assert!(
        plan.edges
            .contains(&(38, 25, forward_iteration_entry::EdgeOwner::Original))
    );
    assert!(
        plan.edges
            .contains(&(45, 19, forward_iteration_entry::EdgeOwner::HeaderContinue))
    );
    assert!(
        plan.edges
            .contains(&(3, 25, forward_iteration_entry::EdgeOwner::EntryCopy))
    );
    assert!(
        plan.nested_ssa_reads
            .iter()
            .any(|(pc, register, _)| *pc == 31 && *register == 5)
    );
    assert!(forward_iteration_entry::validate(code, &plan));
    let mut wrong = plan.clone();
    wrong.nested.as_mut().unwrap().latch = 37;
    assert!(!forward_iteration_entry::validate(code, &wrong));
    let mut wrong = plan.clone();
    wrong.nested_ssa_reads[0].2[0] += 1;
    assert!(!forward_iteration_entry::validate(code, &wrong));
    let mut wrong = plan;
    wrong.edges.pop();
    assert!(!forward_iteration_entry::validate(code, &wrong));
}
#[test]
fn undefined_initial_inputs_foreign_body_entries_and_unowned_cycles_decline() {
    for (pc, word) in [
        (2, 0),
        (4, 24),
        (39, 0xfff5),
        (40, 0xf128),
        (25, 0x0022),
        (27, 0x029b),
    ] {
        let mut c = fixture();
        c.methods[0].code.as_mut().unwrap().instructions[pc] = word;
        assert!(
            forward_iteration_entry::select(c.methods[0].code.as_ref().unwrap()).is_none(),
            "{pc}"
        );
        assert!(
            native_java::render_method("sample.Reader", &c, &c.methods[0]).is_err(),
            "{pc}"
        );
    }
    let mut c = fixture();
    c.methods[0].code.as_mut().unwrap().tries = 1;
    assert!(forward_iteration_entry::select(c.methods[0].code.as_ref().unwrap()).is_none());
}

#[test]
fn inner_copy_budget_and_undefined_phi_operand_are_bounded() {
    let mut c = fixture();
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions[4] = 34; // initial edge into inner latch/body; only partial ownership
    assert!(forward_iteration_entry::select(code).is_none());
    let mut c = fixture();
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions[2] = 0x2101; // move v1 from undefined v2 before inner phi
    assert!(forward_iteration_entry::select(code).is_none());
    assert!(native_java::render_method("sample.Reader", &c, &c.methods[0]).is_err());
    let mut c = fixture();
    let code = c.methods[0].code.as_mut().unwrap();
    let padding = 25;
    code.instructions
        .splice(27..27, std::iter::repeat_n(0, padding));
    code.instructions[9] = (((53 + padding - 9) as u16) << 8) | 0x28;
    code.instructions[16] = (((53 + padding - 16) as u16) << 8) | 0x28;
    code.instructions[39 + padding] = (-13i16 - padding as i16) as u16;
    code.instructions[46 + padding] = (-26i16 - padding as i16) as u16;
    assert!(rdx::native_ir::DecodedMethod::decode(code).is_ok());
    assert!(rdx::native_cfg::ControlFlowGraph::build(code).is_ok());
    assert!(forward_iteration_entry::select(code).is_none());
}

#[test]
fn exception_regions_overlapping_the_inner_read_reject() {
    let mut c = fixture();
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions.extend([0x020d, 0x0227]);
    code.tries = 1;
    code.try_regions = vec![native_dex::DexTryRegion {
        start: 25,
        end: 35,
        catches: Arc::from(vec![(None, 56)]),
    }];
    assert!(forward_iteration_entry::select(code).is_none());
    assert!(native_java::render_method("sample.Reader", &c, &c.methods[0]).is_err());
}

#[test]
fn aggregate_entry_copy_cache_reproves_nested_metadata_and_raw_code() {
    let c = fixture();
    let code = c.methods[0].code.as_ref().unwrap();
    let plan = loop_entry_copy::select(code).unwrap();
    assert!(loop_entry_copy::validate(code, &plan));
    let mut wrong = plan.clone();
    wrong.iteration.as_mut().unwrap().nested_ssa_reads[0].2[0] += 1;
    assert!(!loop_entry_copy::validate(code, &wrong));
    let mut wrong = plan.clone();
    wrong
        .iteration
        .as_mut()
        .unwrap()
        .nested
        .as_mut()
        .unwrap()
        .instructions
        .pop();
    assert!(!loop_entry_copy::validate(code, &wrong));
    let mut wrong = plan.clone();
    wrong.copies[0].instructions.pop();
    assert!(!loop_entry_copy::validate(code, &wrong));
    let mut raw_class = fixture();
    let raw = raw_class.methods[0].code.as_mut().unwrap();
    raw.instructions[27] = 0x029b;
    assert!(!loop_entry_copy::validate(raw, &plan));
}
