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
                    .any(|ty| ty.as_ref() == value.ty || subtype(class, &value.ty, ty))),
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
    caught == "Lorg/json/JSONException;"
        && owner == "Lorg/json/JSONObject;"
        && name == "<init>"
        && ret == "V"
        && args.len() == 1
        && args[0].as_ref() == "Ljava/lang/String;"
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
