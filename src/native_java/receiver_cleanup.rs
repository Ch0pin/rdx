//! Remove only provably redundant casts on SDK Number conversion receivers.

use super::Value;
use crate::native_hierarchy::{Relation, TypeHierarchy};
use std::sync::Arc;

pub(super) fn sdk_number_conversion_receiver(
    value: &Value,
    owner: &str,
    name: &str,
    args: &[Arc<str>],
    ret: &str,
    opcode: u8,
    hierarchy: Option<&TypeHierarchy>,
) -> bool {
    if !matches!(opcode, 0x6e | 0x74)
        || owner != "Ljava/lang/Number;"
        || !args.is_empty()
        || value.literal.is_some()
        || value.wide_literal.is_some()
        || value.raw_bits32
        || !super::super::identifier(&value.text)
    {
        return false;
    }
    let exact_method = matches!(
        (name, ret),
        ("byteValue", "B")
            | ("shortValue", "S")
            | ("intValue", "I")
            | ("longValue", "J")
            | ("floatValue", "F")
            | ("doubleValue", "D")
    );
    exact_method
        && hierarchy.is_some_and(|hierarchy| hierarchy.sdk_numeric_wrapper_to_number(&value.ty))
}

/// A no-argument void virtual call has no parameter overload or covariant
/// return selection to change when its receiver's guaranteed upcast is removed.
pub(super) fn proven_void_receiver(
    value: &Value,
    owner: &str,
    name: &str,
    args: &[Arc<str>],
    ret: &str,
    opcode: u8,
    hierarchy: Option<&TypeHierarchy>,
) -> bool {
    let Some(hierarchy) = hierarchy else {
        return false;
    };
    if !matches!(opcode, 0x6e | 0x74)
        || !args.is_empty()
        || ret != "V"
        || value.ty == owner
        || !super::reference(&value.ty)
        || !super::reference(owner)
        || value.literal.is_some()
        || value.wide_literal.is_some()
        || value.raw_bits32
        || !(value.text == "this" || super::super::identifier(&value.text))
        || !hierarchy.noarg_void_upcast_path(&value.ty, owner, name)
    {
        return false;
    }
    // Android API 35's extracted Activity.class declares public finish()V.
    // Loaded APK definitions must not replace that SDK declaration.
    let pinned_activity_finish = owner == "Landroid/app/Activity;"
        && name == "finish"
        && hierarchy.unshadowed_sdk_type(owner);
    pinned_activity_finish || hierarchy.unambiguous_loaded_call(owner, name, args, ret, opcode)
}

