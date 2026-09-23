//! Declaration-based implementation lookup; no source-text matching or guessed dispatch.
use super::*;
use crate::native_dex::DexMethod;
use crate::native_hierarchy::Relation;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

fn signature(method: &DexMethod) -> String {
    format!(
        "{}({}){}",
        method.name,
        method.parameters.join(""),
        method.return_type
    )
}
fn package(descriptor: &str) -> &str {
    descriptor
        .rsplit_once('/')
        .map_or("", |(package, _)| package)
}

impl NativeDexEngine {
    pub fn implementations(
        &self,
        owner: &str,
        member: Option<&str>,
        cancel: &AtomicBool,
    ) -> Result<Vec<String>> {
        let descriptor = format!("L{};", owner.replace('.', "/"));
        let root = self.class(owner);
        let target = if let Some(member) = member {
            let method = root
                .and_then(|class| class.methods.iter().find(|m| signature(m) == member))
                .context("Method declaration unavailable; cannot establish overriding rules")?;
            ensure!(
                !method.name.starts_with('<') && method.access_flags & (2 | 8 | 16) == 0,
                "Constructors, private, static and final methods cannot be overridden"
            );
            Some(method)
        } else {
            None
        };
        let mut reverse: HashMap<&str, Vec<&str>> = HashMap::new();
        let mut by_descriptor = HashMap::new();
        let mut work = 0usize;
        for (name, class) in &self.classes {
            ensure!(
                !cancel.load(Ordering::Relaxed),
                "Implementation search cancelled"
            );
            by_descriptor.insert(class.descriptor.as_ref(), (name.as_str(), class));
            for parent in class.superclass.iter().chain(class.interfaces.iter()) {
                work += 1;
                ensure!(
                    work <= 4_000_000,
                    "Implementation hierarchy exceeds edge budget"
                );
                reverse
                    .entry(parent.as_ref())
                    .or_default()
                    .push(class.descriptor.as_ref());
            }
        }
        let mut pending = vec![descriptor.as_str()];
        let mut descendants = HashSet::new();
        while let Some(parent) = pending.pop() {
            ensure!(
                !cancel.load(Ordering::Relaxed),
                "Implementation search cancelled"
            );
            if !descendants.insert(parent) {
                continue;
            }
            if let Some(children) = reverse.get(parent) {
                pending.extend(children);
            }
        }
        descendants.remove(descriptor.as_str());
        let mut results = BTreeSet::new();
        let interface = root.is_some_and(|class| class.access_flags & 0x200 != 0);
        let compatible = |method: &DexMethod, target: &DexMethod| {
            method.name == target.name
                && method.parameters == target.parameters
                && (method.return_type == target.return_type
                    || root
                        .and_then(|c| c.symbols.hierarchy.get())
                        .is_some_and(|h| {
                            h.assignable(&method.return_type, &target.return_type)
                                == Relation::Proven
                        }))
        };
        if interface
            && let Some(target) = target
            && target.access_flags & 0x400 == 0
        {
            results.insert(format!("{owner}.{}", signature(target)));
        }
        for child in descendants {
            let Some(&(name, class)) = by_descriptor.get(child) else {
                continue;
            };
            ensure!(
                !cancel.load(Ordering::Relaxed),
                "Implementation search cancelled"
            );
            let Some(target) = target else {
                if class.access_flags & 0x600 == 0 {
                    results.insert(name.to_owned());
                }
                continue;
            };
            // Package-private methods must remain inherited along the superclass path.
            if target.access_flags & (1 | 4) == 0 {
                let mut current = Some(class);
                let mut seen = HashSet::new();
                let mut inherited = false;
                while let Some(node) = current {
                    work += 1;
                    ensure!(
                        work <= 4_000_000,
                        "Implementation lookup exceeds work budget"
                    );
                    if !seen.insert(node.descriptor.as_ref())
                        || package(&node.descriptor) != package(&descriptor)
                    {
                        break;
                    }
                    if node.descriptor.as_ref() == descriptor {
                        inherited = true;
                        break;
                    }
                    current = node
                        .superclass
                        .as_deref()
                        .and_then(|p| by_descriptor.get(p).map(|(_, c)| *c));
                }
                if !inherited {
                    continue;
                }
            }
            let mut current = Some((name, class));
            let mut seen = HashSet::new();
            while let Some((declaring_name, node)) = current {
                work += node.methods.len() + 1;
                ensure!(
                    work <= 4_000_000,
                    "Implementation lookup exceeds work budget"
                );
                if !seen.insert(node.descriptor.as_ref()) {
                    break;
                }
                let matches: Vec<_> = node
                    .methods
                    .iter()
                    .filter(|m| compatible(m, target) && m.access_flags & (2 | 8) == 0)
                    .collect();
                if !matches.is_empty() {
                    for method in matches {
                        if method.access_flags & 0x400 == 0
                            && (!interface || method.access_flags & 1 != 0)
                            && declaring_name != owner
                        {
                            results.insert(format!("{declaring_name}.{}", signature(method)));
                        }
                    }
                    break;
                }
                // Interface implementers may inherit a concrete method from a base
                // class that does not itself declare the interface. Deduplicate it.
                if !interface || class.access_flags & 0x600 != 0 {
                    break;
                }
                current = node
                    .superclass
                    .as_deref()
                    .and_then(|p| by_descriptor.get(p).copied());
            }
        }
        Ok(results.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    fn fixture() -> NativeDexEngine {
        let mut engine = NativeDexEngine::default();
        for (name, parent, interfaces, flags, methods) in [
            (
                "I",
                "java.lang.Object",
                vec![],
                0x601,
                vec![("run", "LValue;", false, 0x401)],
            ),
            ("SubI", "java.lang.Object", vec!["I"], 0x601, vec![]),
            (
                "DefaultI",
                "java.lang.Object",
                vec!["I"],
                0x601,
                vec![("run", "LValue;", false, 1)],
            ),
            (
                "Impl",
                "java.lang.Object",
                vec!["SubI"],
                1,
                vec![("run", "LSubValue;", false, 1), ("run", "LValue;", true, 1)],
            ),
            ("Grand", "Impl", vec![], 1, vec![]),
            (
                "Base",
                "java.lang.Object",
                vec![],
                1,
                vec![("run", "LValue;", false, 1)],
            ),
            ("Inherited", "Base", vec!["I"], 1, vec![]),
            (
                "Abstract",
                "java.lang.Object",
                vec!["I"],
                0x401,
                vec![("run", "LValue;", false, 0x401)],
            ),
            (
                "Static",
                "java.lang.Object",
                vec!["I"],
                1,
                vec![("run", "LValue;", false, 9)],
            ),
            (
                "Private",
                "java.lang.Object",
                vec!["I"],
                1,
                vec![("run", "LValue;", false, 2)],
            ),
            (
                "Unrelated",
                "java.lang.Object",
                vec![],
                1,
                vec![("run", "LValue;", false, 1)],
            ),
            ("Value", "java.lang.Object", vec![], 1, vec![]),
            ("SubValue", "Value", vec![], 1, vec![]),
            (
                "p.Parent",
                "java.lang.Object",
                vec![],
                1,
                vec![("run", "LValue;", false, 0), ("fixed", "V", false, 17)],
            ),
            (
                "p.Child",
                "p.Parent",
                vec![],
                1,
                vec![("run", "LValue;", false, 1)],
            ),
            (
                "q.Child",
                "p.Parent",
                vec![],
                1,
                vec![("run", "LValue;", false, 1)],
            ),
        ] {
            let mut class = native_dex::parse(include_bytes!("../tests/fixtures/hello.dex"))
                .unwrap()
                .classes
                .remove(0);
            let descriptor: Arc<str> = format!("L{};", name.replace('.', "/")).into();
            class.descriptor = descriptor.clone();
            class.superclass = Some(format!("L{};", parent.replace('.', "/")).into());
            class.interfaces = interfaces.iter().map(|i| format!("L{i};").into()).collect();
            class.access_flags = flags;
            class.methods = methods
                .iter()
                .map(|&(name, ret, arg, flags)| DexMethod {
                    declaring_type: descriptor.clone(),
                    name: name.into(),
                    return_type: ret.into(),
                    parameters: if arg { vec!["I".into()] } else { vec![] },
                    thrown_types: vec![],
                    access_flags: flags,
                    code: None,
                })
                .collect();
            engine.classes.insert(name.into(), class);
        }
        let hierarchy = Arc::new(
            crate::native_hierarchy::TypeHierarchy::from_classes(engine.classes.values()).unwrap(),
        );
        for class in engine.classes.values() {
            class.symbols.hierarchy.set(hierarchy.clone()).unwrap();
        }
        engine
    }
    #[test]
    fn interface_implementations_include_indirect_concrete_types() {
        let engine = fixture();
        let names = engine
            .implementations("I", None, &AtomicBool::new(false))
            .unwrap();
        assert!(
            names.contains(&"Impl".into())
                && names.contains(&"Grand".into())
                && names.contains(&"Inherited".into())
        );
        for excluded in ["I", "SubI", "DefaultI", "Abstract", "Unrelated", "Base"] {
            assert!(!names.contains(&excluded.into()), "{excluded}");
        }
        assert!(
            engine
                .implementations("Missing", None, &AtomicBool::new(false))
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn method_implementations_preserve_signatures_covariance_and_inherited_bodies() {
        let engine = fixture();
        assert_eq!(
            engine
                .implementations("I", Some("run()LValue;"), &AtomicBool::new(false))
                .unwrap(),
            [
                "Base.run()LValue;",
                "DefaultI.run()LValue;",
                "Impl.run()LSubValue;"
            ]
        );
        assert_eq!(
            engine
                .implementations("p.Parent", Some("run()LValue;"), &AtomicBool::new(false))
                .unwrap(),
            ["p.Child.run()LValue;"]
        );
        assert!(
            engine
                .implementations("p.Parent", Some("fixed()V"), &AtomicBool::new(false))
                .is_err()
        );
        assert!(
            engine
                .implementations("Static", Some("run()LValue;"), &AtomicBool::new(false))
                .is_err()
        );
        assert!(
            engine
                .implementations("I", None, &AtomicBool::new(true))
                .is_err()
        );
    }
}
