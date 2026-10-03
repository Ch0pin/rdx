//! External effectful suffixes are copied before a common ordinary loop.
use rdx::{
    native_cfg,
    native_dex::{self, DexClass, DexSymbols},
    native_dominators, native_ir, native_java,
};
use std::{fs, process::Command, sync::Arc};
#[path = "../src/native_java/forward_iteration_entry.rs"]
mod forward_iteration_entry;
#[path = "../src/native_java/loop_entry_copy.rs"]
mod loop_entry_copy;
#[path = "../src/native_java/single_guard_entry.rs"]
mod single_guard_entry;

fn fixture(kind: usize) -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class
        .methods
        .retain(|method| method.name.as_ref() == "answer");
    class.symbols = Arc::new(DexSymbols {
        strings: vec![
            if matches!(kind, 1 | 5) {
                "touchWide"
            } else {
                "touch"
            }
            .into(),
        ],
        types: vec!["Lsample/Effects;".into()],
        protos: vec![(
            if matches!(kind, 1 | 5) { "J" } else { "I" }.into(),
            vec!["I".into()],
        )],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let method = &mut class.methods[0];
    method.name = [
        "linear",
        "wide",
        "nested",
        "prefixTerminal",
        "forwardDiamond",
        "wideDiamond",
        "backwardStemDiamond",
    ][kind]
        .into();
    method.access_flags = 9;
    method.parameters = vec!["I".into(), "I".into(), "I".into()];
    method.return_type = if matches!(kind, 1 | 5) { "J" } else { "I" }.into();
    let code = method.code.as_mut().unwrap();
    code.registers = if matches!(kind, 1 | 5) { 6 } else { 5 };
    code.ins = 3;
    code.outs = 1;
    code.tries = 0;
    code.try_regions.clear();
    code.instructions = match kind {
        0 => vec![
            0x0012, 0x0112, 0x0338, 4, 0x4001, 0x0528, 0x2035, 9, 0x00d8, 0x0100, 0x1071, 0, 0,
            0x010a, 0xf828, 0x010f,
        ],
        1 => vec![
            0x0012, 0x0116, 0, 0x0438, 4, 0x5001, 0x0528, 0x3035, 9, 0x00d8, 0x0100, 0x1071, 0, 0,
            0x010b, 0xf828, 0x0110,
        ],
        2 => vec![
            0x0012, 0x0112, 0x0338, 8, 0x4001, 0x003b, 4, 0x0029, 9, 0x0828, 0x2035, 12, 0x00d8,
            0x0100, 0x003b, 3, 0x010f, 0x1071, 0, 0, 0x010a, 0xf528, 0x010f,
        ],
        3 => vec![
            0x0012, 0x0112, 0x0338, 6, 0x4001, 0x003a, 12, 0x0528, 0x2035, 9, 0x00d8, 0x0100,
            0x1071, 0, 0, 0x010a, 0xf828, 0x7112, 0x010f,
        ],
        4 => vec![
            0x0012, 0x0112, 0x0338, 4, 0x4001, 0x0528, 0x2035, 12, 0x00d8, 0x0100, 0x0038, 6,
            0x1071, 0, 0, 0x010a, 0, 0xf528, 0x010f,
        ],
        5 => vec![
            0x0012, 0x0116, 0, 0x0438, 4, 0x5001, 0x0528, 0x3035, 12, 0x00d8, 0x0100, 0x0038, 6,
            0x1071, 0, 0, 0x010b, 0, 0xf528, 0x0110,
        ],
        6 => vec![
            0x0012, 0x0112, 0x0338, 5, 0x4001, 0x0029, 17, 0x2035, 20, 0x00d8, 0x0100, 0x0029, 11,
            0x0038, 6, 0x1071, 0, 0, 0x010a, 0, 0x0029, 5, 0x0401, 0x0029, 0xfff6, 0x0029, 0xffee,
            0x010f,
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

fn effectful_prefix_terminal_fixture() -> DexClass {
    let mut class = fixture(3);
    class.methods[0].name = "effectfulPrefixTerminal".into();
    class.methods[0]
        .code
        .as_mut()
        .unwrap()
        .instructions
        .splice(17..19, [0x1071, 0, 0, 0x010a, 0x010f]);
    class
}

#[test]
fn proof_classifies_every_original_edge_and_keeps_terminal_entry_copies_separate() {
    for kind in 0..4 {
        let class = fixture(kind);
        let code = class.methods[0].code.as_ref().unwrap();
        let plan = loop_entry_copy::select(code).unwrap();
        assert!(loop_entry_copy::validate(code, &plan));
        assert_eq!(plan.copies.len(), if kind >= 2 { 2 } else { 1 });
        assert_eq!(
            plan.copies
                .iter()
                .filter(|copy| copy.header.is_some())
                .count(),
            1
        );
        assert_eq!(
            plan.copies
                .iter()
                .filter(|copy| copy.header.is_none())
                .count(),
            usize::from(kind >= 2)
        );
        assert!(plan.copies.iter().all(|copy| copy.instructions.len() <= 32));
        assert!(plan.targets.contains(&(plan.latch, plan.header)));
        let mut poisoned = plan.clone();
        poisoned.copies[0].instructions.pop();
        assert!(!loop_entry_copy::validate(code, &poisoned));
        let mut poisoned = plan.clone();
        poisoned.edges.pop();
        assert!(!loop_entry_copy::validate(code, &poisoned));
        let mut poisoned = plan.clone();
        poisoned.latch = plan.copies[0].from;
        assert!(!loop_entry_copy::validate(code, &poisoned));
        let mut poisoned = plan.clone();
        poisoned.targets[0].1 = plan.header + 1;
        assert!(!loop_entry_copy::validate(code, &poisoned));
    }
}

#[test]
fn copied_suffix_runs_in_each_entry_and_original_loop_keeps_its_own_effect() {
    for kind in 0..7 {
        let java = source(kind);
        assert_eq!(java.matches("while (true)").count(), 1, "{java}");
        assert_eq!(java.matches("sample.Effects.touch").count(), 2, "{java}");
        assert!(!java.contains("switch ("), "{java}");
    }
}

#[test]
fn handlers_allocations_mid_instruction_entries_and_branching_suffixes_decline() {
    for (pc, word) in [(5, 0x0628), (10, 0x001d), (10, 0x0022), (10, 0x0038)] {
        let mut class = fixture(0);
        let code = class.methods[0].code.as_mut().unwrap();
        code.instructions[pc] = word;
        assert!(
            loop_entry_copy::select(code).is_none(),
            "mutation {pc}={word:#x}"
        );
    }
    let mut class = fixture(0);
    let code = class.methods[0].code.as_mut().unwrap();
    code.tries = 1;
    assert!(loop_entry_copy::select(code).is_none());
    let mut class = fixture(0);
    let code = class.methods[0].code.as_mut().unwrap();
    code.try_regions = vec![native_dex::DexTryRegion {
        start: 10,
        end: 13,
        catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 6)].into(),
    }];
    assert!(loop_entry_copy::select(code).is_none());
    // A wide header value is read with no initialized wide head on one entry.
    let mut class = fixture(1);
    class.methods[0].code.as_mut().unwrap().instructions[1] = 0x0113;
    assert!(native_java::render_method("sample.Hello", &class, &class.methods[0]).is_err());
    // Pending new-instance state cannot cross either split entry edge.
    let mut class = fixture(0);
    let code = class.methods[0].code.as_mut().unwrap();
    code.instructions[0] = 0x0122;
    code.instructions[1] = 0;
    native_cfg::ControlFlowGraph::build(code).unwrap();
    assert!(native_java::render_method("sample.Hello", &class, &class.methods[0]).is_err());
    // The original pure-leaf plan still cannot justify this effectful leaf.
    let original = fixture(3);
    let pure_plan = loop_entry_copy::select(original.methods[0].code.as_ref().unwrap()).unwrap();
    assert!(pure_plan.iteration.is_none());
    let class = effectful_prefix_terminal_fixture();
    let code = class.methods[0].code.as_ref().unwrap();
    assert!(!loop_entry_copy::validate(code, &pure_plan));
    // Admission requires the separate complete iteration/terminal-copy proof.
    let plan = loop_entry_copy::select(code).unwrap();
    let iteration = plan.iteration.as_ref().unwrap();
    assert!(forward_iteration_entry::validate(code, iteration));
    assert!(loop_entry_copy::validate(code, &plan));
    let terminal = plan
        .copies
        .iter()
        .find(|copy| copy.header.is_none())
        .unwrap();
    assert_eq!(terminal.instructions, [17, 20, 21]);
}

#[test]
fn linear_suffix_instruction_budget_accepts_32_and_rejects_33() {
    for count in [29usize, 30] {
        let mut class = fixture(0);
        let code = class.methods[0].code.as_mut().unwrap();
        code.instructions
            .splice(10..10, std::iter::repeat_n(0, count));
        code.instructions[7] = (9 + count) as u16;
        code.instructions[14 + count] = (((-8 - count as i32) as i8 as u8 as u16) << 8) | 0x28;
        assert_eq!(loop_entry_copy::select(code).is_some(), count == 29);
    }
}

#[test]
fn forward_diamonds_and_initial_acyclic_stem_own_only_reachable_nodes() {
    for kind in 4..7 {
        let class = fixture(kind);
        let code = class.methods[0].code.as_ref().unwrap();
        let plan = loop_entry_copy::select(code).unwrap();
        let copy = plan
            .copies
            .iter()
            .find(|copy| copy.header.is_some())
            .unwrap();
        assert_eq!(copy.instructions.last(), Some(&plan.latch));
        assert!(copy.instructions.len() <= 32);
        assert!(loop_entry_copy::validate(code, &plan));
        let mut poisoned = plan.clone();
        poisoned
            .copies
            .iter_mut()
            .find(|copy| copy.header.is_some())
            .unwrap()
            .instructions
            .remove(1);
        assert!(!loop_entry_copy::validate(code, &poisoned));
        let mut poisoned = plan.clone();
        poisoned.targets[0].1 = plan.header + 1;
        assert!(!loop_entry_copy::validate(code, &poisoned));
        let cfg = native_cfg::ControlFlowGraph::build(code).unwrap();
        let owned: std::collections::BTreeSet<_> = copy.instructions.iter().copied().collect();
        for block in &cfg.blocks {
            for &pc in &block.instructions {
                if !owned.contains(&pc) {
                    continue;
                }
                if pc == *block.instructions.last().unwrap() {
                    for edge in &block.successors {
                        let to = cfg.blocks[edge.target].start;
                        assert!(owned.contains(&to) || (pc == plan.latch && to == plan.header));
                    }
                }
            }
        }
        if kind == 6 {
            assert_eq!(copy.target, 22);
            assert!(owned.contains(&13) && owned.contains(&23));
            assert!(!owned.contains(&11)); // Normal-body jump is not duplicated.
        }
    }
}

#[test]
fn backward_conditional_cycles_terminal_arms_and_pending_result_entries_decline() {
    for (pc, word) in [(23, 0x0038), (21, 2), (6, 13), (14, 0xfffa)] {
        let mut class = fixture(6);
        class.methods[0].code.as_mut().unwrap().instructions[pc] = word;
        assert!(
            loop_entry_copy::select(class.methods[0].code.as_ref().unwrap()).is_none(),
            "stem mutation {pc}={word:#x}"
        );
    }
    let mut class = fixture(4);
    class.methods[0].code.as_mut().unwrap().instructions[11] = 8;
    assert!(loop_entry_copy::select(class.methods[0].code.as_ref().unwrap()).is_none());
    let mut class = fixture(4);
    // Conditional successor enters the result with no adjacent call producer.
    class.methods[0].code.as_mut().unwrap().instructions[11] = 5;
    assert!(loop_entry_copy::select(class.methods[0].code.as_ref().unwrap()).is_none());
}

#[test]
fn copied_dag_total_instruction_budget_accepts_32_and_rejects_33() {
    for count in [27usize, 28] {
        let mut class = fixture(4);
        let code = class.methods[0].code.as_mut().unwrap();
        code.instructions
            .splice(16..16, std::iter::repeat_n(0, count));
        code.instructions[7] = (12 + count) as u16;
        code.instructions[11] = (6 + count) as u16;
        code.instructions[17 + count] = (((-11 - count as i32) as i8 as u8 as u16) << 8) | 0x28;
        assert_eq!(loop_entry_copy::select(code).is_some(), count == 27);
    }
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn unchanged_source_jvm_checks_fresh_resume_terminal_nested_wide_and_exception_order() {
    let dir = std::env::temp_dir().join(format!("rdx-loop-entry-copy-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let java = format!(
        r#"package sample;
public class Effects {{
 static java.util.List<Integer> trace=new java.util.ArrayList<>();
 static int failAt; static final RuntimeException sentinel=new RuntimeException("sentinel");
 public static int touch(int i) {{ trace.add(i);if(trace.size()==failAt)throw sentinel;return i*10; }}
 public static long touchWide(int i) {{ trace.add(i);if(trace.size()==failAt)throw sentinel;return ((long)i<<33)+31; }}
 {} {} {} {} {} {} {}
 static long oracle(int kind,int limit,int resume,int seed) {{
   int i=0;long value=0;
   if(resume!=0) {{i=seed;if(kind==2&&i<0)return 0;if(kind==3&&i<0)return 7;if(kind<4||i!=0)value=(kind==1||kind==5)?touchWide(i):touch(i);}}
   while(i<limit) {{ i++;if(kind==2&&i<0)return value;if(kind<4||i!=0)value=(kind==1||kind==5)?touchWide(i):touch(i); }}
   return kind==3?7:value;
 }}
 public static void main(String[] args) {{
   for(int limit=-2;limit<=8;limit++)for(int resume=0;resume<2;resume++)for(int seed=-2;seed<=8;seed++)
     for(int kind=0;kind<7;kind++)for(int failure:new int[]{{0,1,3}}) {{
       trace.clear();failAt=failure;long expected=0;RuntimeException expectedError=null;
       try{{expected=oracle(kind,limit,resume,seed);}}catch(RuntimeException e){{expectedError=e;}}
       java.util.List<Integer> expectedTrace=new java.util.ArrayList<>(trace);trace.clear();
       long actual=0;RuntimeException actualError=null;
       try{{actual=kind==0?linear(limit,resume,seed):kind==1?wide(limit,resume,seed):kind==2?nested(limit,resume,seed):kind==3?prefixTerminal(limit,resume,seed):kind==4?forwardDiamond(limit,resume,seed):kind==5?wideDiamond(limit,resume,seed):backwardStemDiamond(limit,resume,seed);}}catch(RuntimeException e){{actualError=e;}}
       if(expectedError!=actualError||(actualError!=null&&actualError!=sentinel))throw new AssertionError("exception identity");
       if(actualError==null&&actual!=expected)throw new AssertionError("value "+kind+":"+limit+":"+resume+":"+seed+":"+actual+" != "+expected);
       if(!trace.equals(expectedTrace))throw new AssertionError("order "+trace+" != "+expectedTrace);
     }}
 }}
}}
"#,
        source(0),
        source(1),
        source(2),
        source(3),
        source(4),
        source(5),
        source(6)
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

#[test]
#[ignore = "requires javac/java; checks the admitted effectful terminal leaf"]
fn effectful_terminal_copy_matches_encoded_dex_effects_and_fault_identity() {
    let class = effectful_prefix_terminal_fixture();
    let method = &class.methods[0];
    let source = native_java::render_method("sample.Hello", &class, method)
        .unwrap()
        .source;
    let words = method
        .code
        .as_ref()
        .unwrap()
        .instructions
        .iter()
        .map(|word| word.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let dir = std::env::temp_dir().join(format!(
        "rdx-effectful-terminal-copy-{}",
        std::process::id()
    ));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let java = r#"package sample;
public class Effects {
 static final java.util.List<Integer> trace = new java.util.ArrayList<>();
 static final RuntimeException FAULT = new RuntimeException("terminal fault");
 static int failAt;
 public static int touch(int value) { trace.add(value); if(trace.size()==failAt)throw FAULT;return value*10; }
 SOURCE
 static final int[] WORDS={WORDS_LIST};
 // Original register numbers, widths, branch offsets and invocation operands.
 static int oracle(int limit,int resume,int seed) {
  int[] r={0,0,limit,resume,seed};int pc=0,pending=0,budget=1000;
  while(--budget>0) {
   int w=WORDS[pc],op=w&255,a=w>>>8;
   switch(op) {
    case 0x12: {int value=a>>>4;if(value>=8)value-=16;r[a&15]=value;pc++;break;}
    case 0x01:r[a&15]=r[a>>>4];pc++;break;
    case 0x38:pc=r[a]==0?pc+(short)WORDS[pc+1]:pc+2;break;
    case 0x3a:pc=r[a]<0?pc+(short)WORDS[pc+1]:pc+2;break;
    case 0x35:pc=r[a&15]>=r[a>>>4]?pc+(short)WORDS[pc+1]:pc+2;break;
    case 0xd8:{int args=WORDS[pc+1];r[a]=r[args&255]+(byte)(args>>>8);pc+=2;break;}
    case 0x71:if((a>>>4)!=1||WORDS[pc+1]!=0)throw new AssertionError("invoke");pending=touch(r[WORDS[pc+2]&15]);pc+=3;break;
    case 0x0a:r[a]=pending;pc++;break;
    case 0x28:pc+=(byte)a;break;
    case 0x0f:return r[a];
    default:throw new AssertionError("opcode "+op);
   }
  }throw new AssertionError("execution budget");
 }
 public static void main(String[] args) {
  int cases=0;
  for(int limit=-2;limit<=8;limit++)for(int resume=0;resume<2;resume++)
   for(int seed:new int[]{Integer.MIN_VALUE,-2,-1,0,1,2,3,4,5,6,7,8,Integer.MAX_VALUE})
    for(int failure:new int[]{0,1,2,3}) {
     trace.clear();failAt=failure;int expected=0;RuntimeException expectedFault=null;
     try{expected=oracle(limit,resume,seed);}catch(RuntimeException e){expectedFault=e;}
     java.util.List<Integer> expectedTrace=new java.util.ArrayList<>(trace);trace.clear();
     int actual=0;RuntimeException actualFault=null;
     try{actual=effectfulPrefixTerminal(limit,resume,seed);}catch(RuntimeException e){actualFault=e;}
     if(actualFault!=expectedFault||(actualFault!=null&&actualFault!=FAULT))throw new AssertionError("fault identity");
     if(actualFault==null&&actual!=expected)throw new AssertionError("terminal value");
     if(!trace.equals(expectedTrace))throw new AssertionError("effect order "+trace+" != "+expectedTrace);
     cases++;
    }
  System.out.println("effectful terminal copy: "+cases+" encoded DEX comparisons");
 }
}
"#.replace("SOURCE", &source).replace("WORDS_LIST", &words);
    fs::write(dir.join("sample/Effects.java"), &java).unwrap();
    let java_home = std::env::var_os("RDX_JAVA25_HOME").map(std::path::PathBuf::from);
    for (program, argument) in [("javac", "sample/Effects.java"), ("java", "sample.Effects")] {
        let executable = java_home
            .as_ref()
            .map(|home| home.join("bin").join(program))
            .unwrap_or_else(|| program.into());
        let mut command = Command::new(executable);
        if program == "java" {
            command.arg("-Xverify:all");
        }
        let result = command.arg(argument).current_dir(&dir).output().unwrap();
        assert!(
            result.status.success(),
            "{program}: {}\n{java}",
            String::from_utf8_lossy(&result.stderr)
        );
        eprintln!("{}", String::from_utf8_lossy(&result.stdout));
    }
}
