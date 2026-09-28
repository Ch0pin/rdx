//! Effect-aware shrinking of typed allocation captures before Java emission.
//! The existing allocation renderer independently verifies the complete DEX
//! effect trace after every attempted substitution.
use super::allocation::{Allocation, Event, Expr, Rendered};
use anyhow::Result;

const MAX_CAPTURE_REWRITES: usize = 16;

fn exact_type(expr: &Expr) -> Option<String> {
    match expr {
        Expr::StringConstant { .. } => Some("java.lang.String".into()),
        Expr::ClassConstant { .. } => Some("java.lang.Class".into()),
        Expr::Cast { ty, .. } | Expr::CheckCast { ty, .. } => Some(ty.text.clone()),
        Expr::Call { method, .. } => method
            .label
            .rsplit_once(')')
            .and_then(|(_, result)| super::java_type(result).ok()),
        Expr::New(allocation) | Expr::SharedNew { allocation, .. } => {
            Some(allocation.ty.text.clone())
        }
        _ => None,
    }
}

fn substitute(expr: &mut Expr, index: usize, replacement: &Expr, uses: &mut usize) {
    match expr {
        Expr::Capture(candidate) if *candidate == index => {
            *uses += 1;
            *expr = replacement.clone();
        }
        Expr::Capture(candidate) if *candidate > index => *candidate -= 1,
        Expr::Cast { value, .. } | Expr::CheckCast { value, .. } | Expr::Unary { value, .. } => {
            substitute(value, index, replacement, uses)
        }
        Expr::Binary { left, right, .. }
        | Expr::ArrayRead {
            array: left,
            index: right,
            ..
        }
        | Expr::FieldStore {
            receiver: left,
            value: right,
            ..
        } => {
            substitute(left, index, replacement, uses);
            substitute(right, index, replacement, uses);
        }
        Expr::ArrayStore {
            array,
            index: offset,
            value,
            ..
        } => {
            substitute(array, index, replacement, uses);
            substitute(offset, index, replacement, uses);
            substitute(value, index, replacement, uses);
        }
        Expr::FieldRead { receiver, .. } => substitute(receiver, index, replacement, uses),
        Expr::Call { target, args, .. } => {
            substitute(target, index, replacement, uses);
            for argument in args {
                substitute(argument, index, replacement, uses);
            }
        }
        Expr::FilledArray { elements, .. } => {
            for element in elements {
                substitute(element, index, replacement, uses);
            }
        }
        Expr::NewArray { length, .. } => substitute(length, index, replacement, uses),
        Expr::SharedNew { allocation, .. } if allocation.captures.is_empty() => {
            for argument in &mut allocation.arguments {
                substitute(argument, index, replacement, uses);
            }
        }
        // Private nested allocations own their capture indices.
        Expr::New(_) | Expr::SharedNew { .. } => {}
        _ => {}
    }
}

fn once(allocation: &Allocation, index: usize) -> Option<Allocation> {
    let capture = allocation.captures.get(index)?;
    if exact_type(&capture.expression).as_deref() != Some(capture.ty.as_str()) {
        return None;
    }
    let mut next = allocation.clone();
    let expression = next.captures[index].expression.clone();
    let mut uses = 0;
    for capture in next.captures.iter_mut().skip(index + 1) {
        substitute(&mut capture.expression, index, &expression, &mut uses);
    }
    for argument in &mut next.arguments {
        substitute(argument, index, &expression, &mut uses);
    }
    if uses != 1 {
        return None;
    }
    next.captures.remove(index);
    Some(next)
}

pub(super) fn render_checked(
    allocation: &Allocation,
    events: &[Event],
    locals: &[&str],
    live_after: &[bool],
) -> Result<Rendered> {
    let baseline = allocation.render_checked(events, locals)?;
    if allocation.captures.is_empty()
        || allocation.captures.len() > MAX_CAPTURE_REWRITES
        || live_after.len() != allocation.captures.len()
    {
        return Ok(baseline);
    }
    let mut current = allocation.clone();
    let mut retained = live_after.to_vec();
    let mut rendered = baseline;
    for index in (0..allocation.captures.len()).rev() {
        if index >= current.captures.len() {
            continue;
        }
        if retained[index] {
            continue;
        }
        let Some(candidate) = once(&current, index) else {
            continue;
        };
        if let Ok(next) = candidate.render_checked(events, locals) {
            current = candidate;
            retained.remove(index);
            rendered = next;
        }
    }
    Ok(rendered)
}

