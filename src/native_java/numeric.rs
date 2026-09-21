//! Typed numeric opcode semantics. DEX constants retain raw bits until a typed
//! use establishes whether those bits represent an integer or floating value.
use anyhow::{Result, bail, ensure};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Int,
    Long,
    Float,
    Double,
}
impl Kind {
    pub(crate) fn descriptor(self) -> &'static str {
        match self {
            Self::Int => "I",
            Self::Long => "J",
            Self::Float => "F",
            Self::Double => "D",
        }
    }
    pub(crate) fn width(self) -> usize {
        if matches!(self, Self::Long | Self::Double) {
            2
        } else {
            1
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Literal {
    Bits32(u32),
    Bits64(u64),
}
impl Literal {
    pub(crate) fn render(self, kind: Kind) -> Result<String> {
        Ok(match (self, kind) {
            (Self::Bits32(bits), Kind::Int) => (bits as i32).to_string(),
            (Self::Bits64(bits), Kind::Long) => format!("{}L", bits as i64),
            (Self::Bits32(bits), Kind::Float) => {
                let value = f32::from_bits(bits);
                ensure!(!value.is_nan(), "NaN payload reconstruction unsupported");
                if value.is_infinite() {
                    if value.is_sign_negative() {
                        "(-1.0f / 0.0f)".into()
                    } else {
                        "(1.0f / 0.0f)".into()
                    }
                } else {
                    let literal = format!("{value:?}");
                    ensure!(
                        literal.parse::<f32>()?.to_bits() == bits,
                        "Float literal changed raw bits"
                    );
                    format!("{literal}f")
                }
            }
            (Self::Bits64(bits), Kind::Double) => {
                let value = f64::from_bits(bits);
                ensure!(!value.is_nan(), "NaN payload reconstruction unsupported");
                if value.is_infinite() {
                    if value.is_sign_negative() {
                        "(-1.0d / 0.0d)".into()
                    } else {
                        "(1.0d / 0.0d)".into()
                    }
                } else {
                    let literal = format!("{value:?}");
                    ensure!(
                        literal.parse::<f64>()?.to_bits() == bits,
                        "Double literal changed raw bits"
                    );
                    format!("{literal}d")
                }
            }
            _ => bail!("Numeric literal width/type mismatch"),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Unary {
    pub input: Kind,
    pub result: Kind,
    /// Narrow integral conversion results retain the exact Java descriptor.
    pub result_descriptor: &'static str,
    prefix: &'static str,
}
impl Unary {
    pub(crate) fn decode(op: u8) -> Option<Self> {
        use Kind::*;
        let (input, result, descriptor, prefix) = match op {
            0x7b => (Int, Int, "I", "-"),
            0x7c => (Int, Int, "I", "~"),
            0x7d => (Long, Long, "J", "-"),
            0x7e => (Long, Long, "J", "~"),
            0x7f => (Float, Float, "F", "-"),
            0x80 => (Double, Double, "D", "-"),
            0x81 => (Int, Long, "J", "(long) "),
            0x82 => (Int, Float, "F", "(float) "),
            0x83 => (Int, Double, "D", "(double) "),
            0x84 => (Long, Int, "I", "(int) "),
            0x85 => (Long, Float, "F", "(float) "),
            0x86 => (Long, Double, "D", "(double) "),
            0x87 => (Float, Int, "I", "(int) "),
            0x88 => (Float, Long, "J", "(long) "),
            0x89 => (Float, Double, "D", "(double) "),
            0x8a => (Double, Int, "I", "(int) "),
            0x8b => (Double, Long, "J", "(long) "),
            0x8c => (Double, Float, "F", "(float) "),
            0x8d => (Int, Int, "B", "(byte) "),
            0x8e => (Int, Int, "C", "(char) "),
            0x8f => (Int, Int, "S", "(short) "),
            _ => return None,
        };
        Some(Self {
            input,
            result,
            result_descriptor: descriptor,
            prefix,
        })
    }
    pub(crate) fn expression(self, operand: &str) -> String {
        format!("{}({operand})", self.prefix)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Binary {
    pub left: Kind,
    pub right: Kind,
    pub result: Kind,
    pub operator: &'static str,
}
impl Binary {
    /// 23x (0x90..0xaf) and 12x /2addr (0xb0..0xcf) families.
    pub(crate) fn decode(op: u8) -> Option<Self> {
        use Kind::*;
        let op = if (0xb0..=0xcf).contains(&op) {
            op - 0x20
        } else {
            op
        };
        let (kind, offset) = match op {
            0x90..=0x9a => (Int, op - 0x90),
            0x9b..=0xa5 => (Long, op - 0x9b),
            0xa6..=0xaa => (Float, op - 0xa6),
            0xab..=0xaf => (Double, op - 0xab),
            _ => return None,
        };
        let operator = ["+", "-", "*", "/", "%", "&", "|", "^", "<<", ">>", ">>>"][offset as usize];
        Some(Self {
            left: kind,
            right: if offset >= 8 { Int } else { kind },
            result: kind,
            operator,
        })
    }
    pub(crate) fn expression(self, left: &str, right: &str) -> String {
        // Java applies DEX's 0x1f/0x3f shift masking and integer overflow rules.
        format!("({left}) {} ({right})", self.operator)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Compare {
    pub input: Kind,
    pub nan_result: Option<i32>,
}
impl Compare {
    pub(crate) fn decode(op: u8) -> Option<Self> {
        let (input, nan_result) = match op {
            0x2d => (Kind::Float, Some(-1)),
            0x2e => (Kind::Float, Some(1)),
            0x2f => (Kind::Double, Some(-1)),
            0x30 => (Kind::Double, Some(1)),
            0x31 => (Kind::Long, None),
            _ => return None,
        };
        Some(Self { input, nan_result })
    }
    /// Operands MUST be immutable materialized values: comparison repeats them.
    /// Float.compare/Double.compare would be wrong for signed zero and NaNs.
    pub(crate) fn expression(self, left: &str, right: &str) -> String {
        if self.nan_result == Some(-1) {
            format!("(({left}) > ({right}) ? 1 : (({left}) == ({right}) ? 0 : -1))")
        } else {
            format!("(({left}) < ({right}) ? -1 : (({left}) == ({right}) ? 0 : 1))")
        }
    }
}