/// Drop a reference upcast or typed-null cast only when one loaded declaration
/// can be selected by this Java name and arity. This deliberately leaves SDK
/// overload families, generic annotations, varargs, and effectful expressions.
#[allow(clippy::too_many_arguments)]
pub(super) fn proven_call_argument(
    value: &Value,
    owner: &str,
    name: &str,
    args: &[Arc<str>],
    ret: &str,
    opcode: u8,
    argument_index: usize,
    hierarchy: Option<&TypeHierarchy>,
) -> bool {
    let Some(hierarchy) = hierarchy else {
        return false;
    };
    let Some(target) = args.get(argument_index) else {
        return false;
    };
    if !super::reference(target)
        || !hierarchy.unambiguous_loaded_argument_call(owner, name, args, ret, opcode)
    {
        return false;
    }
    if value.literal == Some(0) {
        return value.wide_literal.is_none() && !value.raw_bits32;
    }
    value.literal.is_none()
        && value.wide_literal.is_none()
        && !value.raw_bits32
        && super::reference(&value.ty)
        && value.ty != target.as_ref()
        && (value.text == "this" || super::super::identifier(&value.text))
        && hierarchy.assignable(&value.ty, target) == Relation::Proven
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_dex::{
        DexAnnotation, DexAnnotationDirectory, DexClass, DexMethod, DexSymbols,
    };

    fn value(text: &str, ty: &str) -> Value {
        Value {
            text: text.into(),
            ty: ty.into(),
            literal: None,
            wide_literal: None,
            raw_bits32: false,
        }
    }

    #[test]
    fn exact_number_conversion_contract_only() {
        let hierarchy = TypeHierarchy::from_classes(std::iter::empty::<&DexClass>()).unwrap();
        let good = |value: &Value, owner: &str, name: &str, args: &[Arc<str>], ret: &str, op| {
            sdk_number_conversion_receiver(value, owner, name, args, ret, op, Some(&hierarchy))
        };
        let integer = value("integer", "Ljava/lang/Integer;");
        assert!(good(
            &integer,
            "Ljava/lang/Number;",
            "intValue",
            &[],
            "I",
            0x6e
        ));
        assert!(good(
            &integer,
            "Ljava/lang/Number;",
            "intValue",
            &[],
            "I",
            0x74
        ));
        assert!(!good(
            &integer,
            "Ljava/lang/Number;",
            "intValue",
            &["I".into()],
            "I",
            0x6e
        ));
        assert!(!good(
            &integer,
            "Ljava/lang/Number;",
            "intValue",
            &[],
            "J",
            0x6e
        ));
        assert!(!good(
            &integer,
            "Ljava/lang/Number;",
            "toString",
            &[],
            "Ljava/lang/String;",
            0x6e
        ));
        assert!(!good(
            &integer,
            "Ljava/lang/Number;",
            "intValue",
            &[],
            "I",
            0x72
        ));
        assert!(!good(
            &integer,
            "Ljava/lang/Number;",
            "intValue",
            &[],
            "I",
            0x70
        ));
        assert!(!good(
            &integer,
            "Ljava/lang/Number;",
            "intValue",
            &[],
            "I",
            0x71
        ));
        assert!(!good(
            &integer,
            "Ljava/lang/Integer;",
            "intValue",
            &[],
            "I",
            0x6e
        ));
        assert!(!good(
            &value("object", "Ljava/lang/Object;"),
            "Ljava/lang/Number;",
            "intValue",
            &[],
            "I",
            0x6e
        ));
        assert!(!good(
            &value("call()", "Ljava/lang/Integer;"),
            "Ljava/lang/Number;",
            "intValue",
            &[],
            "I",
            0x6e
        ));
        assert!(!good(
            &value("this.field", "Ljava/lang/Integer;"),
            "Ljava/lang/Number;",
            "intValue",
            &[],
            "I",
            0x6e
        ));
        assert!(!sdk_number_conversion_receiver(
            &integer,
            "Ljava/lang/Number;",
            "intValue",
            &[],
            "I",
            0x6e,
            None
        ));
    }

    fn method(name: &str, parameters: &[&str], ret: &str, flags: u32) -> DexMethod {
        DexMethod {
            declaring_type: "Lsample/Owner;".into(),
            name: name.into(),
            return_type: ret.into(),
            parameters: parameters.iter().map(|ty| Arc::from(*ty)).collect(),
            thrown_types: vec![],
            access_flags: flags,
            code: None,
        }
    }

    fn class(descriptor: &str, parent: &str, mut methods: Vec<DexMethod>) -> DexClass {
        for method in &mut methods {
            method.declaring_type = descriptor.into();
        }
        DexClass {
            symbols: Arc::new(DexSymbols::default()),
            descriptor: descriptor.into(),
            superclass: Some(parent.into()),
            interfaces: vec![],
            access_flags: 1,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            methods,
        }
    }

    #[test]
    fn unique_loaded_call_allows_proven_upcast_and_null_only() {
        let mut target = class(
            "Lsample/Target;",
            "Ljava/lang/Object;",
            vec![method("accept", &["Lsample/Base;", "I"], "V", 9)],
        );
        target.annotations_offset = 1;
        Arc::get_mut(&mut target.symbols)
            .unwrap()
            .annotations
            .insert(1, Arc::new(DexAnnotationDirectory::default()));
        let base = class("Lsample/Base;", "Ljava/lang/Object;", vec![]);
        let sub = class("Lsample/Sub;", "Lsample/Base;", vec![]);
        let hierarchy = TypeHierarchy::from_classes([&target, &base, &sub]).unwrap();
        let args = [Arc::from("Lsample/Base;"), Arc::from("I")];
        let check = |value: &Value| {
            proven_call_argument(
                value,
                "Lsample/Target;",
                "accept",
                &args,
                "V",
                0x71,
                0,
                Some(&hierarchy),
            )
        };
        assert!(check(&value("sub", "Lsample/Sub;")));
        assert!(check(&value("this", "Lsample/Sub;")));
        assert!(check(&Value {
            text: "0".into(),
            ty: "I".into(),
            literal: Some(0),
            wide_literal: None,
            raw_bits32: false,
        }));
        assert!(!check(&value("base", "Ljava/lang/Object;")));
        assert!(!check(&value("getSub()", "Lsample/Sub;")));
        assert!(!proven_call_argument(
            &value("sub", "Lsample/Sub;"),
            "Lsample/Target;",
            "accept",
            &args,
            "V",
            0x72,
            0,
            Some(&hierarchy),
        ));
    }

    #[test]
    fn overload_varargs_and_inherited_static_methods_block_argument_cleanup() {
        let base = class("Lsample/Base;", "Ljava/lang/Object;", vec![]);
        let sub = class("Lsample/Sub;", "Lsample/Base;", vec![]);
        let args = [Arc::from("Lsample/Base;"), Arc::from("I")];
        let value = value("sub", "Lsample/Sub;");
        for target in [
            class(
                "Lsample/Target;",
                "Ljava/lang/Object;",
                vec![
                    method("accept", &["Lsample/Base;", "I"], "V", 9),
                    method("accept", &["Lsample/Sub;", "I"], "V", 9),
                ],
            ),
            class(
                "Lsample/Target;",
                "Ljava/lang/Object;",
                vec![
                    method("accept", &["Lsample/Base;", "I"], "V", 9),
                    method("accept", &["[Lsample/Base;"], "V", 9 | 0x80),
                ],
            ),
            class(
                "Lsample/Target;",
                "Lsample/Base;",
                vec![method("accept", &["Lsample/Base;", "I"], "V", 9)],
            ),
        ] {
            let hierarchy = TypeHierarchy::from_classes([&target, &base, &sub]).unwrap();
            assert!(!proven_call_argument(
                &value,
                "Lsample/Target;",
                "accept",
                &args,
                "V",
                0x71,
                0,
                Some(&hierarchy),
            ));
        }
    }

    #[test]
    fn inherited_virtual_overload_blocks_argument_cleanup() {
        let base = class(
            "Lsample/Base;",
            "Ljava/lang/Object;",
            vec![method("accept", &["Ljava/lang/String;"], "V", 1)],
        );
        let child = class(
            "Lsample/Child;",
            "Lsample/Base;",
            vec![method("accept", &["Ljava/lang/Object;"], "V", 1)],
        );
        let hierarchy = TypeHierarchy::from_classes([&base, &child]).unwrap();
        assert!(!proven_call_argument(
            &value("text", "Ljava/lang/String;"),
            "Lsample/Child;",
            "accept",
            &["Ljava/lang/Object;".into()],
            "V",
            0x6e,
            0,
            Some(&hierarchy),
        ));
    }

    #[test]
    fn synthetic_static_is_safe_but_generic_constructor_is_not() {
        let static_owner = class(
            "Lsample/Target;",
            "Ljava/lang/Object;",
            vec![method(
                "accept$default",
                &["Ljava/lang/Object;"],
                "V",
                9 | 0x1000,
            )],
        );
        let hierarchy = TypeHierarchy::from_classes([&static_owner]).unwrap();
        assert!(proven_call_argument(
            &value("text", "Ljava/lang/String;"),
            "Lsample/Target;",
            "accept$default",
            &["Ljava/lang/Object;".into()],
            "V",
            0x71,
            0,
            Some(&hierarchy),
        ));

        let mut generic = class(
            "Lsample/Generic;",
            "Ljava/lang/Object;",
            vec![method("<init>", &["Ljava/lang/Object;"], "V", 1)],
        );
        generic.annotations_offset = 1;
        let symbols = Arc::get_mut(&mut generic.symbols).unwrap();
        symbols.types.push("Ldalvik/annotation/Signature;".into());
        symbols.annotations.insert(
            1,
            Arc::new(DexAnnotationDirectory {
                class: Some(Arc::from([Arc::new(DexAnnotation {
                    visibility: 2,
                    type_idx: 0,
                    elements: vec![],
                })])),
                ..Default::default()
            }),
        );
        let hierarchy = TypeHierarchy::from_classes([&generic]).unwrap();
        assert!(!proven_call_argument(
            &value("text", "Ljava/lang/String;"),
            "Lsample/Generic;",
            "<init>",
            &["Ljava/lang/Object;".into()],
            "V",
            0x70,
            0,
            Some(&hierarchy),
        ));
    }

    #[test]
    fn unique_constructor_and_sdk_void_receiver_proofs() {
        let box_class = class(
            "Lsample/Box;",
            "Ljava/lang/Object;",
            vec![method("<init>", &["Ljava/lang/Object;", "I"], "V", 1)],
        );
        let activity = class("Lsample/MyActivity;", "Landroid/app/Activity;", vec![]);
        let hierarchy = TypeHierarchy::from_classes([&box_class, &activity]).unwrap();
        let ctor_args = [Arc::from("Ljava/lang/Object;"), Arc::from("I")];
        assert!(proven_call_argument(
            &value("activity", "Lsample/MyActivity;"),
            "Lsample/Box;",
            "<init>",
            &ctor_args,
            "V",
            0x70,
            0,
            Some(&hierarchy),
        ));
        assert!(proven_call_argument(
            &value("this", "Lsample/MyActivity;"),
            "Lsample/Box;",
            "<init>",
            &ctor_args,
            "V",
            0x70,
            0,
            Some(&hierarchy),
        ));
        assert!(proven_void_receiver(
            &value("activity", "Lsample/MyActivity;"),
            "Landroid/app/Activity;",
            "finish",
            &[],
            "V",
            0x6e,
            Some(&hierarchy),
        ));
        assert!(proven_void_receiver(
            &value("this", "Lsample/MyActivity;"),
            "Landroid/app/Activity;",
            "finish",
            &[],
            "V",
            0x6e,
            Some(&hierarchy),
        ));
        assert!(!proven_void_receiver(
            &value("activity", "Lsample/MyActivity;"),
            "Landroid/app/Activity;",
            "getIntent",
            &[],
            "Landroid/content/Intent;",
            0x6e,
            Some(&hierarchy),
        ));
        let shadow = class(
            "Landroid/app/Activity;",
            "Landroid/content/Context;",
            vec![],
        );
        let shadowed = TypeHierarchy::from_classes([&box_class, &activity, &shadow]).unwrap();
        assert!(!proven_void_receiver(
            &value("activity", "Lsample/MyActivity;"),
            "Landroid/app/Activity;",
            "finish",
            &[],
            "V",
            0x6e,
            Some(&shadowed),
        ));
        let conflicting = class(
            "Lsample/MyActivity;",
            "Landroid/app/Activity;",
            vec![method("finish", &[], "I", 1)],
        );
        let conflicting_hierarchy =
            TypeHierarchy::from_classes([&box_class, &conflicting]).unwrap();
        assert!(!proven_void_receiver(
            &value("activity", "Lsample/MyActivity;"),
            "Landroid/app/Activity;",
            "finish",
            &[],
            "V",
            0x6e,
            Some(&conflicting_hierarchy),
        ));
        let hidden = class(
            "Lsample/MyActivity;",
            "Landroid/app/Activity;",
            vec![method("finish", &[], "V", 2)],
        );
        let hidden_hierarchy = TypeHierarchy::from_classes([&box_class, &hidden]).unwrap();
        assert!(!proven_void_receiver(
            &value("activity", "Lsample/MyActivity;"),
            "Landroid/app/Activity;",
            "finish",
            &[],
            "V",
            0x6e,
            Some(&hidden_hierarchy),
        ));
    }
}