#[cfg(test)]
mod tests {
    use super::super::allocation::{Capture, Symbol};
    use super::*;

    fn symbol(text: &str, label: &str) -> Symbol {
        Symbol {
            text: text.into(),
            label: label.into(),
        }
    }

    fn fixture() -> (Allocation, Vec<Event>) {
        let method = "sample.Hook.make(Ljava/lang/Class;)Ljava/lang/Object;";
        (
            Allocation {
                site: 1,
                constructor_site: 5,
                ty: symbol("Holder", "sample.Holder"),
                captures: vec![
                    Capture {
                        ty: "java.lang.Class".into(),
                        name: "classValue".into(),
                        expression: Expr::ClassConstant {
                            site: 2,
                            ty: symbol("Target", "sample.Target"),
                        },
                    },
                    Capture {
                        ty: "java.lang.Object".into(),
                        name: "value".into(),
                        expression: Expr::Call {
                            site: 3,
                            target: Box::new(Expr::Local("Hook".into())),
                            method: symbol("make", method),
                            args: vec![Expr::Capture(0)],
                        },
                    },
                ],
                arguments: vec![Expr::Capture(1)],
            },
            vec![
                Event::Allocate {
                    site: 1,
                    ty: "sample.Holder".into(),
                },
                Event::ClassResolution {
                    site: 2,
                    ty: "sample.Target".into(),
                },
                Event::Call {
                    site: 3,
                    method: method.into(),
                },
                Event::Construct {
                    site: 5,
                    ty: "sample.Holder".into(),
                },
            ],
        )
    }

    #[test]
    fn single_use_captures_inline_with_exact_events_and_navigation() {
        let (allocation, events) = fixture();
        let baseline = allocation.render_checked(&events, &["Hook"]).unwrap();
        let result = render_checked(&allocation, &events, &["Hook"], &[false, false]).unwrap();
        assert_eq!(
            baseline.declarations,
            ["java.lang.Class classValue;", "java.lang.Object value;"]
        );
        assert!(baseline.expression.contains("classValue = Target.class"));
        assert!(baseline.expression.contains("value = Hook.make"));
        assert!(result.declarations.is_empty(), "{result:?}");
        assert_eq!(result.expression, "new Holder(Hook.make(Target.class))");
        assert_eq!(result.events, events);
        for (spelling, label) in [
            ("Holder", "sample.Holder"),
            (
                "make",
                "sample.Hook.make(Ljava/lang/Class;)Ljava/lang/Object;",
            ),
            ("Target", "sample.Target"),
        ] {
            let link = result
                .links
                .iter()
                .find(|link| link.label == label)
                .unwrap();
            assert_eq!(
                result
                    .expression
                    .chars()
                    .skip(link.start)
                    .take(link.end - link.start)
                    .collect::<String>(),
                spelling
            );
        }
    }

    #[test]
    fn repeated_use_and_mismatched_static_type_keep_assignment() {
        let (mut allocation, events) = fixture();
        allocation.arguments.push(Expr::Capture(1));
        let result = render_checked(&allocation, &events, &["Hook"], &[false, false]).unwrap();
        assert_eq!(result.declarations, ["java.lang.Object value;"]);
        assert_eq!(
            result.expression,
            "new Holder((value = Hook.make(Target.class)), value)"
        );

        let (mut allocation, events) = fixture();
        if let Expr::Call { args, .. } = &mut allocation.captures[1].expression {
            args.push(Expr::Capture(0));
        }
        let result = render_checked(&allocation, &events, &["Hook"], &[false, false]).unwrap();
        assert_eq!(result.declarations, ["java.lang.Class classValue;"]);
        assert!(result.expression.contains("classValue = Target.class"));

        let (mut allocation, events) = fixture();
        allocation.captures[1].ty = "java.lang.String".into();
        let result = render_checked(&allocation, &events, &["Hook"], &[false, false]).unwrap();
        assert_eq!(result.declarations, ["java.lang.String value;"]);
        assert!(result.expression.contains("value = Hook.make"));
    }

