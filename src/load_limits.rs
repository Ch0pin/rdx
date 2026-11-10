//! Explicit per-load profiles. Higher limits never become global defaults.
use std::fmt;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LoadLimits {
    #[default]
    Standard,
    Large,
}
impl LoadLimits {
    pub const fn retained_bytes(self) -> usize {
        match self {
            Self::Standard => 1024 * 1024 * 1024,
            Self::Large => 2 * 1024 * 1024 * 1024,
        }
    }
    pub const fn classes(self) -> usize {
        match self {
            Self::Standard => 250_000,
            Self::Large => 600_000,
        }
    }
    pub const fn resource_index_bytes(self) -> usize {
        match self {
            Self::Standard => 256 * 1024 * 1024,
            Self::Large => 512 * 1024 * 1024,
        }
    }
    pub const fn resource_table_bytes(self) -> u64 {
        match self {
            Self::Standard => 64 * 1024 * 1024,
            Self::Large => 128 * 1024 * 1024,
        }
    }
    pub const fn total_dex_bytes(self) -> u64 {
        match self {
            Self::Standard => 256 * 1024 * 1024,
            Self::Large => 512 * 1024 * 1024,
        }
    }
    pub const fn allowance(self, resource: LoadResource) -> u64 {
        match resource {
            LoadResource::RetainedDex => self.retained_bytes() as u64,
            LoadResource::Classes => self.classes() as u64,
            LoadResource::TotalDex => self.total_dex_bytes(),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadResource {
    RetainedDex,
    Classes,
    TotalDex,
}

#[derive(Clone, Debug)]
pub struct LoadLimitExceeded {
    pub resource: LoadResource,
    pub limit: u64,
    pub required_at_least: u64,
}
impl fmt::Display for LoadLimitExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (name, unit) = match self.resource {
            LoadResource::RetainedDex => ("Parsed DEX allocation budget", "bytes"),
            LoadResource::Classes => ("Class inventory limit", "classes"),
            LoadResource::TotalDex => ("Combined DEX input limit", "bytes"),
        };
        write!(
            f,
            "{name} exceeded: needs at least {} {unit}, current limit {} {unit}",
            self.required_at_least, self.limit
        )
    }
}
impl std::error::Error for LoadLimitExceeded {}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn standard_defaults_and_large_profile_remain_bounded() {
        assert_eq!(LoadLimits::default(), LoadLimits::Standard);
        assert_eq!(LoadLimits::Standard.retained_bytes(), 1024 * 1024 * 1024);
        assert_eq!(LoadLimits::Standard.classes(), 250_000);
        assert_eq!(LoadLimits::Large.retained_bytes(), 2 * 1024 * 1024 * 1024);
        assert_eq!(LoadLimits::Large.classes(), 600_000);
        assert_eq!(
            LoadLimits::Standard.resource_index_bytes(),
            256 * 1024 * 1024
        );
        assert_eq!(LoadLimits::Large.resource_index_bytes(), 512 * 1024 * 1024);
        assert_eq!(
            LoadLimits::Standard.resource_table_bytes(),
            64 * 1024 * 1024
        );
        assert_eq!(LoadLimits::Large.resource_table_bytes(), 128 * 1024 * 1024);
        assert_eq!(LoadLimits::Large.total_dex_bytes(), 512 * 1024 * 1024);
    }
}
