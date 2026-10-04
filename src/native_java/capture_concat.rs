//! One fresh, unescaped platform builder with two String operands.
//! This is source-level concat normalization: operand/application effects stay
//! ordered; private allocation/OOM timing and empty-result identity are excluded.
use super::super::allocation::{Allocation, Event, Expr};
use super::{exact_type, substitute};
use crate::native_dex::DexClass;
const BUILDER: &str = "java.lang.StringBuilder";
const STRING: &str = "java.lang.String";
fn uses(allocation: &Allocation, index: usize) -> usize {
    let mut count = 0;
    for expr in allocation
        .captures
        .iter()
        .map(|c| &c.expression)
        .chain(&allocation.arguments)
    {
        substitute(&mut expr.clone(), index, &Expr::Null, &mut count);
    }
    count
}
fn call<'a>(expr: &'a Expr, index: usize, signature: &str) -> Option<(usize, &'a [Expr])> {
    let Expr::Call {
        site,
        target,
        method,
        args,
    } = expr
    else {
        return None;
    };
    (matches!(target.as_ref(),Expr::Capture(i) if *i==index) && method.label == signature)
        .then_some((*site, args))
}
pub(super) fn normalize(
    class: &DexClass,
    allocation: &Allocation,
    events: &[Event],
    live: &[bool],
) -> Option<(Allocation, Vec<Event>, Vec<bool>)> {
    if allocation.captures.len() > 16 || allocation.captures.len() != live.len() {
        return None;
    }
    let hierarchy = class.symbols.hierarchy.get()?;
    if !["Ljava/lang/StringBuilder;", "Ljava/lang/String;"]
        .iter()
        .all(|ty| hierarchy.unshadowed_sdk_type(ty))
    {
        return None;
    }
    for builder in 2..allocation.captures.len().saturating_sub(3) {
        let host = builder - 2;
        let path = builder - 1;
        let first = builder + 1;
        let second = builder + 2;
        let result = builder + 3;
        if [host, path, builder, first, second]
            .iter()
            .any(|i| live[*i] || uses(allocation, *i) != 1)
        {
            continue;
        }
        if [host, path].iter().any(|i| {
            allocation.captures[*i].ty != STRING
                || exact_type(&allocation.captures[*i].expression).as_deref() != Some(STRING)
        }) {
            continue;
        }
        if [builder, first, second]
            .iter()
            .any(|i| allocation.captures[*i].ty != BUILDER)
            || allocation.captures[result].ty != STRING
        {
            continue;
        }
        let Expr::SharedNew {
            allocation: fresh,
            constructor_label,
        } = &allocation.captures[builder].expression
        else {
            continue;
        };
        if fresh.ty.label != BUILDER
            || !fresh.captures.is_empty()
            || !fresh.arguments.is_empty()
            || constructor_label != "java.lang.StringBuilder.<init>()V"
        {
            continue;
        }
        let append = "java.lang.StringBuilder.append(Ljava/lang/String;)Ljava/lang/StringBuilder;";
        let Some((append1, a)) = call(&allocation.captures[first].expression, builder, append)
        else {
            continue;
        };
        if a != [Expr::Capture(host)] {
            continue;
        }
        let Some((append2, a)) = call(&allocation.captures[second].expression, first, append)
        else {
            continue;
        };
        if a != [Expr::Capture(path)] {
            continue;
        }
        let tostring = "java.lang.StringBuilder.toString()Ljava/lang/String;";
        let Some((finish, a)) = call(&allocation.captures[result].expression, second, tostring)
        else {
            continue;
        };
        if !a.is_empty() {
            continue;
        }
        let private = [
            Event::Allocate {
                site: fresh.site,
                ty: BUILDER.into(),
            },
            Event::Construct {
                site: fresh.constructor_site,
                ty: BUILDER.into(),
            },
            Event::Call {
                site: append1,
                method: append.into(),
            },
            Event::Call {
                site: append2,
                method: append.into(),
            },
            Event::Call {
                site: finish,
                method: tostring.into(),
            },
        ];
        let Some(start) = events
            .windows(private.len())
            .position(|window| window == private)
        else {
            continue;
        };
        // Exact contiguous, uniquely identified intrinsic events only. Everything
        // before and after remains in the independently checked effect trace.
        if private
            .iter()
            .any(|event| events.iter().filter(|e| *e == event).count() != 1)
        {
            continue;
        }
        let mut candidate = allocation.clone();
        candidate.captures[result].expression = Expr::StringConcat {
            left: Box::new(Expr::Capture(host)),
            right: Box::new(Expr::Capture(path)),
        };
        let mut retained = live.to_vec();
        for index in [second, first, builder] {
            candidate.captures.remove(index);
            retained.remove(index);
            let mut remaining = 0;
            for capture in &mut candidate.captures {
                substitute(&mut capture.expression, index, &Expr::Null, &mut remaining);
            }
            for arg in &mut candidate.arguments {
                substitute(arg, index, &Expr::Null, &mut remaining);
            }
            if remaining != 0 {
                return None;
            }
        }
        let mut normalized = events.to_vec();
        normalized.drain(start..start + private.len());
        return Some((candidate, normalized, retained));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::super::super::allocation::{Capture, Symbol};
    use super::*;
    use crate::native_dex::DexSymbols;
    use std::sync::Arc;
    fn symbol(text: &str, label: &str) -> Symbol {
        Symbol {
            text: text.into(),
            label: label.into(),
        }
    }
    fn call(site: usize, target: Expr, name: &str, label: &str, args: Vec<Expr>) -> Expr {
        Expr::Call {
            site,
            target: Box::new(target),
            method: symbol(name, label),
            args,
        }
    }
    pub(super) fn fixture() -> (DexClass, Allocation, Vec<Event>) {
        let class = DexClass {
            descriptor: "Lsample/Test;".into(),
            superclass: Some("Ljava/lang/Object;".into()),
            interfaces: vec![],
            access_flags: 1,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            methods: vec![],
            symbols: Arc::new(DexSymbols::default()),
        };
        class
            .symbols
            .hierarchy
            .set(Arc::new(
                crate::native_hierarchy::TypeHierarchy::from_classes([&class]).unwrap(),
            ))
            .unwrap();
        let append = "java.lang.StringBuilder.append(Ljava/lang/String;)Ljava/lang/StringBuilder;";
        let captures = vec![
            Capture {
                ty: STRING.into(),
                name: "host".into(),
                expression: call(
                    1,
                    Expr::Local("uri".into()),
                    "getHóst",
                    "sample.Uri.getHost()Ljava/lang/String;",
                    vec![],
                ),
            },
            Capture {
                ty: STRING.into(),
                name: "path".into(),
                expression: call(
                    2,
                    Expr::Local("uri".into()),
                    "getPath",
                    "sample.Uri.getPath()Ljava/lang/String;",
                    vec![],
                ),
            },
            Capture {
                ty: BUILDER.into(),
                name: "builder".into(),
                expression: Expr::SharedNew {
                    allocation: Box::new(Allocation {
                        site: 3,
                        constructor_site: 4,
                        ty: symbol(BUILDER, BUILDER),
                        captures: vec![],
                        arguments: vec![],
                    }),
                    constructor_label: "java.lang.StringBuilder.<init>()V".into(),
                },
            },
            Capture {
                ty: BUILDER.into(),
                name: "first".into(),
                expression: call(
                    5,
                    Expr::Capture(2),
                    "append",
                    append,
                    vec![Expr::Capture(0)],
                ),
            },
            Capture {
                ty: BUILDER.into(),
                name: "second".into(),
                expression: call(
                    6,
                    Expr::Capture(3),
                    "append",
                    append,
                    vec![Expr::Capture(1)],
                ),
            },
            Capture {
                ty: STRING.into(),
                name: "text".into(),
                expression: call(
                    7,
                    Expr::Capture(4),
                    "toString",
                    "java.lang.StringBuilder.toString()Ljava/lang/String;",
                    vec![],
                ),
            },
        ];
        let events = vec![
            Event::Allocate {
                site: 0,
                ty: "sample.Holder".into(),
            },
            Event::Call {
                site: 1,
                method: "sample.Uri.getHost()Ljava/lang/String;".into(),
            },
            Event::Call {
                site: 2,
                method: "sample.Uri.getPath()Ljava/lang/String;".into(),
            },
            Event::Allocate {
                site: 3,
                ty: BUILDER.into(),
            },
            Event::Construct {
                site: 4,
                ty: BUILDER.into(),
            },
            Event::Call {
                site: 5,
                method: append.into(),
            },
            Event::Call {
                site: 6,
                method: append.into(),
            },
            Event::Call {
                site: 7,
                method: "java.lang.StringBuilder.toString()Ljava/lang/String;".into(),
            },
            Event::Construct {
                site: 8,
                ty: "sample.Holder".into(),
            },
        ];
        (
            class,
            Allocation {
                site: 0,
                constructor_site: 8,
                ty: symbol("Holder", "sample.Holder"),
                captures,
                arguments: vec![Expr::Capture(5)],
            },
            events,
        )
    }
    #[test]
    fn typed_concat_keeps_truthful_external_trace_links_and_no_dead_declarations() {
        let (class, a, e) = fixture();
        let r =
            super::super::render_platform_checked(&class, &a, &e, &["uri"], &[false; 6]).unwrap();
        assert_eq!(r.expression, "new Holder((uri.getHóst() + uri.getPath()))");
        assert!(r.declarations.is_empty());
        assert_eq!(
            r.events,
            [e[0].clone(), e[1].clone(), e[2].clone(), e[8].clone()]
        );
        assert!(
            a.render_checked(&r.events, &["uri"]).is_err(),
            "general trace guard must remain exact"
        );
        let chars: Vec<_> = r.expression.chars().collect();
        for link in r.links {
            let token: String = chars[link.start..link.end].iter().collect();
            assert!(
                matches!(token.as_str(), "Holder" | "getHóst" | "getPath"),
                "{token}"
            );
        }
    }
    #[test]
    fn original_effect_reordering_rejects_and_live_string_result_retains_alias() {
        let (class, a, mut e) = fixture();
        e.swap(1, 2);
        assert!(
            super::super::render_platform_checked(&class, &a, &e, &["uri"], &[false; 6]).is_err()
        );
        e.swap(1, 2);
        let mut live = [false; 6];
        live[5] = true;
        let rendered =
            super::super::render_platform_checked(&class, &a, &e, &["uri"], &live).unwrap();
        assert_eq!(rendered.declarations, ["java.lang.String text;"]);
        assert_eq!(
            rendered.expression,
            "new Holder((text = (uri.getHóst() + uri.getPath())))"
        );
        assert_eq!(
            rendered.events,
            [e[0].clone(), e[1].clone(), e[2].clone(), e[8].clone()]
        );
    }
    #[test]
    fn live_aliases_other_overloads_extra_effects_and_shadows_decline() {
        for index in 0..5 {
            let (class, a, e) = fixture();
            let mut live = [false; 6];
            live[index] = true;
            assert!(normalize(&class, &a, &e, &live).is_none());
        }
        for shape in 0..5 {
            let (class, mut a, mut e) = fixture();
            match shape {
                0 => {
                    if let Expr::Call { method, .. } = &mut a.captures[3].expression {
                        method.label="java.lang.StringBuilder.append(Ljava/lang/Object;)Ljava/lang/StringBuilder;".into()
                    }
                }
                1 => a.arguments.push(Expr::Capture(2)),
                2 => e.insert(
                    5,
                    Event::Call {
                        site: 20,
                        method: "sample.Hook.effect()V".into(),
                    },
                ),
                3 => a.captures[0].ty = "java.lang.Object".into(),
                4 => {
                    if let Expr::SharedNew { allocation, .. } = &mut a.captures[2].expression {
                        allocation.arguments.push(Expr::Null)
                    }
                }
                _ => unreachable!(),
            };
            assert!(
                normalize(&class, &a, &e, &[false; 6]).is_none(),
                "shape {shape}"
            );
        }
        for descriptor in ["Ljava/lang/StringBuilder;", "Ljava/lang/String;"] {
            let (mut class, a, e) = fixture();
            let shadow = DexClass {
                descriptor: descriptor.into(),
                superclass: Some("Ljava/lang/Object;".into()),
                interfaces: vec![],
                access_flags: 1,
                annotations_offset: 0,
                static_values_offset: 0,
                static_values: vec![],
                fields: vec![],
                methods: vec![],
                symbols: Arc::new(DexSymbols::default()),
            };
            let hierarchy =
                crate::native_hierarchy::TypeHierarchy::from_classes([&class, &shadow]).unwrap();
            class.symbols = Arc::new(DexSymbols::default());
            class.symbols.hierarchy.set(Arc::new(hierarchy)).unwrap();
            assert!(normalize(&class, &a, &e, &[false; 6]).is_none());
        }
    }
}

#[cfg(test)]
mod jvm_tests {
    #[test]
    #[ignore = "requires javac and java"]
    fn jvm_concat_content_nulls_application_effects_and_identity_contract() {
        use std::{fs, process::Command};
        let (class, a, e) = super::tests::fixture();
        let r =
            super::super::render_platform_checked(&class, &a, &e, &["uri"], &[false; 6]).unwrap();
        let dir = std::env::temp_dir().join(format!("rdx-capture-concat-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let source = format!(
            r#"public class Check {{
static String trace;static int fail;static RuntimeException marker=new RuntimeException();
static class Uri {{String host,path;Uri(String h,String p){{host=h;path=p;}}String getHóst(){{trace+="H";if(fail==1)throw marker;return host;}}String getPath(){{trace+="P";if(fail==2)throw marker;return path;}}}}
static class Holder {{String value;Holder(String s){{trace+="C";if(fail==3)throw marker;value=s;}}}}
static Holder emitted(Uri uri){{return {};}}
static Holder original(Uri uri){{String host,path;StringBuilder builder;return new Holder(((host=uri.getHóst())==null?((path=uri.getPath())==null?(builder=new StringBuilder()):(builder=new StringBuilder())):((path=uri.getPath())==null?(builder=new StringBuilder()):(builder=new StringBuilder()))).append(host).append(path).toString());}}
static String outcome(boolean emitted,Uri uri){{trace="";try{{Holder result=emitted?emitted(uri):original(uri);return trace+":"+result.value;}}catch(RuntimeException ex){{if(ex!=marker)throw new AssertionError(ex);return trace+":throw";}}}}
public static void main(String[]args){{int checks=0;String[]values={{null,"","host","π😀"}};for(String host:values)for(String path:values)for(fail=0;fail<=3;fail++){{Uri uri=new Uri(host,path);String before=outcome(false,uri),after=outcome(true,uri);if(!before.equals(after))throw new AssertionError(before+" != "+after);checks++;}}
fail=0;Uri empty=new Uri("","");trace="";if(original(empty).value!=""||emitted(empty).value=="")throw new AssertionError("empty identity contract");
Uri nonempty=new Uri("host","");trace="";if(original(nonempty).value==original(nonempty).value||emitted(nonempty).value==emitted(nonempty).value)throw new AssertionError("nonempty freshness");System.out.print(checks);}}
}}"#,
            r.expression
        );
        fs::write(dir.join("Check.java"), source).unwrap();
        let compiled = Command::new("javac")
            .arg(dir.join("Check.java"))
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
            .arg("Check")
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "{}",
            String::from_utf8_lossy(&run.stderr)
        );
        assert_eq!(run.stdout, b"64");
        fs::remove_dir_all(dir).unwrap();
    }
}
