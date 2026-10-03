//! Primary loop guards may exit backward only through proven pure tails.
use rdx::{
    native_dex::{self, DexClass, DexSymbols},
    native_java,
};
use std::{fs, process::Command, sync::Arc};

#[path = "../src/native_java/backward_pure_exit.rs"]
mod backward_pure_exit;

fn fixture(wide: bool) -> DexClass {
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
        protos: vec![("I".into(), vec!["I".into()])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let method = &mut class.methods[0];
    method.name = if wide { "wide" } else { "primary" }.into();
    method.access_flags = 9;
    method.parameters = if wide {
        vec!["J".into(), "I".into(), "I".into()]
    } else {
        vec!["I".into(), "I".into()]
    };
    method.return_type = if wide { "J" } else { "I" }.into();
    let code = method.code.as_mut().unwrap();
    code.registers = if wide { 7 } else { 4 };
    code.ins = if wide { 4 } else { 2 };
    code.outs = 1;
    code.tries = 0;
    code.try_regions.clear();
    // Bypass enters the shared tail directly. The sole loop exit also enters
    // that tail backward, then joins immediately after the unconditional latch.
    code.instructions = vec![
        0x0012,
        if wide { 0x0638 } else { 0x0338 },
        4,
        if wide { 0x3104 } else { 0x0101 },
        0x0828,
        if wide { 0x5035 } else { 0x2035 },
        0xfffe,
        0x1071,
        0,
        0,
        0x000a,
        0xfa28,
        if wide { 0x0110 } else { 0x010f },
    ];
    class
}

fn source(wide: bool) -> String {
    let class = fixture(wide);
    native_java::render_method("sample.Hello", &class, &class.methods[0])
        .unwrap()
        .source
}

#[test]
fn primary_backward_exit_keeps_bypass_and_wide_value_paths() {
    for wide in [false, true] {
        let source = source(wide);
        assert_eq!(source.matches("while (true)").count(), 1, "{source}");
        assert_eq!(
            source.matches("sample.Effects.touch(").count(),
            1,
            "{source}"
        );
        assert!(source.contains("break;"), "{source}");
        assert!(!source.contains("fallback"), "{source}");
    }
}

fn arrays() -> (Vec<u16>, Vec<usize>, Vec<Option<usize>>) {
    let class = fixture(false);
    let words = class.methods[0].code.as_ref().unwrap().instructions.clone();
    let widths = vec![1, 2, 0, 1, 1, 2, 0, 3, 0, 0, 1, 1, 1];
    let mut targets = vec![None; words.len()];
    targets[1] = Some(5);
    targets[4] = Some(12);
    targets[5] = Some(3);
    targets[11] = Some(5);
    (words, widths, targets)
}

#[test]
fn proof_checks_pure_opcodes_cached_edges_ownership_and_every_suffix_edge() {
    let (words, widths, targets) = arrays();
    let prove = |words: &[u16],
                 widths: &[usize],
                 targets: &[Option<usize>],
                 protected: &[std::ops::Range<usize>],
                 reentry: bool| {
        backward_pure_exit::prove(words, widths, targets, protected, 5, 11, 5, |pc| {
            if pc == 12 {
                Some(if reentry { vec![5] } else { vec![] })
            } else {
                None
            }
        })
    };
    let plan = prove(&words, &widths, &targets, &[], false).unwrap();
    assert_eq!(plan.path, vec![3, 4]);
    assert_eq!((plan.target, plan.join), (3, 12));
    assert!(
        prove(
            &words,
            &widths,
            &targets,
            std::slice::from_ref(&(3..4)),
            false
        )
        .is_none()
    );
    assert!(
        prove(
            &words,
            &widths,
            &targets,
            std::slice::from_ref(&(3..7)),
            false
        )
        .is_some()
    );
    assert!(prove(&words, &widths, &targets, &[], true).is_none());
    // A forward continuation cannot replay the cloned tail on the same exit.
    assert!(
        backward_pure_exit::prove(&words, &widths, &targets, &[], 5, 11, 5, |pc| if pc == 12 {
            Some(vec![3])
        } else {
            None
        })
        .is_none()
    );
    let mut poisoned = targets.clone();
    poisoned[4] = Some(5);
    assert!(prove(&words, &widths, &poisoned, &[], false).is_none());
    let mut poisoned = widths.clone();
    poisoned[3] = 2;
    assert!(prove(&words, &poisoned, &targets, &[], false).is_none());
    for word in [
        0x1071, 0x001a, 0x001c, 0x0021, 0x001f, 0x0027, 0x0038, 0x0300,
    ] {
        let mut poisoned = words.clone();
        poisoned[3] = word;
        assert!(
            prove(&poisoned, &widths, &targets, &[], false).is_none(),
            "{word:#x}"
        );
    }
    // A secondary successor of a downstream conditional/switch must not hide
    // reentry, even if another successor terminates.
    assert!(
        backward_pure_exit::prove(&words, &widths, &targets, &[], 5, 11, 5, |pc| match pc {
            12 => Some(vec![0, 5]),
            0 => Some(vec![]),
            _ => None,
        })
        .is_none()
    );
}

#[test]
fn emission_suffix_projection_rejects_exceptional_reentry() {
    let (mut words, mut widths, mut targets) = arrays();
    words.truncate(12);
    words.extend([0x1071, 0, 1, 0x010a, 0x010f, 0x010d, 0xf328]);
    widths.truncate(12);
    widths.extend([3, 0, 0, 1, 1, 1, 1]);
    targets.resize(words.len(), None);
    targets[18] = Some(5);
    let prove = |exceptional: bool| {
        backward_pure_exit::prove(&words, &widths, &targets, &[], 5, 11, 5, |pc| match pc {
            12 => Some(if exceptional { vec![15, 17] } else { vec![15] }),
            15 => Some(vec![16]),
            16 => Some(vec![]),
            17 => Some(vec![18]),
            18 => Some(vec![5]),
            _ => None,
        })
    };
    assert!(prove(false).is_some());
    assert!(prove(true).is_none());
}

#[test]
fn pure_tail_instruction_budget_accepts_32_and_rejects_33() {
    for count in [32usize, 33] {
        let header = count;
        let latch = header + 2;
        let join = latch + 1;
        let mut words = vec![0; header];
        words[header - 1] = 0x0428;
        words.extend([0x0038, (-(header as i16)) as u16, 0xfe28, 0x000e]);
        let mut widths = vec![1; words.len()];
        widths[header] = 2;
        widths[header + 1] = 0;
        let mut targets = vec![None; words.len()];
        targets[header - 1] = Some(join);
        targets[header] = Some(0);
        targets[latch] = Some(header);
        let proof = backward_pure_exit::prove(
            &words,
            &widths,
            &targets,
            &[],
            header,
            latch,
            header,
            |pc| (pc == join).then(Vec::new),
        );
        assert_eq!(proof.is_some(), count == 32, "instruction count {count}");
    }
}

#[test]
fn effectful_or_differently_protected_backward_tail_retains_fallback() {
    let mut class = fixture(false);
    class.methods[0].code.as_mut().unwrap().instructions = vec![
        0x0012, 0x0338, 7, 0x1071, 0, 0, 0x010a, 0x0828, 0x2035, 0xfffb, 0x1071, 0, 0, 0x000a,
        0xfa28, 0x010f,
    ];
    assert!(native_java::render_method("sample.Hello", &class, &class.methods[0]).is_err());
    let mut class = fixture(false);
    let code = class.methods[0].code.as_mut().unwrap();
    code.instructions.extend([0x010d, 0x0127]);
    code.tries = 1;
    code.try_regions = vec![native_dex::DexTryRegion {
        start: 3,
        end: 4,
        catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 13)].into(),
    }];
    assert!(native_java::render_method("sample.Hello", &class, &class.methods[0]).is_err());
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn unchanged_source_jvm_preserves_iteration_trace_bypass_wide_values_and_exception_identity() {
    let dir =
        std::env::temp_dir().join(format!("rdx-primary-backward-exit-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let java = format!(
        r#"package sample;
public class Effects {{
  static java.util.List<Integer> trace = new java.util.ArrayList<>();
  static int failAt;
  static final RuntimeException sentinel = new RuntimeException("sentinel");
  public static int touch(int n) {{ trace.add(n); if(n==failAt) throw sentinel; return n+1; }}
  {}
  {}
  public static void main(String[] args) {{
    for(int limit=-2; limit<12; limit++) for(int bypass=0; bypass<2; bypass++)
      for(int mode=0; mode<3; mode++) for(int kind=0; kind<2; kind++) {{
        trace.clear(); failAt=mode==0 ? -1 : mode==1 ? 0 : Math.max(0,limit/2);
        boolean enter=bypass==0 && limit>0;
        long seed=Long.MIN_VALUE+limit;
        try {{
          long actual=kind==0 ? primary(limit,bypass) : wide(seed,limit,bypass);
          if(mode!=0 && enter) throw new AssertionError("missing exception");
          long expected=kind==0 ? (bypass==0 ? Math.max(0,limit) : 0) : seed;
          if(actual!=expected) throw new AssertionError("value "+actual+" != "+expected);
        }} catch(RuntimeException e) {{
          if(mode==0 || !enter || e!=sentinel) throw new AssertionError("exception identity",e);
        }}
        int calls=enter ? (mode!=0 ? failAt+1 : limit) : 0;
        if(trace.size()!=calls) throw new AssertionError("effect count "+trace);
        for(int i=0;i<calls;i++) if(trace.get(i)!=i) throw new AssertionError("order "+trace);
      }}
  }}
}}
"#,
        source(false),
        source(true)
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
