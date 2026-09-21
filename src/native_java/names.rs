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
