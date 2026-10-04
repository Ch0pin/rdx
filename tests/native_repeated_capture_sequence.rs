#[path = "../src/native_java/allocation.rs"]
#[allow(dead_code)]
mod allocation;
use allocation::{Allocation, Capture, Event, Expr, Symbol};
fn sym(text: &str) -> Symbol {
    Symbol {
        text: text.into(),
        label: text.into(),
    }
}
fn call(site: usize, target: Expr, name: &str, args: Vec<Expr>) -> Expr {
    Expr::Call {
        site,
        target: Box::new(target),
        method: sym(name),
        args,
    }
}
fn fixture() -> (Allocation, Vec<Event>) {
    let child = Allocation {
        site: 3,
        constructor_site: 4,
        ty: sym("Builder"),
        captures: vec![],
        arguments: vec![],
    };
    let captures = vec![
        Capture {
            ty: "String".into(),
            name: "hóst".into(),
            expression: call(1, Expr::Local("uri".into()), "getHost", vec![]),
        },
        Capture {
            ty: "String".into(),
            name: "path".into(),
            expression: call(2, Expr::Local("uri".into()), "getPath", vec![]),
        },
        Capture {
            ty: "Builder".into(),
            name: "builder".into(),
            expression: Expr::SharedNew {
                allocation: Box::new(child),
                constructor_label: "Builder.init".into(),
            },
        },
        Capture {
            ty: "String".into(),
            name: "text".into(),
            expression: call(
                7,
                call(
                    6,
                    call(5, Expr::Capture(2), "append", vec![Expr::Capture(0)]),
                    "append",
                    vec![Expr::Capture(1)],
                ),
                "toString",
                vec![],
            ),
        },
    ];
    let events = vec![
        Event::Allocate {
            site: 0,
            ty: "Holder".into(),
        },
        Event::Call {
            site: 1,
            method: "getHost".into(),
        },
        Event::Call {
            site: 2,
            method: "getPath".into(),
        },
        Event::Allocate {
            site: 3,
            ty: "Builder".into(),
        },
        Event::Construct {
            site: 4,
            ty: "Builder".into(),
        },
        Event::Call {
            site: 5,
            method: "append".into(),
        },
        Event::Call {
            site: 6,
            method: "append".into(),
        },
        Event::Call {
            site: 7,
            method: "toString".into(),
        },
        Event::Construct {
            site: 8,
            ty: "Holder".into(),
        },
    ];
    (
        Allocation {
            site: 0,
            constructor_site: 8,
            ty: sym("Holder"),
            captures,
            arguments: vec![Expr::Capture(3), Expr::Capture(0), Expr::Capture(1)],
        },
        events,
    )
}
#[test]
fn repeated_equal_arms_become_one_in_place_block_with_exact_links() {
    let (a, events) = fixture();
    let r = a.render_checked(&events, &["uri"]).unwrap();
    assert_eq!(r.events, events);
    assert!(r.expression.contains("switch (0) { default -> { hóst = uri.getHost(); path = uri.getPath(); yield (builder = new Builder()); } }"), "{}", r.expression);
    assert!(!r.expression.contains(" ? "));
    for name in ["getHost", "getPath", "Builder", "append", "toString"] {
        let expected = if name == "append" { 2 } else { 1 };
        assert_eq!(
            r.links
                .iter()
                .filter(|link| link.label == name
                    || (name == "Builder" && link.label == "Builder.init"))
                .count(),
            expected
        );
    }
    let chars: Vec<_> = r.expression.chars().collect();
    for link in r.links {
        let token: String = chars[link.start..link.end].iter().collect();
        assert_eq!(
            token,
            if link.label == "Builder.init" {
                "Builder"
            } else {
                &link.label
            }
        );
    }
}
#[test]
fn changed_effect_order_forward_dependencies_and_output_budget_reject() {
    let (a, mut events) = fixture();
    events.swap(1, 2);
    assert!(a.render_checked(&events, &["uri"]).is_err());
    let (mut a, events) = fixture();
    a.captures[0].expression = Expr::Capture(1);
    assert!(a.render_checked(&events, &["uri"]).is_err());
    let (mut a, events) = fixture();
    if let Expr::Call { method, .. } = &mut a.captures[0].expression {
        method.text = "x".repeat(17000);
    }
    assert!(a.render_checked(&events, &["uri"]).is_err());
}
#[test]
#[ignore = "requires javac and java"]
fn jvm_nulls_definite_assignment_and_effect_exception_order() {
    use std::{fs, process::Command};
    let (a, e) = fixture();
    let r = a.render_checked(&e, &["uri"]).unwrap();
    let source = format!(
        r#"public class Check {{
static String trace; static int fail; static RuntimeException sentinel = new RuntimeException();
static void event(String value, int point) {{ trace += value; if (fail == point) throw sentinel; }}
static class Uri {{ String host, path; Uri(String h, String p) {{host=h;path=p;}} String getHost() {{event("H",1); return host;}} String getPath() {{event("P",2);return path;}} }}
static class Builder {{ StringBuilder value = new StringBuilder(); Builder() {{event("B",3);}} Builder append(String s) {{event("A",4);value.append(s);return this;}} public String toString() {{event("T",5);return value.toString();}} }}
static class Holder {{String value, host,path; Holder(String v,String h,String p) {{event("C",6);value=v;host=h;path=p;}}}}
static Holder emitted(Uri uri) {{ {} return {}; }}
static Holder original(Uri uri) {{ String h,p; Builder b; return new Holder(((h=uri.getHost())==null ? ((p=uri.getPath())==null ? (b=new Builder()) : (b=new Builder())) : ((p=uri.getPath())==null ? (b=new Builder()) : (b=new Builder()))).append(h).append(p).toString(),h,p); }}
static String outcome(boolean emitted, Uri uri) {{ trace="";try {{Holder h=emitted?emitted(uri):original(uri); return trace+":"+h.value+":"+h.host+":"+h.path;}}catch(RuntimeException e) {{if(e!=sentinel)throw new AssertionError(e);return trace+":throw";}} }}
public static void main(String[] args) {{int checks=0; for(String h:new String[]{{null,"host"}})for(String p:new String[]{{null,"/path"}})for(fail=0;fail<=6;fail++){{Uri uri=new Uri(h,p);String before=outcome(false,uri), after=outcome(true,uri);if(!before.equals(after))throw new AssertionError(before+" != "+after);if(fail==0&&!after.startsWith("HPBAATC:"+String.valueOf(h)+String.valueOf(p)+":"))throw new AssertionError(after);checks++;}}System.out.print(checks);}}
}}"#,
        r.declarations.join(" "),
        r.expression
    );
    let dir = std::env::temp_dir().join(format!("rdx-repeated-sequence-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("Check.java"), source).unwrap();
    let result = Command::new("javac")
        .arg(dir.join("Check.java"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let result = Command::new("java")
        .arg("-cp")
        .arg(&dir)
        .arg("Check")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, b"28");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "requires RDX_NOTIFICATIONS_APK with the supplied CapitalOne build"]
fn exact_apk_private_builder_normalizes_to_readable_concat_with_navigation() {
    use rdx::engine::{DecompilerEngine, NativeEngine};
    let apk = std::env::var_os("RDX_NOTIFICATIONS_APK").expect("APK required");
    let mut engine = NativeEngine::start().unwrap();
    engine.open(std::path::Path::new(&apk)).unwrap();
    let code = engine
        .decompile_with_metadata(
            "uk.co.capitalone.android.core.notifications.deeplink.NotificationsDeepLinkHandler",
        )
        .unwrap();
    assert!(
        code.source.contains("uri.getHost() + uri.getPath()"),
        "{}",
        code.source
    );
    for erased in [
        "switch (0)",
        "StringBuilder",
        "String text2;",
        "String text3;",
    ] {
        assert!(!code.source.contains(erased), "{}", code.source);
    }
    assert!(!code.source.contains("Native DEX disassembly"));
    assert!(!code.source.contains(" == null ? "));
    let chars: Vec<_> = code.source.chars().collect();
    for (suffix, token, count) in [
        (".getHost()Ljava/lang/String;", "getHost", 1),
        (".getPath()Ljava/lang/String;", "getPath", 1),
        (
            ".append(Ljava/lang/String;)Ljava/lang/StringBuilder;",
            "append",
            0,
        ),
        (".toString()Ljava/lang/String;", "toString", 0),
    ] {
        let links: Vec<_> = code
            .links
            .iter()
            .filter(|link| {
                let owner = if token == "getHost" || token == "getPath" {
                    "android.net.Uri"
                } else {
                    "java.lang.StringBuilder"
                };
                link.label == format!("{owner}{suffix}")
            })
            .collect();
        assert_eq!(links.len(), count, "{suffix}");
        for link in links {
            assert_eq!(
                chars[link.start..link.end].iter().collect::<String>(),
                token
            );
        }
    }
    for link in &code.links {
        assert!(link.start < link.end && link.end <= chars.len());
    }
}
