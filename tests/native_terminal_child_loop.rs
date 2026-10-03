//! Natural parent extent and exact terminal-child exit frames.
use rdx::{
    native_cfg,
    native_dex::{self, DexClass, DexSymbols},
    native_dominators, native_ir, native_java,
};
use std::{fs, process::Command, sync::Arc};

#[path = "../src/native_java/terminal_child_loop.rs"]
mod terminal_child_loop;

fn fixture(kind: usize) -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class
        .methods
        .retain(|method| method.name.as_ref() == "answer");
    class.symbols = Arc::new(DexSymbols {
        strings: vec!["touch".into()],
        types: vec!["Lsample/Effects;".into()],
        protos: vec![("I".into(), vec!["I".into(), "I".into()])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let method = &mut class.methods[0];
    method.name = [
        "nested",
        "twoGuards",
        "terminal",
        "wide",
        "parentContinue",
        "prefixEffect",
    ][kind]
        .into();
    method.access_flags = 9;
    method.parameters = vec!["I".into(), "I".into()];
    method.return_type = if kind == 3 { "J" } else { "I" }.into();
    let code = method.code.as_mut().unwrap();
    code.registers = if kind == 3 { 8 } else { 6 };
    code.ins = 2;
    code.outs = 2;
    code.tries = 0;
    code.try_regions.clear();
    code.instructions = match kind {
        0 => vec![
            0x0012, 0x0212, 0x4035, 0x0010, 0x00d8, 0x0100, 0x0112, 0x5135, 0xfffb, 0x2071, 0,
            0x0010, 0x030a, 0x0290, 0x0302, 0x01d8, 0x0101, 0xf628, 0x020f,
        ],
        1 => vec![
            0x0012, 0x0212, 0x4035, 0x0012, 0x00d8, 0x0100, 0x0112, 0x5135, 0xfffb, 0x4135, 0xfff9,
            0x2071, 0, 0x0010, 0x030a, 0x0290, 0x0302, 0x01d8, 0x0101, 0xf428, 0x020f,
        ],
        2 => vec![
            0x0012, 0x0212, 0x4035, 0x0011, 0x00d8, 0x0100, 0x0112, 0x5135, 0xfffb, 0x2071, 0,
            0x0010, 0x030a, 0x0290, 0x0302, 0x01d8, 0x0101, 0x5134, 0xfff6, 0x020f,
        ],
        3 => vec![
            0x0012, 0x0216, 0, 0x6035, 0x0011, 0x00d8, 0x0100, 0x0112, 0x7135, 0xfffb, 0x2071, 0,
            0x0010, 0x040a, 0x4481, 0x029b, 0x0402, 0x01d8, 0x0101, 0xf528, 0x0210,
        ],
        4 => vec![
            0x0012, 0x0212, 0x4035, 0x0012, 0x00d8, 0x0100, 0x5032, 0xfffc, 0x0112, 0x5135, 0xfff9,
            0x2071, 0, 0x0010, 0x030a, 0x0290, 0x0302, 0x01d8, 0x0101, 0xf628, 0x020f,
        ],
        5 => vec![
            0x0012, 0x0212, 0x4035, 0x0016, 0x00d8, 0x0100, 0x0112, 0x2071, 0, 0x0010, 0x030a,
            0x0290, 0x0302, 0x5135, 0xfff5, 0x2071, 0, 0x0010, 0x030a, 0x0290, 0x0302, 0x01d8,
            0x0101, 0xf028, 0x020f,
        ],
        _ => unreachable!(),
    };
    class
}

fn source(kind: usize) -> String {
    let class = fixture(kind);
    native_java::render_method("sample.Hello", &class, &class.methods[0])
        .unwrap()
        .source
}

#[test]
fn proof_retains_original_backedges_and_natural_body_extent() {
    for kind in 0..6 {
        let class = fixture(kind);
        let code = class.methods[0].code.as_ref().unwrap();
        let plan = terminal_child_loop::select(code).unwrap();
        assert!(terminal_child_loop::validate(code, &plan));
        assert!(
            plan.child_header_continue_guards
                .iter()
                .all(|pc| plan.parent_backedges.contains(pc))
        );
        assert_eq!(
            plan.parent_backedges.len(),
            if kind == 1 || kind == 4 { 2 } else { 1 }
        );
        assert_eq!(plan.parent_body_end, plan.child_body_end);
        assert!(
            plan.parent_backedges
                .iter()
                .all(|pc| *pc < plan.child_latch)
        );
        assert_eq!(
            plan.child_header_continue_guards.len(),
            if kind == 1 { 2 } else { 1 }
        );
        assert_eq!(plan.terminal_fallthrough.is_some(), kind == 2);
        assert_eq!(
            plan.edges
                .iter()
                .filter(|e| e.owner == terminal_child_loop::EdgeOwner::ChildContinue)
                .count(),
            1
        );
        assert_eq!(
            plan.edges
                .iter()
                .filter(|e| e.owner == terminal_child_loop::EdgeOwner::ParentContinue)
                .count(),
            plan.parent_backedges.len()
        );
        let mut poisoned = plan.clone();
        poisoned.parent_body_end = plan.parent_backedges[0] + 2;
        assert!(!terminal_child_loop::validate(code, &poisoned));
        let mut poisoned = plan.clone();
        poisoned.edges.pop();
        assert!(!terminal_child_loop::validate(code, &poisoned));
        let mut poisoned = plan.clone();
        poisoned.child_latch = plan.parent_backedges[0];
        assert!(!terminal_child_loop::validate(code, &poisoned));
    }
}

#[test]
fn proof_rejects_unowned_entries_transfers_handlers_monitors_switches_and_allocations() {
    // Parent guard enters child interior; child latch targets a nondominating body.
    // A body branch to the ancestor is outside the selected header guard chain.
    for (pc, word) in [
        (3, 7),
        (17, 0xf828),
        (15, 0xf328),
        (4, 0x001d),
        (4, 0x0022),
        (4, 0x002b),
    ] {
        let mut fresh = fixture(0);
        let bad = fresh.methods[0].code.as_mut().unwrap();
        bad.instructions[pc] = word;
        assert!(
            terminal_child_loop::select(bad).is_none(),
            "mutation {pc}={word:#x}"
        );
    }
    let mut fresh = fixture(0);
    let bad = fresh.methods[0].code.as_mut().unwrap();
    bad.tries = 1;
    assert!(terminal_child_loop::select(bad).is_none());
    let mut fresh = fixture(0);
    let bad = fresh.methods[0].code.as_mut().unwrap();
    bad.try_regions = vec![native_dex::DexTryRegion {
        start: 9,
        end: 12,
        catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 18)].into(),
    }];
    assert!(terminal_child_loop::select(bad).is_none());
    let mut fresh = fixture(0);
    let bad = fresh.methods[0].code.as_mut().unwrap();
    bad.instructions.resize(2049, 0);
    assert!(terminal_child_loop::select(bad).is_none());
    // Valid instruction boundaries, but an entry can bypass the child header.
    let mut fresh = fixture(0);
    let bad = fresh.methods[0].code.as_mut().unwrap();
    bad.instructions[4] = 0x0438;
    bad.instructions[5] = 5;
    native_cfg::ControlFlowGraph::build(bad).unwrap();
    assert!(terminal_child_loop::select(bad).is_none());
    assert!(native_java::render_method("sample.Hello", &fresh, &fresh.methods[0]).is_err());
    // Three nested natural owners require a different, deeper frame proof.
    let mut fresh = fixture(0);
    let bad = fresh.methods[0].code.as_mut().unwrap();
    bad.instructions = vec![
        0x0012, 0x0112, 0x4035, 15, 0x00d8, 0x0100, 0x5135, 0xfffc, 0x0212, 0x5235, 0xfffd, 0x02d8,
        0x0102, 0xfc28, 0x01d8, 0x0101, 0xf628, 0x000f,
    ];
    native_cfg::ControlFlowGraph::build(bad).unwrap();
    assert!(terminal_child_loop::select(bad).is_none());
}

