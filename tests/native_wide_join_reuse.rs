//! Join ownership follows the incoming values, not an obsolete entry wide pair.
use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_java,
    native_method::MethodAnalysis,
};
use std::{fs, process::Command, sync::Arc};

fn fixture(words: Vec<u16>, return_type: &str) -> DexClass {
    let descriptor: Arc<str> = "Lsample/WideJoinReuse;".into();
    DexClass {
        symbols: Arc::new(DexSymbols::default()),
        descriptor: descriptor.clone(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![DexMethod {
            declaring_type: descriptor,
            name: "choose".into(),
            return_type: return_type.into(),
            parameters: vec!["I".into()],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 5,
                ins: 1,
                outs: 0,
                tries: 0,
                try_regions: vec![],
                instructions: words,
                offset: 0,
            }),
        }],
    }
}

fn branch_fixture() -> DexClass {
    fixture(
        vec![
            0x0016, 0, // old wide v0/v1
            0x0438, 4,      // if selector == 0 -> @6
            0x1112, // v1 = 1: invalidate old head v0
            0x0228, // -> join @7
            0x2112, // v1 = 2: invalidate old head v0
            0x010f, // return fresh narrow v1
        ],
        "I",
    )
}

fn switch_fixture() -> DexClass {
    fixture(
        vec![
            0x0016, 0, // old wide v0/v1
            0x042b, 10, 0,      // switch v4, payload @12
            0x3112, // default: v1 = 3
            0x0528, // -> join @11
            0x1112, // case 0: v1 = 1
            0x0328, // -> join @11
            0x2112, // case 1: v1 = 2
            0x0128, // -> join @11
            0x010f, // return fresh narrow v1
            0x0100, 2, 0, 0, 5, 0, 7, 0, // packed-switch payload
        ],
        "I",
    )
}

fn source(class: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.WideJoinReuse", class, &class.methods[0])?.source)
}

#[test]
fn branch_reuses_an_old_wide_tail_as_a_live_narrow_head() {
    let class = branch_fixture();
    MethodAnalysis::build(&class, &class.methods[0]).unwrap();
    let java = source(&class).unwrap();
    assert!(java.contains("return ") && java.contains("if ("), "{java}");
    assert!(!java.contains("wide-tail"), "{java}");
}

#[test]
fn switch_reuses_an_old_wide_tail_in_every_incoming_path() {
    let class = switch_fixture();
    MethodAnalysis::build(&class, &class.methods[0]).unwrap();
    let java = source(&class).unwrap();
    assert!(
        java.contains("switch (") && java.contains("return "),
        "{java}"
    );
    assert!(!java.contains("wide-tail"), "{java}");
}

#[test]
fn live_wide_join_rejects_a_predecessor_that_replaced_only_the_upper_word() {
    let class = fixture(vec![0x0016, 0, 0x0438, 4, 0x1112, 0x0228, 0, 0x0010], "J");
    MethodAnalysis::build(&class, &class.methods[0]).unwrap();
    assert!(source(&class).is_err());
}

#[test]
fn narrow_join_rejects_a_predecessor_without_a_fresh_upper_word_definition() {
    for words in [
        vec![0x0016, 0, 0x0438, 4, 0x1112, 0x0228, 0, 0x010f],
        vec![0, 0, 0x0438, 4, 0x1112, 0x0228, 0, 0x010f],
    ] {
        let class = fixture(words, "I");
        MethodAnalysis::build(&class, &class.methods[0]).unwrap();
        assert!(source(&class).is_err());
    }
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn wide_tail_reuse_jvm_returns_exact_branch_and_switch_values() {
    let directory =
        std::env::temp_dir().join(format!("rdx-wide-join-reuse-{}", std::process::id()));
    fs::create_dir_all(directory.join("sample")).unwrap();
    for (class, expression) in [
        (branch_fixture(), "selector == 0 ? 2 : 1"),
        (
            switch_fixture(),
            "selector == 0 ? 1 : selector == 1 ? 2 : 3",
        ),
    ] {
        let java = format!(
            "package sample; public class WideJoinReuse {{\n{}\npublic static void main(String[] args) {{ for(int selector : new int[] {{Integer.MIN_VALUE,-1,0,1,2,Integer.MAX_VALUE}}) {{ int expected={expression}; if(choose(selector)!=expected) throw new AssertionError(selector); }} }} }}",
            source(&class).unwrap()
        );
        fs::write(directory.join("sample/WideJoinReuse.java"), java).unwrap();
        for (program, argument) in [
            ("javac", "sample/WideJoinReuse.java"),
            ("java", "sample.WideJoinReuse"),
        ] {
            let result = Command::new(program)
                .arg(argument)
                .current_dir(&directory)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{program}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
    }
}
