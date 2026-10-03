#[path = "../src/native_java/allocation.rs"]
mod allocation;
use allocation::{Allocation, Capture, Event, Expr, Symbol};
fn sym(text: &str, label: &str) -> Symbol {
    Symbol {
        text: text.into(),
        label: label.into(),
    }
}
fn call(site: usize, target: Expr, name: &str, args: Vec<Expr>) -> Expr {
    Expr::Call {
        site,
        target: Box::new(target),
        method: sym(name, name),
        args,
    }
}
fn fixture(ty: &str, first: &str) -> (Allocation, Vec<Event>) {
    let a = Allocation {
        site: 0,
        constructor_site: 4,
        ty: sym("Holder", "Holder"),
        captures: vec![
            Capture {
                ty: ty.into(),
                name: "prior".into(),
                expression: call(1, Expr::Local("source".into()), first, vec![]),
            },
            Capture {
                ty: "Receiver".into(),
                name: "receiver".into(),
                expression: Expr::FieldRead {
                    site: 2,
                    receiver: Box::new(Expr::Local("source".into())),
                    field: sym("receiver", "Source.receiver"),
                },
            },
            Capture {
                ty: "java.lang.String".into(),
                name: "result".into(),
                expression: call(3, Expr::Capture(1), "b", vec![Expr::Capture(0)]),
            },
        ],
        arguments: vec![Expr::Capture(2)],
    };
    let e = vec![
        Event::Allocate {
            site: 0,
            ty: "Holder".into(),
        },
        Event::Call {
            site: 1,
            method: first.into(),
        },
        Event::Read {
            site: 2,
            field: "Source.receiver".into(),
        },
        Event::Call {
            site: 3,
            method: "b".into(),
        },
        Event::Construct {
            site: 4,
            ty: "Holder".into(),
        },
    ];
    (a, e)
}
#[test]
fn earlier_argument_call_precedes_later_receiver_field_and_keeps_both_links() {
    let (a, e) = fixture("java.lang.Class", "firstRef");
    let r = a.render_checked(&e, &["source"]).unwrap();
    assert_eq!(r.events, e);
    assert!(r.expression.contains("== null ?"), "{}", r.expression);
    assert_eq!(r.expression.matches("firstRef()").count(), 1);
    assert_eq!(r.expression.matches("source.receiver").count(), 2);
    assert_eq!(r.expression.matches(".b(prior)").count(), 1);
    let receiver_links: Vec<_> = r
        .links
        .iter()
        .filter(|l| l.label == "Source.receiver")
        .collect();
    assert_eq!(receiver_links.len(), 2);
    for l in &r.links {
        let token: String = r
            .expression
            .chars()
            .skip(l.start)
            .take(l.end - l.start)
            .collect();
        assert!(
            matches!(token.as_str(), "Holder" | "firstRef" | "receiver" | "b"),
            "{} => {token}",
            l.label
        );
    }
}
#[test]
fn scalar_comparisons_keep_exact_events_and_assign_live_captures() {
    for (ty, comparison) in [
        ("boolean", "== false"),
        ("byte", "== 0"),
        ("char", "== 0"),
        ("short", "== 0"),
        ("int", "== 0"),
        ("long", "== 0L"),
        ("float", "== 0.0f"),
        ("double", "== 0.0d"),
    ] {
        let (a, e) = fixture(ty, "first");
        let r = a.render_checked(&e, &["source"]).unwrap();
        assert_eq!(r.events, e);
        assert!(r.expression.contains(comparison), "{}", r.expression);
        assert_eq!(
            r.declarations,
            [
                format!("{ty} prior;"),
                "Receiver receiver;".into(),
                "java.lang.String result;".into()
            ]
        );
        assert_eq!(r.expression.matches("receiver =").count(), 2);
        assert_eq!(r.expression.matches("result =").count(), 1);
    }
}
#[test]
fn sequencing_rejects_invalid_domains_trace_changes_cycles_and_expansion() {
    for ty in ["void", "var", "null", "<unknown>", "<wide-tail>", "?", ""] {
        let (a, e) = fixture(ty, "first");
        assert!(a.render_checked(&e, &["source"]).is_err(), "accepted {ty}");
    }
    let (mut a, mut e) = fixture("int", "first");
    e.swap(1, 2);
    assert!(
        a.render_checked(&e, &["source"])
            .unwrap_err()
            .to_string()
            .contains("effect order")
    );
    e.swap(1, 2);
    a.captures[0].expression = Expr::Capture(1);
    assert!(a.render_checked(&e, &["source"]).is_err());
    let (mut a, e) = fixture("int", "first");
    if let Expr::FieldRead { field, .. } = &mut a.captures[1].expression {
        field.text = "x".repeat(9000);
    }
    assert!(
        a.render_checked(&e, &["source"])
            .unwrap_err()
            .to_string()
            .contains("budget")
    );
    let (mut a, _) = fixture("int", "first");
    a.captures = (0..10)
        .map(|i| Capture {
            ty: "int".into(),
            name: format!("c{i}"),
            expression: call(i + 1, Expr::Local("source".into()), "first", vec![]),
        })
        .collect();
    a.arguments = vec![Expr::Capture(9)];
    assert!(
        a.render_checked(&[], &["source"])
            .unwrap_err()
            .to_string()
            .contains("budget")
    );
}
#[test]
#[ignore = "requires javac and java"]
fn jvm_both_arms_scalars_throwing_operands_and_live_aliases() {
    use std::process::Command;
    let cases = [
        ("refNull", "java.lang.Class", "firstRef", "null"),
        ("refValue", "java.lang.Class", "firstRef", "String.class"),
        ("boolFalse", "boolean", "firstBool", "false"),
        ("boolTrue", "boolean", "firstBool", "true"),
        ("intZero", "int", "firstInt", "0"),
        ("intMax", "int", "firstInt", "Integer.MAX_VALUE"),
        ("longZero", "long", "firstLong", "0L"),
        ("longMin", "long", "firstLong", "Long.MIN_VALUE"),
        ("floatZero", "float", "firstFloat", "-0.0f"),
        ("floatNan", "float", "firstFloat", "Float.NaN"),
        ("doubleZero", "double", "firstDouble", "0.0d"),
        ("doubleNan", "double", "firstDouble", "Double.NaN"),
        ("byteMin", "byte", "firstByte", "Byte.MIN_VALUE"),
        ("charMax", "char", "firstChar", "Character.MAX_VALUE"),
        ("shortMin", "short", "firstShort", "Short.MIN_VALUE"),
        ("array", "int[]", "firstArray", "new int[]{3,4}"),
    ];
    let mut methods = String::new();
    for (name, ty, first, value) in cases {
        let (a, e) = fixture(ty, first);
        let r = a.render_checked(&e, &["source"]).unwrap();
        methods.push_str(&format!("static void {name}() {{ log.setLength(0); Source source = new Source(); source.value = {value}; {} Holder h = {}; if (!log.toString().equals(\"PBC\") || receiver != source.receiver || !h.value.equals(result) || !java.util.Objects.equals(prior, source.value) || !result.startsWith(\"fresh:\")) throw new AssertionError(\"{name}:\" + log); }}\n", r.declarations.join(" "), r.expression));
    }
    let (a, e) = fixture("java.lang.Class", "firstRef");
    let r = a.render_checked(&e, &["source"]).unwrap();
    let throwing = format!(
        "static void throwing(int mode) {{ log.setLength(0); Source source = new Source(); source.mode = mode; {} try {{ Holder h = {}; throw new AssertionError(\"no throw\"); }} catch (RuntimeException ex) {{ if (mode == 3 ? !(ex instanceof NullPointerException) : ex != marker) throw new AssertionError(ex); if (!log.toString().equals(mode == 2 ? \"PB\" : \"P\")) throw new AssertionError(log); }} }}",
        r.declarations.join(" "),
        r.expression
    );
    let java = format!(
        r#"public class SequenceCheck {{
static StringBuilder log = new StringBuilder(); static RuntimeException marker = new RuntimeException("marker");
static class Holder {{ static {{ if (log.length() != 0) throw new AssertionError("allocation delayed:" + log); }} String value; Holder(String x) {{ log.append('C'); value=x; }} }}
static class Receiver {{ String name; boolean fail; Receiver(String n, boolean f) {{ name=n; fail=f; }} String b(Object x) {{ log.append('B'); if(fail) throw marker; return name+":"+x; }} String b(char x) {{ return b(Character.valueOf(x)); }} String b(boolean x) {{ return b(Boolean.valueOf(x)); }} String b(int x) {{ return b(Integer.valueOf(x)); }} String b(long x) {{ return b(Long.valueOf(x)); }} String b(float x) {{ return b(Float.valueOf(x)); }} String b(double x) {{ return b(Double.valueOf(x)); }} }}
static class Source {{ Object value; int mode; Receiver receiver = new Receiver("stale",false); Object prior() {{ log.append('P'); if(mode==1) throw marker; receiver=mode==3?null:new Receiver("fresh",mode==2); return value; }} byte firstByte() {{ return (Byte) prior(); }} char firstChar() {{ return (Character) prior(); }} short firstShort() {{ return (Short) prior(); }} int[] firstArray() {{ return (int[]) prior(); }} Class firstRef() {{ return (Class) prior(); }} boolean firstBool() {{ return (Boolean) prior(); }} int firstInt() {{ return (Integer) prior(); }} long firstLong() {{ return (Long) prior(); }} float firstFloat() {{ return (Float) prior(); }} double firstDouble() {{ return (Double) prior(); }} }}
{methods}
{throwing}
public static void main(String[] args) {{ refNull(); refValue(); boolFalse(); boolTrue(); intZero(); intMax(); longZero(); longMin(); floatZero(); floatNan(); doubleZero(); doubleNan(); byteMin(); charMax(); shortMin(); array(); throwing(1); throwing(2); throwing(3); System.out.print("OK"); }}
}}"#
    );
    let dir = std::env::temp_dir().join(format!("rdx-allocation-sequence-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("SequenceCheck.java"), java).unwrap();
    let compiled = Command::new("javac")
        .arg(dir.join("SequenceCheck.java"))
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let run = Command::new("java")
        .arg("-cp")
        .arg(&dir)
        .arg("SequenceCheck")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(run.stdout, b"OK");
}

#[test]
fn reference_cast_prefix_retains_exact_type_link_and_capture_identity() {
    let (mut a, mut events) = fixture("Receiver", "firstRef");
    a.captures[0].expression = Expr::CheckCast {
        site: 5,
        ty: sym("Receiver", "sample.Receiver"),
        value: Box::new(Expr::Cast {
            ty: sym("java.lang.Object", "java.lang.Object"),
            value: Box::new(call(1, Expr::Local("source".into()), "firstRef", vec![])),
        }),
    };
    events.insert(
        2,
        Event::CheckCast {
            site: 5,
            ty: "sample.Receiver".into(),
        },
    );
    let r = a.render_checked(&events, &["source"]).unwrap();
    assert_eq!(r.events, events);
    assert_eq!(r.expression.matches("firstRef()").count(), 1);
    assert!(r.expression.contains(".b(prior)"), "{}", r.expression);
    for (label, expected) in [
        ("sample.Receiver", "Receiver"),
        ("java.lang.Object", "java.lang.Object"),
    ] {
        let links: Vec<_> = r.links.iter().filter(|l| l.label == label).collect();
        assert_eq!(links.len(), 1);
        let l = links[0];
        assert_eq!(
            r.expression
                .chars()
                .skip(l.start)
                .take(l.end - l.start)
                .collect::<String>(),
            expected
        );
    }
}

#[test]
fn production_decoder_sequences_strict_forwarder_and_ignored_object_append() {
    use rdx::{
        native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
        native_hierarchy::TypeHierarchy,
        native_java,
        native_method::MethodAnalysis,
    };
    use std::sync::Arc;
    let class = DexClass {
        descriptor: "Lsample/Test;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            strings: vec![
                "<init>".into(),
                "getClass".into(),
                "receiver".into(),
                "b".into(),
                "append".into(),
                "toString".into(),
                "suffix".into(),
            ],
            types: vec![
                "Lsample/E;".into(),
                "Ljava/lang/StringBuilder;".into(),
                "Ljava/lang/Object;".into(),
                "Ljava/lang/Class;".into(),
                "Lsample/Factory;".into(),
                "Lsample/Receiver;".into(),
                "Ljava/lang/IllegalArgumentException;".into(),
                "Lsample/Source;".into(),
            ],
            protos: vec![
                ("V".into(), vec![]),
                ("Ljava/lang/Class;".into(), vec![]),
                (
                    "Ljava/lang/Object;".into(),
                    vec!["Ljava/lang/Class;".into()],
                ),
                (
                    "Ljava/lang/StringBuilder;".into(),
                    vec!["Ljava/lang/Object;".into()],
                ),
                (
                    "Ljava/lang/StringBuilder;".into(),
                    vec!["Ljava/lang/String;".into()],
                ),
                ("Ljava/lang/String;".into(), vec![]),
                ("V".into(), vec!["Ljava/lang/String;".into()]),
            ],
            fields: vec![(4, 5, 2)],
            methods: vec![
                (1, 0, 0),
                (2, 1, 1),
                (5, 2, 3),
                (1, 3, 4),
                (1, 4, 4),
                (1, 5, 5),
                (6, 6, 0),
            ],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/Test;".into(),
            name: "make".into(),
            return_type: "Lsample/E;".into(),
            parameters: vec!["Lsample/Source;".into()],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 5,
                ins: 1,
                outs: 2,
                tries: 0,
                try_regions: vec![],
                offset: 0,
                instructions: vec![
                    0x0022, 0, 0x0122, 1, 0x1070, 0, 1, 0x106e, 1, 4, 0x040c, 0x0262, 0, 0x206e, 2,
                    0x0042, 0x040c, 0x206e, 3, 0x0041, 0x041a, 6, 0x206e, 4, 0x0041, 0x106e, 5, 1,
                    0x040c, 0x2070, 6, 0x0040, 0x0011,
                ],
            }),
        }],
    };
    let leaf = DexClass {
        descriptor: "Lsample/E;".into(),
        superclass: Some("Ljava/lang/IllegalArgumentException;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: class.symbols.clone(),
        methods: vec![],
    };
    let h = Arc::new(TypeHierarchy::from_classes([&class, &leaf]).unwrap());
    assert_eq!(h.recovered_constructors("Lsample/E;").len(), 1);
    assert!(h.recovered_constructors("Lsample/E;")[0].strict_allocation_order);
    assert!(class.symbols.hierarchy.set(h).is_ok());
    MethodAnalysis::build(&class, &class.methods[0])
        .unwrap()
        .infer_types()
        .unwrap();
    let out = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert!(
        out.source
            .contains("new sample.E(new java.lang.StringBuilder()"),
        "{}",
        out.source
    );
    assert!(out.source.contains("== null ?"), "{}", out.source);
    assert_eq!(out.source.matches("getClass()").count(), 1);
    assert_eq!(out.source.matches("sample.Factory.receiver").count(), 2);
    assert_eq!(out.source.matches(".b(").count(), 1);
    assert_eq!(out.source.matches(".append(").count(), 2);
    assert!(
        out.source.find("getClass()").unwrap()
            < out.source.find("sample.Factory.receiver").unwrap()
    );
    for label in [
        "java.lang.IllegalArgumentException.<init>(Ljava/lang/String;)V",
        "java.lang.StringBuilder.append(Ljava/lang/Object;)Ljava/lang/StringBuilder;",
    ] {
        assert!(
            out.links.iter().any(|l| l.label == label),
            "missing {label}"
        );
    }
}

#[test]
fn argument_block_keeps_capture_order_without_duplicated_continuations() {
    let (a, e) = fixture("int", "first");
    let r = a
        .render_argument_block_checked(&e, &["source"], &[])
        .unwrap();
    assert_eq!(r.events, e);
    assert!(r.declarations.iter().all(|d| !d.contains('=')));
    assert_eq!(r.expression.matches("source.first()").count(), 1);
    assert_eq!(r.expression.matches("yield result").count(), 1);
    let before = r.expression.find("source.first()").unwrap();
    let field = r.expression.find("source.receiver").unwrap();
    let call = r.expression.find("receiver.b(prior)").unwrap();
    assert!(before < field && field < call);
}
