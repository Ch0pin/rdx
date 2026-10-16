//! Complete loaded inheritance is required before argument types may change.
use super::{OBJECT, TypeHierarchy};
use std::collections::HashSet;

impl TypeHierarchy {
    pub(crate) fn cast_cleanup_call(
        &self,
        owner: &str,
        name: &str,
        args: &[String],
        ret: &str,
        is_static: bool,
    ) -> bool {
        let Some(root) = self.loaded_call_families.get(owner) else {
            return false;
        };
        let Some(method) = root
            .methods
            .get(name)
            .and_then(|family| family.get(&args.len()))
            .and_then(Option::as_ref)
        else {
            return false;
        };
        if !root.public
            || root.generic_class
            || self.ambiguous.contains(owner)
            || method.access_flags & 1 == 0
            || method.access_flags & (0x40 | 0x80 | 0x1000) != 0
            || (method.access_flags & 8 != 0) != is_static
            || method.annotated
            || method.return_type.as_ref() != ret
            || !method
                .parameters
                .iter()
                .map(|x| x.as_ref())
                .eq(args.iter().map(String::as_str))
            || root.varargs_names.contains(name)
        {
            return false;
        }
        // Constructors do not inherit overloads. Generic owners remain excluded.
        if name == "<init>" {
            return !is_static && ret == "V";
        }
        let mut pending = vec![owner];
        let mut seen = HashSet::new();
        let mut edges = 0;
        while let Some(current) = pending.pop() {
            if !seen.insert(current) {
                continue;
            }
            if seen.len() > 256 || self.ambiguous.contains(current) {
                return false;
            }
            if current == OBJECT && self.unshadowed_sdk_type(OBJECT) {
                if matches!(
                    name,
                    "clone"
                        | "equals"
                        | "finalize"
                        | "getClass"
                        | "hashCode"
                        | "notify"
                        | "notifyAll"
                        | "toString"
                        | "wait"
                ) {
                    return false;
                }
                continue;
            }
            // These two platform interfaces declare no methods.
            if matches!(current, "Ljava/io/Serializable;" | "Ljava/lang/Cloneable;")
                && self.unshadowed_sdk_type(current)
            {
                continue;
            }
            let (Some(entry), Some(summary)) = (
                self.entries.get(current),
                self.loaded_call_families.get(current),
            ) else {
                return false;
            };
            if summary.varargs_names.contains(name) {
                return false;
            }
            // Even an identical inherited signature is conservatively retained:
            // generic substitutions and covariant declarations need a richer proof.
            if current != owner
                && summary
                    .methods
                    .get(name)
                    .is_some_and(|family| family.contains_key(&args.len()))
            {
                return false;
            }
            edges += entry.interfaces.len() + usize::from(entry.superclass.is_some());
            if edges > 1024 {
                return false;
            }
            pending.extend(entry.superclass.iter().map(|s| s.as_ref()));
            pending.extend(entry.interfaces.iter().map(|s| s.as_ref()));
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unloaded_call_families_retain_argument_casts() {
        let hierarchy = TypeHierarchy::from_classes([]).unwrap();
        for owner in ["Lsample/Unknown;", "Ljava/lang/Object;"] {
            assert!(!hierarchy.cast_cleanup_call(
                owner,
                "accept",
                &["Ljava/lang/Object;".into()],
                "V",
                true,
            ));
        }
    }
}
