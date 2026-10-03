use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_java,
};
use std::sync::Arc;

fn fixture(parameters: &[&str], words: &[u16], registers: u16) -> DexClass {
    let parameters: Vec<Arc<str>> = parameters.iter().map(|ty| Arc::from(*ty)).collect();
    DexClass {
        descriptor: "Lsample/WideChild;".into(),
        superclass: Some("Lsample/WideBase;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            types: vec!["Lsample/WideBase;".into()],
            strings: vec!["<init>".into()],
            protos: vec![("V".into(), parameters.clone())],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/WideChild;".into(),
            name: "<init>".into(),
            return_type: "V".into(),
            parameters,
            thrown_types: vec![],
            access_flags: 0x10001,
            code: Some(DexCode {
                registers,
                ins: registers,
                outs: registers,
                tries: 0,
                try_regions: vec![],
                instructions: words.to_vec(),
                offset: 0,
            }),
        }],
    }
}
fn render(class: &DexClass) -> anyhow::Result<rdx::engine::DecompiledCode> {
    native_java::render_method("sample.WideChild", class, &class.methods[0])
}

#[test]
fn super_constructor_reads_wide_heads_in_packed_and_range_calls() {
    for words in [vec![0x3070, 0, 0x0210, 0x000e], vec![0x0376, 0, 0, 0x000e]] {
        let class = fixture(&["J"], &words, 3);
        let rendered = render(&class).unwrap();
        assert!(
            rendered.source.contains("super(p0);"),
            "{}",
            rendered.source
        );
        assert!(
            rendered
                .links
                .iter()
                .any(|link| link.label == "sample.WideBase.<init>(J)V")
        );
    }
    let class = fixture(&["J", "D", "J"], &[0x0776, 0, 0, 0x000e], 7);
    assert!(
        render(&class)
            .unwrap()
            .source
            .contains("super(p0, p1, p2);")
    );
}

#[test]
fn wide_constructor_rejects_nonadjacent_words_and_reference_alias_of_this() {
    let malformed = fixture(&["J"], &[0x3070, 0, 0x0110, 0x000e], 3);
    assert!(render(&malformed).is_err());
    let mut escaped = fixture(
        &["Ljava/lang/Object;"],
        &[0x0107, 0x2070, 0, 0x0010, 0x000e],
        2,
    );
    escaped.methods[0].parameters = vec!["Ljava/lang/Object;".into()];
    assert!(render(&escaped).is_err());
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn emitted_wide_constructor_preserves_values_and_exception_identity() {
    use std::{fs, process::Command};
    let class = fixture(&["J", "D", "J"], &[0x0776, 0, 0, 0x000e], 7);
    let rendered = render(&class).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-wide-super-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let harness = r#"
package sample;
class WideBase {
    static final RuntimeException marker = new RuntimeException();
    final long first; final double second; final long third;
    WideBase(long a, double b, long c) { if (a == 17) throw marker; first=a; second=b; third=c; }
}
public class WideChild extends WideBase {
METHOD
    public static void main(String[] args) {
        WideChild value = new WideChild(Long.MIN_VALUE, -0.0d, Long.MAX_VALUE);
        if (value.first != Long.MIN_VALUE || Double.doubleToRawLongBits(value.second) != Long.MIN_VALUE || value.third != Long.MAX_VALUE) throw new AssertionError();
        try { new WideChild(17, Double.NaN, 3); throw new AssertionError(); }
        catch (RuntimeException actual) { if (actual != marker) throw new AssertionError(); }
    }
}
"#;
    fs::write(
        dir.join("sample/WideChild.java"),
        harness.replace("METHOD", &rendered.source),
    )
    .unwrap();
    for (program, argument) in [
        ("javac", "sample/WideChild.java"),
        ("java", "sample.WideChild"),
    ] {
        let result = Command::new(program)
            .arg(argument)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{program}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