    #[test]
    fn live_capture_keeps_original_name_and_later_capture_can_still_inline() {
        let (allocation, events) = fixture();
        let result = render_checked(&allocation, &events, &["Hook"], &[true, false]).unwrap();
        assert_eq!(result.declarations, ["java.lang.Class classValue;"]);
        assert_eq!(
            result.expression,
            "new Holder(Hook.make((classValue = Target.class)))"
        );
        let baseline = allocation.render_checked(&events, &["Hook"]).unwrap();
        assert_eq!(
            render_checked(&allocation, &events, &["Hook"], &[true, true]).unwrap(),
            baseline
        );
        assert_eq!(
            render_checked(&allocation, &events, &["Hook"], &[]).unwrap(),
            baseline
        );
    }

    #[test]
    fn poisoned_effect_trace_and_unbound_local_are_rejected() {
        let (allocation, mut events) = fixture();
        events.swap(1, 2);
        assert!(render_checked(&allocation, &events, &["Hook"], &[false, false]).is_err());
        let (_, events) = fixture();
        assert!(render_checked(&allocation, &events, &[], &[false, false]).is_err());
    }

    #[test]
    #[ignore = "requires javac and java"]
    fn jvm_preserves_allocation_order_overloads_and_exception_identity() {
        use std::{fs, process::Command};
        let (allocation, events) = fixture();
        let baseline = allocation.render_checked(&events, &["Hook"]).unwrap();
        let reduced = render_checked(&allocation, &events, &["Hook"], &[false, false]).unwrap();
        let dir = std::env::temp_dir().join(format!("rdx-capture-shrink-{}", std::process::id()));
        fs::create_dir_all(dir.join("sample")).unwrap();
        let java = format!(
            r#"package sample;
public class Test {{
  static Holder before() {{ {} return {}; }}
  static Holder after() {{ {} return {}; }}
  static void check(boolean ok) {{ if (!ok) throw new AssertionError(Hook.trace); }}
  static void run(boolean fail) {{
    Hook.trace = ""; Hook.fail = fail;
    Object first = null;
    try {{ first = before().value; }} catch (RuntimeException ex) {{ check(ex == Hook.sentinel); }}
    String a = Hook.trace;
    Hook.trace = "";
    Object second = null;
    try {{ second = after().value; }} catch (RuntimeException ex) {{ check(ex == Hook.sentinel); }}
    check(a.equals(Hook.trace) && (fail || (first == Target.class && second == Target.class)));
  }}
  public static void main(String[] args) {{ run(false); run(true); System.out.print("ok"); }}
}}
class Target {{}}
class Holder {{ final Object value; Holder(Object x) {{ Hook.trace += "C"; value = x; }} }}
class Hook {{
  static String trace = ""; static boolean fail;
  static final RuntimeException sentinel = new RuntimeException("sentinel");
  static Object make(Class<?> type) {{ trace += "M"; if (fail) throw sentinel; return type; }}
  static Object make(Object type) {{ throw new AssertionError("wrong overload"); }}
}}
"#,
            baseline.declarations.join(" "),
            baseline.expression,
            reduced.declarations.join(" "),
            reduced.expression
        );
        fs::write(dir.join("sample/Test.java"), java).unwrap();
        let compiled = Command::new("javac")
            .arg(dir.join("sample/Test.java"))
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
            .arg("sample.Test")
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "{}",
            String::from_utf8_lossy(&run.stderr)
        );
        assert_eq!(run.stdout, b"ok");
        fs::remove_dir_all(dir).unwrap();
    }
}
