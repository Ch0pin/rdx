use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;
fn fixture(
    checked: bool,
    shared_void: bool,
    nonterminal_catch: bool,
    wide: bool,
) -> (DexClass, DexMethod) {
    let symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Api;".into()],
        strings: vec![
            "protectedCall".into(),
            "tail".into(),
            "handler".into(),
            "join".into(),
        ],
        protos: vec![
            (if wide { "J" } else { "I" }.into(), vec![]),
            (
                if shared_void {
                    "V"
                } else if wide {
                    "J"
                } else {
                    "I"
                }
                .into(),
                vec![if wide { "J" } else { "I" }.into()],
            ),
            ("V".into(), vec![]),
        ],
        methods: vec![(0, 0, 0), (0, 1, 1), (0, 2, 2), (0, 2, 3)],
        ..Default::default()
    });
    let caught = if checked {
        "Ljava/io/IOException;"
    } else {
        "Ljava/lang/RuntimeException;"
    };
    let words = if wide {
        vec![
            0x0071, 0, 0, 0x000b, 0x2071, 1, 0x0010, 0x000b, 0x0010, 0x020d, 0x0016, 0xffff, 0x0010,
        ]
    } else if shared_void {
        if nonterminal_catch {
            vec![
                0x0071, 0, 0, 0x000a, 0x1071, 1, 0, 0x0528, 0x010d, 0x0071, 2, 0, 0x0071, 3, 0,
                0x000e,
            ]
        } else {
            vec![
                0x0071, 0, 0, 0x000a, 0x1071, 1, 0, 0x0528, 0x010d, 0x0071, 2, 0, 0x000e,
            ]
        }
    } else {
        vec![
            0x0071, 0, 0, 0x000a, 0x1071, 1, 0, 0x000a, 0x000f, 0x010d, 0xf012, 0x000f,
        ]
    };
    let method = DexMethod {
        declaring_type: "Lsample/Example;".into(),
        name: "run".into(),
        return_type: if shared_void {
            "V"
        } else if wide {
            "J"
        } else {
            "I"
        }
        .into(),
        parameters: vec![],
        thrown_types: if checked { vec![caught.into()] } else { vec![] },
        access_flags: 9,
        code: Some(DexCode {
            registers: if wide { 3 } else { 2 },
            ins: 0,
            outs: if wide { 2 } else { 1 },
            tries: 1,
            try_regions: vec![DexTryRegion {
                start: 0,
                end: 4,
                catches: vec![(Some(caught.into()), if shared_void { 8 } else { 9 })].into(),
            }],
            instructions: words,
            offset: 0,
        }),
    };
    let c = DexClass {
        symbols: symbols.clone(),
        descriptor: "Lsample/Example;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 0x11,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![],
    };
    let mut api = DexClass {
        symbols: symbols.clone(),
        descriptor: "Lsample/Api;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 0x11,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![],
    };
    for (name, ret, params) in [
        ("protectedCall", if wide { "J" } else { "I" }, vec![]),
        (
            "tail",
            if shared_void {
                "V"
            } else if wide {
                "J"
            } else {
                "I"
            },
            vec![if wide { "J" } else { "I" }.into()],
        ),
        ("handler", "V", vec![]),
        ("join", "V", vec![]),
    ] {
        api.methods.push(DexMethod {
            declaring_type: api.descriptor.clone(),
            name: name.into(),
            return_type: ret.into(),
            parameters: params,
            thrown_types: if checked && matches!(name, "protectedCall" | "tail") {
                vec![caught.into()]
            } else {
                vec![]
            },
            access_flags: 9,
            code: None,
        });
    }
    symbols
        .hierarchy
        .set(Arc::new(TypeHierarchy::from_classes([&c, &api]).unwrap()))
        .unwrap();
    (c, method)
}
fn render(checked: bool, shared_void: bool, nonterminal: bool) -> Result<String, anyhow::Error> {
    let (c, m) = fixture(checked, shared_void, nonterminal, false);
    native_java::render_method("sample.Example", &c, &m).map(|s| s.source)
}
#[test]
fn effectful_tail_follows_terminal_catch_and_hoists_normal_values() {
    let s = render(false, false, false).unwrap();
    let tail = s.find("sample.Api.tail").unwrap();
    let catch = s.find("catch").unwrap();
    assert!(tail > catch, "{s}");
    assert!(s[..catch].contains("sample.Api.protectedCall"), "{s}");
}
#[test]
fn shared_bare_void_return_keeps_handler_terminal() {
    let s = render(false, true, false).unwrap();
    assert!(
        s.find("sample.Api.tail").unwrap() > s.find("catch").unwrap(),
        "{s}"
    );
}
#[test]
fn nonterminal_handler_keeps_normal_effect_outside_catch_and_shared_join() {
    let source = render(false, true, true).unwrap();
    assert!(
        source.contains("= false;") && source.contains("= true;"),
        "{source}"
    );
    assert!(
        source.find("catch").unwrap() < source.find("sample.Api.tail").unwrap(),
        "{source}"
    );
    assert!(
        source.find("sample.Api.tail").unwrap() < source.find("sample.Api.join").unwrap(),
        "{source}"
    );
    assert_eq!(source.matches("sample.Api.join").count(), 1, "{source}");
}
#[test]
fn checked_tail_retains_its_declared_exception_outside_catch() {
    let s = render(true, false, false).unwrap();
    assert!(s.contains("throws java.io.IOException"), "{s}");
    assert!(
        s.find("sample.Api.tail").unwrap() > s.find("catch").unwrap(),
        "{s}"
    );
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn jvm_preserves_tail_exception_identity_order_and_handler_ownership() {
    for checked in [false, true] {
        for shared_void in [false, true] {
            let source = render(checked, shared_void, false).unwrap();
            let dir = std::env::temp_dir().join(format!(
                "rdx-normal-tail-{}-{checked}-{shared_void}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            struct Cleanup(std::path::PathBuf);
            impl Drop for Cleanup {
                fn drop(&mut self) {
                    let _ = std::fs::remove_dir_all(&self.0);
                }
            }
            let _cleanup = Cleanup(dir.clone());
            let ty = if checked {
                "java.io.IOException"
            } else {
                "RuntimeException"
            };
            let throws = if checked {
                "throws java.io.IOException"
            } else {
                ""
            };
            let ret = if shared_void { "void" } else { "int" };
            let returned = if shared_void {
                "return;"
            } else {
                "return value+1;"
            };
            let invoke = if shared_void {
                "Example.run();if(mode==2)throw new AssertionError(\"tail caught\");"
            } else {
                "int result=Example.run();if(mode==2||result!=(mode==1?-1:4))throw new AssertionError(\"result\");"
            };
            let caught_trace = if shared_void { "PH" } else { "P" };
            let java = format!(
                r#"package sample; class Api {{static int mode;static String trace;static {ty} original;static int protectedCall() {throws} {{trace+="P";if(mode==1)throw original;return 3;}}static {ret} tail(int value) {throws} {{trace+="T";if(value!=3)throw new AssertionError("value");if(mode==2)throw original;{returned}}}static void handler(){{trace+="H";}}}} class Example {{{source}}} public class Runner{{public static void main(String[]args) throws Exception{{for(int mode=0;mode<3;mode++){{Api.mode=mode;Api.trace="";Api.original=new {ty}();try{{{invoke}}}catch({ty} caught){{if(mode!=2||caught!=Api.original)throw new AssertionError("identity");}}if(!Api.trace.equals(mode==1?"{caught_trace}":"PT"))throw new AssertionError(Api.trace);}}}}}}"#
            );
            std::fs::write(dir.join("Runner.java"), java).unwrap();
            let o = std::process::Command::new("javac")
                .args([
                    "-d",
                    dir.to_str().unwrap(),
                    dir.join("Runner.java").to_str().unwrap(),
                ])
                .output()
                .unwrap();
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
            let o = std::process::Command::new("java")
                .args(["-cp", dir.to_str().unwrap(), "sample.Runner"])
                .output()
                .unwrap();
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        }
    }
}
#[test]
fn wide_prefix_values_and_navigation_links_survive_the_split() {
    let (c, m) = fixture(false, false, false, true);
    let rendered = native_java::render_method("sample.Example", &c, &m).unwrap();
    assert!(rendered.source.contains("long v"), "{}", rendered.source);
    for label in ["sample.Api.protectedCall()J", "sample.Api.tail(J)J"] {
        let link = rendered.links.iter().find(|l| l.label == label).unwrap();
        let spelling = rendered
            .source
            .chars()
            .skip(link.start)
            .take(link.end - link.start)
            .collect::<String>();
        assert_eq!(
            spelling,
            if label.contains("protectedCall") {
                "protectedCall"
            } else {
                "tail"
            }
        );
    }
}
#[test]
fn handler_reentering_normal_tail_remains_rejected() {
    let (c, mut m) = fixture(false, true, false, false);
    m.code.as_mut().unwrap().instructions = vec![
        0x0071, 0, 0, 0x000a, 0x1071, 1, 0, 0x0328, 0x010d, 0xfb28, 0x000e,
    ];
    assert!(native_java::render_method("sample.Example", &c, &m).is_err());
}
#[test]
fn partially_overlapping_try_ownership_remains_rejected() {
    let (c, mut m) = fixture(false, true, false, false);
    let code = m.code.as_mut().unwrap();
    code.tries = 2;
    code.try_regions.push(DexTryRegion {
        start: 3,
        end: 7,
        catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 8)].into(),
    });
    assert!(native_java::render_method("sample.Example", &c, &m).is_err());
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn jvm_preserves_wide_live_out_word_pair() {
    let (c, m) = fixture(false, false, false, true);
    let source = native_java::render_method("sample.Example", &c, &m)
        .unwrap()
        .source;
    let dir = std::env::temp_dir().join(format!("rdx-normal-tail-wide-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());
    let java = format!(
        r#"package sample; class Api {{static int mode;static RuntimeException original;static String trace;static long protectedCall(){{trace+="P";if(mode==1)throw original;return 0x100000002L;}}static long tail(long value){{trace+="T";if(value!=0x100000002L)throw new AssertionError("pair");if(mode==2)throw original;return value+1;}}}} class Example {{{source}}} public class Runner {{public static void main(String[]args){{for(int mode=0;mode<3;mode++){{Api.mode=mode;Api.original=new RuntimeException();Api.trace="";try{{long value=Example.run();if(mode==2||value!=(mode==1?-1L:0x100000003L))throw new AssertionError("result");}}catch(RuntimeException caught){{if(mode!=2||caught!=Api.original)throw new AssertionError("identity");}}if(!Api.trace.equals(mode==1?"P":"PT"))throw new AssertionError(Api.trace);}}}}}}"#
    );
    std::fs::write(dir.join("Runner.java"), java).unwrap();
    let o = std::process::Command::new("javac")
        .args([
            "-d",
            dir.to_str().unwrap(),
            dir.join("Runner.java").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = std::process::Command::new("java")
        .args(["-cp", dir.to_str().unwrap(), "sample.Runner"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}
#[test]
fn nested_try_with_competing_handler_ownership_remains_rejected() {
    let (c, mut m) = fixture(false, true, false, false);
    let code = m.code.as_mut().unwrap();
    code.tries = 2;
    code.try_regions.push(DexTryRegion {
        start: 0,
        end: 3,
        catches: vec![(Some("Ljava/lang/Throwable;".into()), 8)].into(),
    });
    assert!(native_java::render_method("sample.Example", &c, &m).is_err());
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn jvm_nonterminal_catch_preserves_normal_only_tail_and_shared_join() {
    for checked in [false, true] {
        let source = render(checked, true, true).unwrap();
        let ty = if checked {
            "java.io.IOException"
        } else {
            "RuntimeException"
        };
        let throws = if checked {
            "throws java.io.IOException"
        } else {
            ""
        };
        let java = format!(
            r#"package sample;class Api{{static int mode;static String trace;static {ty} original=new {ty}(),second=new {ty}();static int protectedCall(){throws}{{trace+="P";if(mode==1||mode==3||mode==5)throw original;return 3;}}static void tail(int value){throws}{{trace+="T";if(value!=3)throw new AssertionError("default prefix observed");if(mode==2)throw original;}}static void handler(){throws}{{trace+="H";if(mode==3)throw second;}}static void join(){throws}{{trace+="J";if(mode==4||mode==5)throw original;}}}}class Example{{{source}}}public class Runner{{public static void main(String[]args)throws Exception{{String[]trace={{"PTJ","PHJ","PT","PH","PTJ","PHJ"}};for(int mode=0;mode<6;mode++){{Api.mode=mode;Api.trace="";try{{Example.run();if(mode>=2)throw new AssertionError("failure missing");}}catch({ty} caught){{if(caught!=(mode==3?Api.second:Api.original)||mode<2)throw new AssertionError("identity");}}if(!Api.trace.equals(trace[mode]))throw new AssertionError(Api.trace);}}}}}}"#
        );
        let dir = std::env::temp_dir().join(format!(
            "rdx-nonterminal-normal-tail-{}-{checked}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(dir.clone());
        let path = dir.join("Runner.java");
        std::fs::write(&path, java).unwrap();
        let o = std::process::Command::new("javac")
            .arg("-d")
            .arg(&dir)
            .arg(path)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "{}\n{source}",
            String::from_utf8_lossy(&o.stderr)
        );
        let o = std::process::Command::new("java")
            .arg("-cp")
            .arg(&dir)
            .arg("sample.Runner")
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }
}

// A pure branch exits the first try toward a return located after a separately
// protected effect. Splitting the first try must retain both handler sets.
fn crossing_return_frontier_fixture() -> (DexClass, DexMethod) {
    let (class, mut method) = fixture(false, false, false, false);
    method.parameters = vec!["Z".into(), "Z".into()];
    method.code = Some(DexCode {
        registers: 5,
        ins: 2,
        outs: 1,
        tries: 2,
        try_regions: vec![
            DexTryRegion {
                start: 2,
                end: 13,
                catches: vec![(Some("Ljava/lang/IllegalArgumentException;".into()), 22)].into(),
            },
            DexTryRegion {
                start: 16,
                end: 20,
                catches: vec![(Some("Ljava/lang/IllegalStateException;".into()), 22)].into(),
            },
        ],
        instructions: vec![
            0x0012, // 0: v0 = 0, both discarded-exception handler returns
            0x1112, // 1: v1 = 1, early success
            0x0071, 0, 0,      // 2: protectedCall()
            0x020a, // 5: move-result v2
            0x0338, 15, // 6: if !first, goto early return at 21
            0x0071, 2, 0, // 8: handler(), still in first try
            0x0439, 5, // 11: if second, goto independently protected tail at 16
            0x0438, 8,      // 13: outside first try, if !second goto early return
            0x0000, // 15: fallthrough to tail
            0x1071, 1, 1,      // 16: tail(v1)
            0x000a, // 19: move-result v0
            0x0228, // 20: goto shared handler/normal return at 22
            0x1001, // 21: move v0, v1
            0x000f, // 22: return v0, handler has no move-exception
        ],
        offset: 0,
    });
    (class, method)
}

fn earlier_control_crossing_return_frontier_fixture() -> (DexClass, DexMethod) {
    let (class, mut method) = crossing_return_frontier_fixture();
    method.name = "runEarlier".into();
    let code = method.code.as_mut().unwrap();
    // Before the escaping conditional, this internal branch skips directly
    // to the later protected conditional. Splitting only at the escape would
    // leave a nonlinear prefix and reproduce the original exit-proof failure.
    code.instructions.splice(6..6, [0x0438, 7]);
    code.try_regions[0].end += 2;
    code.try_regions[1].start += 2;
    code.try_regions[1].end += 2;
    for region in &mut code.try_regions {
        region.catches = vec![(region.catches[0].0.clone(), 24)].into();
    }
    (class, method)
}

fn mixed_join_and_outside_handler_fixture() -> (DexClass, DexMethod) {
    let (class, mut method) = crossing_return_frontier_fixture();
    method.name = "runOutside".into();
    let code = method.code.as_mut().unwrap();
    // Normal completion and the first handler enter the early-success move.
    // The second handler lies one instruction beyond that enclosing join and
    // independently returns the original zero snapshot.
    code.instructions[20] = 0x0128;
    code.try_regions[1].catches = vec![
        (Some("Ljava/lang/IllegalStateException;".into()), 21),
        (Some("Ljava/lang/IllegalArgumentException;".into()), 22),
    ]
    .into();
    (class, method)
}

#[test]
fn shared_join_catch_and_independent_outside_catch_keep_distinct_results() {
    let (class, method) = mixed_join_and_outside_handler_fixture();
    let java = native_java::render_method("sample.Example", &class, &method)
        .unwrap()
        .source;
    assert!(java.contains("sample.Api.tail("), "{java}");
    assert!(
        java.contains("catch (java.lang.IllegalStateException"),
        "{java}"
    );
    assert!(
        java.contains("catch (java.lang.IllegalArgumentException"),
        "{java}"
    );
}

#[test]
fn first_control_split_preserves_internal_branch_before_protected_escape() {
    let (class, method) = earlier_control_crossing_return_frontier_fixture();
    let java = native_java::render_method("sample.Example", &class, &method)
        .unwrap()
        .source;
    assert!(java.contains("sample.Api.protectedCall("), "{java}");
    assert!(java.contains("sample.Api.handler("), "{java}");
    assert!(java.contains("sample.Api.tail("), "{java}");
    assert!(
        java.contains("catch (java.lang.IllegalArgumentException"),
        "{java}"
    );
    assert!(
        java.contains("catch (java.lang.IllegalStateException"),
        "{java}"
    );
}

#[test]
fn pure_protected_escape_preserves_later_independent_try_and_shared_return() {
    let (class, method) = crossing_return_frontier_fixture();
    let java = native_java::render_method("sample.Example", &class, &method)
        .unwrap()
        .source;
    for name in ["protectedCall", "handler", "tail"] {
        assert!(java.contains(&format!("sample.Api.{name}(")), "{java}");
    }
    assert!(
        java.contains("catch (java.lang.IllegalArgumentException"),
        "{java}"
    );
    assert!(
        java.contains("catch (java.lang.IllegalStateException"),
        "{java}"
    );
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn pure_protected_escape_jvm_preserves_dispatch_effects_and_exception_identity() {
    use std::{fs, process::Command};
    let (class, method) = crossing_return_frontier_fixture();
    let rendered = native_java::render_method("sample.Example", &class, &method)
        .unwrap()
        .source
        .replace("sample.Api", "Api");
    let (earlier_class, earlier_method) = earlier_control_crossing_return_frontier_fixture();
    let earlier_rendered =
        native_java::render_method("sample.Example", &earlier_class, &earlier_method)
            .unwrap()
            .source
            .replace("sample.Api", "Api");
    let (outside_class, outside_method) = mixed_join_and_outside_handler_fixture();
    let outside_rendered =
        native_java::render_method("sample.Example", &outside_class, &outside_method)
            .unwrap()
            .source
            .replace("sample.Api", "Api");
    let java = format!(
        r#"public class CrossingReturnFrontier {{
        static class Api {{
            static int fail, calls, handlers, tails;
            static final IllegalArgumentException argument=new IllegalArgumentException("argument identity");
            static final IllegalStateException state=new IllegalStateException("state identity");
            static int protectedCall() {{ calls++; if(fail==1)throw argument; if(fail==5)throw state; return 8; }}
            static void handler() {{ handlers++; if(fail==2)throw argument; }}
            static int tail(int input) {{ tails++; if(fail==3)throw state; if(fail==4)throw argument; return input+8; }}
            static void reset(int failure) {{ fail=failure; calls=handlers=tails=0; }}
        }}
        {rendered}
        {earlier_rendered}
        {outside_rendered}
        static int dex(boolean earlier,boolean first,boolean second) {{
            int v0=0,v1=1;
            try {{
                Api.protectedCall();
                if(earlier&&!second)return v1;
                if(!first)return v1;
                Api.handler();
            }} catch(IllegalArgumentException discarded) {{ return v0; }}
            if(!second)return v1;
            try {{ v0=Api.tail(v1); }} catch(IllegalStateException discarded) {{}}
            return v0;
        }}
        static Object outcome(boolean generated,boolean earlier,boolean first,boolean second) {{
            try {{ return Integer.valueOf(generated?(earlier?runEarlier(first,second):run(first,second)):dex(earlier,first,second)); }}
            catch(Throwable fault) {{ return fault; }}
        }}
        static int dexOutside(boolean first,boolean second) {{
            int v0=0,v1=1;
            try {{
                Api.protectedCall();
                if(!first)return v1;
                Api.handler();
            }} catch(IllegalArgumentException discarded) {{ return v0; }}
            if(!second)return v1;
            try {{ v0=Api.tail(v1); }}
            catch(IllegalStateException discarded) {{ return v1; }}
            catch(IllegalArgumentException discarded) {{ return v0; }}
            return v1;
        }}
        static Object outsideOutcome(boolean generated,boolean first,boolean second) {{
            try {{ return Integer.valueOf(generated?runOutside(first,second):dexOutside(first,second)); }}
            catch(Throwable fault) {{ return fault; }}
        }}
        public static void main(String[] args) {{
            for(boolean earlier:new boolean[]{{false,true}})
            for(boolean first:new boolean[]{{false,true}})
            for(boolean second:new boolean[]{{false,true}})
            for(int failure=0;failure<6;failure++) {{
                Api.reset(failure);
                Object expected=outcome(false,earlier,first,second);
                int calls=Api.calls,handlers=Api.handlers,tails=Api.tails;
                Api.reset(failure);
                Object actual=outcome(true,earlier,first,second);
                if(expected instanceof Throwable?actual!=expected:!expected.equals(actual))
                    throw new AssertionError("result or identity first="+first+" second="+second+" failure="+failure);
                if(Api.calls!=calls||Api.handlers!=handlers||Api.tails!=tails
                    ||calls!=1||handlers>1||tails>1)
                    throw new AssertionError("effect counts first="+first+" second="+second+" failure="+failure);
            }}
            for(boolean first:new boolean[]{{false,true}})
            for(boolean second:new boolean[]{{false,true}})
            for(int failure=0;failure<6;failure++) {{
                Api.reset(failure);
                Object expected=outsideOutcome(false,first,second);
                int calls=Api.calls,handlers=Api.handlers,tails=Api.tails;
                Api.reset(failure);
                Object actual=outsideOutcome(true,first,second);
                if(expected instanceof Throwable?actual!=expected:!expected.equals(actual))
                    throw new AssertionError("mixed handler result or identity failure="+failure);
                if(Api.calls!=calls||Api.handlers!=handlers||Api.tails!=tails)
                    throw new AssertionError("mixed handler effect counts failure="+failure);
                if(first&&second&&failure==3&&!Integer.valueOf(1).equals(actual))
                    throw new AssertionError("shared join handler result");
                if(first&&second&&failure==4&&!Integer.valueOf(0).equals(actual))
                    throw new AssertionError("outside handler snapshot");
            }}
        }}
    }}"#
    );
    let dir = std::env::temp_dir().join(format!(
        "rdx-crossing-return-frontier-{}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("CrossingReturnFrontier.java"), &java).unwrap();
    for (program, arg) in [
        ("javac", "CrossingReturnFrontier.java"),
        ("java", "CrossingReturnFrontier"),
    ] {
        let output = Command::new(program)
            .arg(arg)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{program}: {}\n{java}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
