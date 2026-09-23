//! Deterministic Java display names for DEX members.
use super::identifier;
use anyhow::{Result, bail};

const PREFIX: &str = "_rdx_";
const MAX_ENCODED_LENGTH: usize = 4096;

/// Maps every non-Java member name to an injective ASCII identifier.  Names
/// already beginning with the alias prefix are encoded too, so no DEX name can
/// collide with a generated alias.
pub(super) fn member(name: &str) -> Result<String> {
    if identifier(name) && !name.starts_with(PREFIX) {
        return Ok(name.into());
    }
    let encoded_len = PREFIX.len().saturating_add(name.len().saturating_mul(2));
    if encoded_len > MAX_ENCODED_LENGTH {
        bail!("member alias exceeds reconstruction limit");
    }
    let mut alias = String::with_capacity(encoded_len);
    alias.push_str(PREFIX);
    for byte in name.bytes() {
        use std::fmt::Write;
        write!(&mut alias, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok(alias)
}

/// Stable aliases are presentation-only; DEX descriptors remain symbol identities.
/// Encode reserved-prefix names too, so independently rendered classes agree
/// without a corpus-wide rename table or collisions with original identifiers.
pub(super) fn qualified(name: &str, separator: char) -> Result<String> {
    anyhow::ensure!(
        name.len() <= MAX_ENCODED_LENGTH,
        "class alias exceeds reconstruction limit"
    );
    if name
        .split(separator)
        .all(|part| identifier(part) && !part.starts_with(PREFIX))
    {
        return Ok(name.replace(separator, "."));
    }
    let parts: Vec<_> = name.split(separator).collect();
    anyhow::ensure!(
        parts
            .iter()
            .all(|part| !part.is_empty()
                && !part.chars().any(|c| matches!(c, '/' | '.' | ';' | '['))),
        "Invalid class name structure"
    );
    let display = parts
        .into_iter()
        .map(member)
        .collect::<Result<Vec<_>>>()?
        .join(".");
    anyhow::ensure!(
        display.len() <= MAX_ENCODED_LENGTH,
        "class alias exceeds reconstruction limit"
    );
    Ok(display)
}

pub(super) fn label(descriptor: &str) -> Option<String> {
    descriptor
        .trim_start_matches('[')
        .strip_prefix('L')?
        .strip_suffix(';')
        .map(|name| name.replace('/', "."))
}

#[cfg(test)]
mod tests {
    use super::member;

    #[test]
    fn aliases_invalid_and_reserved_prefix_names_injectively() {
        assert_eq!(member("validName").unwrap(), "validName");
        assert_eq!(member("foo-bar").unwrap(), "_rdx_666f6f2d626172");
        assert_eq!(member("_rdx_foo").unwrap(), "_rdx_5f7264785f666f6f");
        assert_ne!(
            member("foo-bar").unwrap(),
            member("_rdx_666f6f2d626172").unwrap()
        );
    }
}

#[cfg(test)]
mod class_tests {
    use super::*;
    #[test]
    fn aliases_are_injective_and_malformed_paths_still_fail() {
        let originals = [
            "Foo-Bar",
            "Foo_Bar",
            "class",
            "_rdx_636c617373",
            "é",
            "1",
            "Outer$1",
            "ordinary",
        ];
        let aliases: std::collections::BTreeSet<_> = originals
            .iter()
            .map(|s| qualified(s, '/').unwrap())
            .collect();
        assert_eq!(aliases.len(), originals.len());
        assert!(aliases.iter().all(|s| identifier(s)));
        for bad in ["", "a//b", "/a", "a/", "a.b", "a;b", "[a"] {
            assert!(qualified(bad, '/').is_err(), "{bad}");
        }
        assert!(qualified(&"!".repeat(3000), '/').is_err());
    }
}
