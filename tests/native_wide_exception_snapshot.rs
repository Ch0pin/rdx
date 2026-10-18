use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_java,
};
use std::sync::Arc;
fn fixture(shape: &str) -> (DexClass, DexMethod) {
    let (ret, parameters, registers, ins, words, start, end, handler, caught) = match shape {
        "fallbackJ" => (
            "J",
            vec!["Ljava/lang/String;", "J"],
            3,
            3,
            vec![0x1071, 0, 0, 0x000b, 0x0010, 0x0110],
            0,
            4,
            5,
            "Ljava/lang/RuntimeException;",
        ),
        "fallbackD" => (
            "D",
            vec!["Ljava/lang/String;", "D"],
            3,
            3,
            vec![0x1071, 0, 0, 0x000b, 0x0010, 0x0110],
            0,
            4,
            5,
            "Ljava/lang/RuntimeException;",
        ),
        "commit" => (
            "J",
            vec!["J"],
            4,
            2,
            vec![0x0216, 7, 0x0071, 1, 0, 0x0210, 0x000d, 0x0210],
            0,
            5,
            6,
            "Ljava/lang/RuntimeException;",
        ),
        "divide" => (
            "J",
            vec!["J", "J"],
            4,
            4,
            vec![0x009e, 0x0200, 0x0010, 0x020d, 0x0010],
            0,
            2,
            3,
            "Ljava/lang/ArithmeticException;",
        ),
        "swap" => (
            "J",
            vec!["J", "J"],
            6,
            4,
            vec![
                0x2004, 0x4204, 0x0404, 0x0071, 1, 0, 0x009c, 0x0402, 0x0010, 0x000d, 0x009c,
                0x0402, 0x0010,
            ],
            0,
            6,
            9,
            "Ljava/lang/RuntimeException;",
        ),
        "partial" => (
            "J",
            vec!["J"],
            4,
            2,
            vec![0x1312, 0x0071, 1, 0, 0x0210, 0x000d, 0x0210],
            0,
            4,
            5,
            "Ljava/lang/RuntimeException;",
        ),
        "typechange" => (
            "J",
            vec!["J"],
            4,
            2,
            vec![0x2286, 0x0071, 1, 0, 0x0210, 0x000d, 0x0210],
            0,
            4,
            5,
            "Ljava/lang/RuntimeException;",
        ),
        _ => panic!("shape"),
    };
    let symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Api;".into()],
        strings: vec!["parse".into(), "fail".into()],
        protos: vec![
            (ret.into(), vec!["Ljava/lang/String;".into()]),
            ("V".into(), vec![]),
        ],
        methods: vec![(0, 0, 0), (0, 1, 1)],
        ..Default::default()
    });
    let method = DexMethod {
        declaring_type: "Lsample/Example;".into(),
        name: shape.into(),
        return_type: ret.into(),
        parameters: parameters.into_iter().map(Into::into).collect(),
        thrown_types: vec![],
        access_flags: 9,
        code: Some(DexCode {
            registers,
            ins,
            outs: 1,
            tries: 1,
            try_regions: vec![DexTryRegion {
                start,
                end,
                catches: vec![(Some(caught.into()), handler)].into(),
            }],
            instructions: words,
            offset: 0,
        }),
    };
    let class = DexClass {
        symbols,
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
    (class, method)
}
fn render(shape: &str) -> Result<rdx::engine::DecompiledCode, anyhow::Error> {
    let (c, m) = fixture(shape);
    native_java::render_method("sample.Example", &c, &m)
}
#[test]
fn long_and_double_fallback_snapshots_keep_complete_pairs() {
    for shape in ["fallbackJ", "fallbackD", "commit", "divide", "swap"] {
        let s = render(shape).unwrap().source;
        assert!(s.contains("catch"), "{s}");
        assert!(!s.contains("throws"), "{s}");
    }
}
#[test]
fn partial_overwrite_and_changed_wide_type_remain_rejected() {
    for shape in ["partial", "typechange"] {
        assert!(render(shape).is_err(), "{shape}");
    }
}
#[test]
fn wide_snapshot_call_links_preserve_exact_navigation_spelling() {
    let s = render("fallbackJ").unwrap();
    let l = s
        .links
        .iter()
        .find(|l| l.label == "sample.Api.parse(Ljava/lang/String;)J")
        .unwrap();
    assert_eq!(
        s.source
            .chars()
            .skip(l.start)
            .take(l.end - l.start)
            .collect::<String>(),
        "parse"
    );
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn jvm_preserves_wide_fallback_commit_division_and_pair_swap() {
    for shape in ["fallbackJ", "fallbackD", "commit", "divide", "swap"] {
        let source = render(shape).unwrap().source;
        let dir =
            std::env::temp_dir().join(format!("rdx-wide-snapshot-{}-{shape}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(dir.clone());
        let (api, checks) = match shape {
            "fallbackJ" => (
                "static long parse(String value){if(fail)throw original;return Long.parseLong(value);}",
                "for(boolean failed:new boolean[]{false,true}){Api.fail=failed;long result=Example.fallbackJ(\"123\",0x100000002L);if(result!=(failed?0x100000002L:123L))throw new AssertionError(result);}",
            ),
            "fallbackD" => (
                "static double parse(String value){if(fail)throw original;return Double.parseDouble(value);}",
                "for(boolean failed:new boolean[]{false,true}){Api.fail=failed;double seed=Double.longBitsToDouble(0x7ff8000012345678L);double result=Example.fallbackD(\"1.25\",seed);if(Double.doubleToRawLongBits(result)!=Double.doubleToRawLongBits(failed?seed:1.25))throw new AssertionError(result);}",
            ),
            "commit" => (
                "",
                "for(boolean failed:new boolean[]{false,true}){Api.fail=failed;if(Example.commit(0x100000002L)!=7L)throw new AssertionError();}",
            ),
            "divide" => (
                "",
                "long seed=0x100000002L;if(Example.divide(seed,0L)!=seed||Example.divide(seed,2L)!=seed/2L)throw new AssertionError();",
            ),
            "swap" => (
                "",
                "for(boolean failed:new boolean[]{false,true}){Api.fail=failed;if(Example.swap(0x100000002L,3L)!=3L-0x100000002L)throw new AssertionError();}",
            ),
            _ => unreachable!(),
        };
        let java = format!(
            "package sample;class Api{{static boolean fail;static RuntimeException original=new RuntimeException();static void fail(){{if(fail)throw original;}}{api}}}class Example{{{source}}}public class Runner{{public static void main(String[]args){{{checks}}}}}"
        );
        let path = dir.join("Runner.java");
        std::fs::write(&path, java).unwrap();
        let output = std::process::Command::new("javac")
            .arg("-d")
            .arg(&dir)
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{shape}: {}\n{source}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = std::process::Command::new("java")
            .arg("-cp")
            .arg(&dir)
            .arg("sample.Runner")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{shape}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn truncated_wide_pair_is_rejected() {
    let (c, mut m) = fixture("commit");
    m.code.as_mut().unwrap().registers = 3;
    assert!(native_java::render_method("sample.Example", &c, &m).is_err());
}
