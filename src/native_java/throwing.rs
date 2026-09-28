//! Conservative throw emission from recognized unchecked types or an exact
//! declared exception type. Unknown hierarchy relationships remain unsupported.
use super::Value;
use crate::native_dex::{DexClass, DexMethod};
use anyhow::{Result, ensure};

fn subtype(class: &DexClass, source: &str, target: &str) -> bool {
    class.symbols.hierarchy.get().is_some_and(|hierarchy| {
        hierarchy.assignable(source, target) == crate::native_hierarchy::Relation::Proven
    })
}

fn unchecked_type(class: &DexClass, ty: &str) -> bool {
    unchecked(ty)
        || subtype(class, ty, "Ljava/lang/RuntimeException;")
        || subtype(class, ty, "Ljava/lang/Error;")
        || (ty == class.descriptor.as_ref() && class.superclass.as_deref().is_some_and(unchecked))
}

fn unchecked(ty: &str) -> bool {
    matches!(
        ty,
        "Ljava/lang/RuntimeException;"
            | "Landroid/content/ActivityNotFoundException;"
            | "Ljava/lang/Error;"
            | "Ljava/lang/IllegalArgumentException;"
            | "Ljava/lang/IllegalStateException;"
            | "Ljava/lang/NullPointerException;"
            | "Ljava/lang/UnsupportedOperationException;"
            | "Ljava/lang/IndexOutOfBoundsException;"
            | "Ljava/lang/ArrayIndexOutOfBoundsException;"
            | "Ljava/lang/StringIndexOutOfBoundsException;"
            | "Ljava/lang/ClassCastException;"
            | "Ljava/lang/ArithmeticException;"
            | "Ljava/lang/SecurityException;"
            | "Ljava/lang/NumberFormatException;"
            | "Ljava/lang/NegativeArraySizeException;"
            | "Ljava/lang/ArrayStoreException;"
            | "Ljava/util/ConcurrentModificationException;"
            | "Ljava/util/NoSuchElementException;"
            | "Ljava/lang/AssertionError;"
            | "Ljava/lang/ExceptionInInitializerError;"
            | "Ljava/lang/NoClassDefFoundError;"
            | "Ljava/lang/LinkageError;"
            | "Ljava/lang/OutOfMemoryError;"
            | "Ljava/lang/StackOverflowError;"
    )
}

fn checked(ty: &str) -> bool {
    matches!(
        ty,
        "Ljava/lang/Throwable;"
            | "Landroid/os/RemoteException;"
            | "Lorg/json/JSONException;"
            | "Ljava/lang/Exception;"
            | "Ljava/io/IOException;"
            | "Ljava/io/FileNotFoundException;"
            | "Ljava/io/EOFException;"
            | "Ljava/io/InterruptedIOException;"
            | "Ljava/io/UnsupportedEncodingException;"
            | "Ljava/io/UTFDataFormatException;"
            | "Ljava/net/SocketException;"
            | "Ljava/net/SocketTimeoutException;"
            | "Ljava/net/UnknownHostException;"
            | "Ljava/net/MalformedURLException;"
            | "Ljava/lang/ReflectiveOperationException;"
            | "Ljava/lang/ClassNotFoundException;"
            | "Ljava/lang/NoSuchMethodException;"
            | "Ljava/lang/NoSuchFieldException;"
            | "Ljava/lang/IllegalAccessException;"
            | "Ljava/lang/InstantiationException;"
            | "Ljava/lang/InterruptedException;"
            | "Ljava/lang/CloneNotSupportedException;"
            | "Ljava/lang/reflect/InvocationTargetException;"
            | "Ljava/sql/SQLException;"
            | "Ljava/text/ParseException;"
            | "Ljava/util/concurrent/ExecutionException;"
            | "Ljava/util/concurrent/TimeoutException;"
    )
}

pub(super) fn inferable_checked_type(class: &DexClass, ty: &str) -> bool {
    validate_type(class, ty).is_ok() && !unchecked_type(class, ty)
}

pub(super) fn validate_type(class: &DexClass, ty: &str) -> Result<()> {
    ensure!(
        unchecked_type(class, ty)
            || checked(ty)
            || subtype(class, ty, "Ljava/lang/Throwable;")
            || (ty == class.descriptor.as_ref()
                && class
                    .superclass
                    .as_deref()
                    .is_some_and(|parent| unchecked(parent) || checked(parent))),
        "Exception type requires unavailable Throwable hierarchy"
    );
    Ok(())
}

pub(super) fn validate_catch_type(class: &DexClass, ty: &str, protected_throw: bool) -> Result<()> {
    validate_type(class, ty)?;
    ensure!(
        protected_throw
            || unchecked_type(class, ty)
            || matches!(ty, "Ljava/lang/Exception;" | "Ljava/lang/Throwable;")
            || (ty == class.descriptor.as_ref()
                && class.superclass.as_deref().is_some_and(unchecked)),
        "Checked catch requires unavailable protected-call exception analysis"
    );
    Ok(())
}

