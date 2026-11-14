//! Bounds helpers for untrusted DEX sizes (wasm32-safe; no capacity-overflow panics).

use crate::error::{DexError, Result};

/// End offset of a table of `n` items of `stride` bytes starting at `off`.
/// Rejects mul/add overflow (corrupt header sizes on 32-bit targets).
pub fn table_end(off: usize, n: usize, stride: usize, what: &str) -> Result<usize> {
    let bytes = n
        .checked_mul(stride)
        .ok_or_else(|| DexError::Truncated(format!("{what}: size overflow ({n} * {stride})")))?;
    off.checked_add(bytes)
        .ok_or_else(|| DexError::Truncated(format!("{what}: offset overflow")))
}

/// Allocate a `Vec` with capacity `n` if it cannot overflow the allocator guard.
///
/// `Vec::with_capacity` panics with "capacity overflow" when
/// `n > isize::MAX / size_of::<T>()` — common with corrupt ULEB128 sizes on wasm32.
pub fn vec_with_capacity<T>(n: usize, what: &str) -> Result<Vec<T>> {
    if !capacity_ok::<T>(n) {
        return Err(DexError::Truncated(format!(
            "{what}: capacity overflow ({n})"
        )));
    }
    Ok(Vec::with_capacity(n))
}

/// True when `Vec::<T>::with_capacity(n)` will not panic on capacity overflow.
#[inline]
pub fn capacity_ok<T>(n: usize) -> bool {
    let elem = std::mem::size_of::<T>();
    if elem == 0 {
        return true;
    }
    n <= (isize::MAX as usize) / elem
}

/// Ensure a counted list can fit in the remaining bytes (each entry ≥ 1 byte).
pub fn ensure_count_fits(count: usize, remaining: usize, what: &str) -> Result<()> {
    if count > remaining {
        return Err(DexError::Truncated(format!(
            "{what}: count {count} exceeds remaining {remaining} bytes"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_end_overflow() {
        assert!(table_end(0, usize::MAX / 2 + 1, 4, "t").is_err());
        assert!(table_end(usize::MAX - 2, 2, 4, "t").is_err());
    }

    #[test]
    fn vec_with_capacity_huge_is_err() {
        // Must reject without attempting a multi-GB allocation.
        let too_many = (isize::MAX as usize / std::mem::size_of::<u64>()).saturating_add(1);
        let err = vec_with_capacity::<u64>(too_many, "huge").unwrap_err();
        assert!(err.to_string().contains("capacity overflow"));
        assert!(!capacity_ok::<u8>((isize::MAX as usize).saturating_add(1)));
    }

    #[test]
    fn ensure_count_fits_ok() {
        assert!(ensure_count_fits(3, 10, "x").is_ok());
        assert!(ensure_count_fits(11, 10, "x").is_err());
    }
}