#[test]
fn emitted_loop_structure_keeps_effect_sites_and_both_guard_exits() {
    for kind in 0..6 {
        let java = source(kind);
        // Exact CFG ownership is checked above; emission may flatten the two
        // natural owners into one loop with explicit carry updates.
        let loop_count = java.matches("while (true)").count();
        assert!((1..=2).contains(&loop_count), "{java}");
        assert_eq!(
            java.matches("sample.Effects.touch(").count(),
            if kind == 5 { 2 } else { 1 },
            "{java}"
        );
        assert!(!java.contains("switch ("), "{java}");
    }
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn unchanged_source_jvm_preserves_parent_child_carries_trace_terminal_exit_and_exceptions() {
    let dir = std::env::temp_dir().join(format!("rdx-terminal-child-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let java = format!(
        r#"package sample;
public class Effects {{
  static java.util.List<String> trace = new java.util.ArrayList<>();
  static int failAt;
  static final RuntimeException sentinel = new RuntimeException("sentinel");
  public static int touch(int i,int j) {{ trace.add(i+":"+j); if(trace.size()==failAt) throw sentinel; return i*10+j; }}
  {} {} {} {} {} {}
  static long oracle(int kind,int outer,int inner) {{
    long sum=0;
    for(int i=0;i<outer;) {{ i++;
      if(kind==4 && i==inner) continue;
      if(kind==5) {{
        for(int j=0;;j++) {{ sum+=touch(i,j); if(j>=inner) break; sum+=touch(i,j); }}
        continue;
      }}
      int bound=kind==1 ? Math.min(inner,outer) : inner;
      for(int j=0;j<bound;j++) sum+=touch(i,j);
      if(kind==2 && inner>0) return sum;
    }}
    return sum;
  }}
  public static void main(String[] args) {{
    for(int outer=-2;outer<=8;outer++) for(int inner=-2;inner<=8;inner++)
      for(int kind=0;kind<6;kind++) for(int failure:new int[]{{0,1,3}}) {{
        trace.clear(); failAt=failure; long expected=0; RuntimeException expectedError=null;
        try {{ expected=oracle(kind,outer,inner); }} catch(RuntimeException e) {{ expectedError=e; }}
        java.util.List<String> expectedTrace=new java.util.ArrayList<>(trace);
        trace.clear(); long actual=0; RuntimeException actualError=null;
        try {{ actual=kind==0 ? nested(outer,inner) : kind==1 ? twoGuards(outer,inner) : kind==2 ? terminal(outer,inner) : kind==3 ? wide(outer,inner) : kind==4 ? parentContinue(outer,inner) : prefixEffect(outer,inner); }}
        catch(RuntimeException e) {{ actualError=e; }}
        if(expectedError!=actualError || (actualError!=null && actualError!=sentinel)) throw new AssertionError("exception identity");
        if(actualError==null && actual!=expected) throw new AssertionError("value "+kind+":"+outer+":"+inner+":"+actual+" != "+expected);
        if(!trace.equals(expectedTrace)) throw new AssertionError("order "+trace+" != "+expectedTrace);
      }}
  }}
}}
"#,
        source(0),
        source(1),
        source(2),
        source(3),
        source(4),
        source(5)
    );
    fs::write(dir.join("sample/Effects.java"), &java).unwrap();
    for (program, argument) in [("javac", "sample/Effects.java"), ("java", "sample.Effects")] {
        let result = Command::new(program)
            .arg(argument)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{program}: {}\n{java}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