pub(super) fn expression(class: &DexClass, method: &DexMethod, value: &Value) -> Result<String> {
    if value.literal == Some(0) {
        return Ok("null".into());
    }
    let direct_unchecked_subclass =
        value.ty == class.descriptor.as_ref() && class.superclass.as_deref().is_some_and(unchecked);
    let known_checked = checked(&value.ty)
        || subtype(class, &value.ty, "Ljava/lang/Throwable;")
        || (value.ty == class.descriptor.as_ref()
            && class
                .superclass
                .as_deref()
                .is_some_and(|ty| checked(ty) || unchecked(ty)));
    ensure!(
        unchecked_type(class, &value.ty)
            || direct_unchecked_subclass
            || (known_checked
                && method.name.as_ref() != "<clinit>"
                && method
                    .thrown_types
                    .iter()
                    .map(AsRef::as_ref)
                    .chain(super::super::inherited_override_exception(class, method))
                    .any(|ty| ty == value.ty || subtype(class, &value.ty, ty))),
        "Throw requires an established unchecked type or matching declared exception"
    );
    Ok(value.text.clone())
}

// Android's org.json implementation declares a checked JSONException. Keep
// this exact descriptor proof rather than accepting every checked catch.
// https://developer.android.com/reference/org/json/JSONObject#JSONObject(java.lang.String)
pub(super) fn known_call_throws(
    owner: &str,
    name: &str,
    args: &[std::sync::Arc<str>],
    ret: &str,
    caught: &str,
) -> bool {
    (caught == "Landroid/os/RemoteException;"
        && owner == "Landroid/os/IBinder;"
        && name == "transact"
        && ret == "Z"
        && args.iter().map(AsRef::as_ref).eq([
            "I",
            "Landroid/os/Parcel;",
            "Landroid/os/Parcel;",
            "I",
        ]))
        || caught == "Lorg/json/JSONException;"
            && owner == "Lorg/json/JSONObject;"
            && name == "<init>"
            && ret == "V"
            && args.len() == 1
            && args[0].as_ref() == "Ljava/lang/String;"
}

// Throws metadata is optional in DEX. Infer an explicit checked throw only
// when the hierarchy proves both its declaration contract and exact loaded
// callers. Private methods retain their separate non-override rule; other
// methods require a final owner, while constructors use exact direct calls.
pub(super) fn may_infer_declaration(class: &DexClass, method: &DexMethod, value: &Value) -> bool {
    let declaration_permitted = method.access_flags & 2 != 0
        || class.symbols.hierarchy.get().is_some_and(|hierarchy| {
            hierarchy.permits_inferred_checked_throw(class, method, &value.ty)
        });
    method.name.as_ref() != "<clinit>"
        && method.thrown_types.is_empty()
        && declaration_permitted
        && validate_type(class, &value.ty).is_ok()
        && !unchecked_type(class, &value.ty)
}

/// An exact one-instruction static helper that throws its sole checked
/// parameter has an order-independent inferred contract. No other body shape
/// is summarized here; calls with effects or control flow need full analysis.
pub(super) fn simple_static_parameter_throw<'a>(
    class: &DexClass,
    method: &'a DexMethod,
) -> Option<&'a str> {
    if method.access_flags & 8 == 0
        || !method.thrown_types.is_empty()
        || method.parameters.len() != 1
    {
        return None;
    }
    let code = method.code.as_ref()?;
    let parameter_register = code.registers.checked_sub(code.ins)?;
    if code.ins != 1
        || !code.try_regions.is_empty()
        || code.instructions.as_slice() != [0x27 | (parameter_register << 8)]
    {
        return None;
    }
    let ty = method.parameters[0].as_ref();
    (inferable_checked_type(class, ty)
        && may_infer_declaration(
            class,
            method,
            &Value {
                text: String::new(),
                ty: ty.to_string(),
                literal: None,
                wide_literal: None,
                raw_bits32: false,
            },
        ))
    .then_some(ty)
}

pub(super) fn locally_caught(
    class: &DexClass,
    method: &DexMethod,
    pc: usize,
    value: &Value,
) -> bool {
    validate_type(class, &value.ty).is_ok()
        && method.code.as_ref().is_some_and(|code| {
            code.try_regions.iter().any(|region| {
                region.start as usize <= pc
                    && pc < region.end as usize
                    && region.catches.iter().any(|(ty, _)| {
                        ty.as_ref().is_none_or(|ty| {
                            ty.as_ref() == "Ljava/lang/Throwable;"
                                || ty.as_ref() == value.ty
                                || subtype(class, &value.ty, ty)
                        })
                    })
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checked_json_constructor_proof_uses_exact_signature() {
        let args = vec![std::sync::Arc::<str>::from("Ljava/lang/String;")];
        assert!(known_call_throws(
            "Lorg/json/JSONObject;",
            "<init>",
            &args,
            "V",
            "Lorg/json/JSONException;"
        ));
        assert!(!known_call_throws(
            "Lorg/json/JSONObject;",
            "optString",
            &args,
            "Ljava/lang/String;",
            "Lorg/json/JSONException;"
        ));
        assert!(!known_call_throws(
            "Lorg/json/JSONObject;",
            "<init>",
            &[],
            "V",
            "Lorg/json/JSONException;"
        ));
        assert!(!known_call_throws(
            "Lother/JSONObject;",
            "<init>",
            &args,
            "V",
            "Lorg/json/JSONException;"
        ));
        assert!(!known_call_throws(
            "Lorg/json/JSONObject;",
            "<init>",
            &args,
            "V",
            "Ljava/io/IOException;"
        ));
    }
}
