//! Numeric values shared by checking, core normalization, and emission.

use crate::ast::Role;

#[derive(Clone, Copy)]
pub struct NumericLiteral<'a> {
    pub(crate) digits: &'a str,
    pub(crate) integer: Option<&'a crate::ast::IntegerLiteralDiagnostic>,
}

pub(crate) fn float_decimal(role: Role, digits: &str) -> Option<String> {
    let digits = scientific_decimal(digits)?;
    let value = match role {
        Role::F32 => f64::from(digits.parse::<f32>().ok()?),
        Role::F64 => digits.parse::<f64>().ok()?,
        _ => return None,
    };
    if !value.is_finite() {
        return None;
    }
    let mut decimal = value.to_string();
    if !decimal.contains('.') {
        let point = decimal.find(['e', 'E']).unwrap_or(decimal.len());
        decimal.insert_str(point, ".0");
    }
    Some(decimal)
}

/// Combine the written exponent with the decimal-point position before the
/// IEEE parser sees either. Its exponent accumulator can saturate for long
/// source mantissas whose exponent cancels their scale. Moving the point
/// preserves every significant digit and never expands the coefficient.
fn scientific_decimal(source: &str) -> Option<String> {
    let (negative, source) = source
        .strip_prefix('-')
        .map_or((false, source), |s| (true, s));
    let (mantissa, exponent) = source
        .find(['e', 'E'])
        .map_or((source, "0"), |at| (&source[..at], &source[at + 1..]));
    let (whole, fraction) = mantissa.split_once('.')?;
    if whole.is_empty()
        || fraction.is_empty()
        || !whole
            .bytes()
            .chain(fraction.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let (exponent_negative, exponent) = exponent.strip_prefix('-').map_or_else(
        || (false, exponent.strip_prefix('+').unwrap_or(exponent)),
        |digits| (true, digits),
    );
    if exponent.is_empty() || !exponent.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let exponent = exponent.bytes().fold(0_i128, |value, digit| {
        value
            .saturating_mul(10)
            .saturating_add(i128::from(digit - b'0'))
    });
    let exponent = if exponent_negative {
        -exponent
    } else {
        exponent
    };
    let coefficient = whole.bytes().chain(fraction.bytes());
    let Some(first) = coefficient.clone().position(|digit| digit != b'0') else {
        return Some(if negative { "-0.0" } else { "0.0" }.to_owned());
    };
    let last =
        coefficient.clone().enumerate().fold(
            first,
            |last, (index, digit)| if digit != b'0' { index } else { last },
        );
    let exponent = exponent.saturating_add(whole.len() as i128 - first as i128 - 1);
    let mut normalized = String::with_capacity(last - first + 24);
    if negative {
        normalized.push('-');
    }
    let mut significant = coefficient.skip(first).take(last - first + 1);
    normalized.push(char::from(significant.next()?));
    normalized.push('.');
    if first == last {
        normalized.push('0');
    } else {
        normalized.extend(significant.map(char::from));
    }
    normalized.push('e');
    normalized.push_str(&exponent.to_string());
    Some(normalized)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Integer {
    negative: bool,
    magnitude: u128,
}

impl Integer {
    pub(crate) fn parse(digits: &str) -> Option<Self> {
        let (negative, digits) = digits
            .strip_prefix('-')
            .map_or((false, digits), |digits| (true, digits));
        if digits.is_empty() || !digits.bytes().all(|digit| digit.is_ascii_digit()) {
            return None;
        }
        let magnitude = digits.parse::<u128>().ok()?;
        Some(Self {
            negative: negative && magnitude != 0,
            magnitude,
        })
    }

    pub(crate) fn fits(self, role: Role) -> bool {
        let (signed, bits) = match role {
            Role::I8 => (true, 8),
            Role::I16 => (true, 16),
            Role::I32 => (true, 32),
            Role::I64 => (true, 64),
            Role::I128 => (true, 128),
            Role::U8 => (false, 8),
            Role::U16 => (false, 16),
            Role::U32 => (false, 32),
            Role::U64 => (false, 64),
            Role::U128 => (false, 128),
            Role::F32 | Role::F64 | Role::Bool | Role::Str => return false,
        };
        if signed {
            let negative_limit = 1_u128 << (bits - 1);
            self.magnitude <= negative_limit - u128::from(!self.negative)
        } else {
            !self.negative && (bits == 128 || self.magnitude < (1_u128 << bits))
        }
    }

    pub(crate) fn decimal(self) -> String {
        if self.negative {
            format!("-{}", self.magnitude)
        } else {
            self.magnitude.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_role_boundaries() {
        for (signed, roles) in [
            (
                true,
                [Role::I8, Role::I16, Role::I32, Role::I64, Role::I128],
            ),
            (
                false,
                [Role::U8, Role::U16, Role::U32, Role::U64, Role::U128],
            ),
        ] {
            for (bits, role) in [8, 16, 32, 64, 128].into_iter().zip(roles) {
                let maximum = if signed {
                    (1_u128 << (bits - 1)) - 1
                } else if bits == 128 {
                    u128::MAX
                } else {
                    (1_u128 << bits) - 1
                };
                assert!(Integer::parse(&maximum.to_string()).unwrap().fits(role));
                if let Some(overflow) = maximum.checked_add(1) {
                    assert!(!Integer::parse(&overflow.to_string()).unwrap().fits(role));
                }
                let minimum = if signed { maximum + 1 } else { 0 };
                assert!(Integer::parse(&format!("-{minimum}")).unwrap().fits(role));
                assert!(
                    !Integer::parse(&format!("-{}", minimum + 1))
                        .unwrap()
                        .fits(role)
                );
            }
        }
        assert!(Integer::parse("340282366920938463463374607431768211456").is_none());
    }

    #[test]
    fn integer_decimal_value_is_independent_of_spelling() {
        for (source, expected) in [("00010", "10"), ("-00010", "-10"), ("-000", "0")] {
            assert_eq!(Integer::parse(source).unwrap().decimal(), expected);
        }
        assert!(Integer::parse("-0").unwrap().fits(Role::U128));
        assert_eq!(
            Integer::parse(&format!("{}1", "0".repeat(1000)))
                .unwrap()
                .decimal(),
            "1"
        );
        for malformed in ["", "-", "+1", "1e0", "1_0", "1.0"] {
            assert!(Integer::parse(malformed).is_none(), "{malformed}");
        }
    }

    #[test]
    fn float_rounding_is_direct_and_preserves_ieee_values() {
        for (source, bits) in [
            ("0.1", 0x3dcccccd_u32),
            ("16777217.0", 0x4b800000),
            ("1.0000000596046448", 0x3f800001),
            ("1.000000059604644775390625", 0x3f800000),
            ("1.000000178813934326171875", 0x3f800002),
            ("1.0e-45", 1),
            ("3.4028234663852886e38", 0x7f7fffff),
            ("-0.0", 0x80000000),
            ("-1.0e-9999", 0x80000000),
        ] {
            let canonical = float_decimal(Role::F32, source).unwrap();
            assert_eq!(
                canonical.parse::<f64>().unwrap().to_bits(),
                f64::from(f32::from_bits(bits)).to_bits(),
                "{source}"
            );
            assert_eq!(float_decimal(Role::F32, &canonical), Some(canonical));
        }
        for (source, bits) in [
            ("0.1", 0x3fb999999999999a_u64),
            ("5.0e-324", 1),
            ("1.7976931348623157e308", 0x7fefffffffffffff),
            ("-0.0", 0x8000000000000000),
            ("-1.0e-9999", 0x8000000000000000),
        ] {
            let canonical = float_decimal(Role::F64, source).unwrap();
            assert_eq!(
                canonical.parse::<f64>().unwrap().to_bits(),
                bits,
                "{source}"
            );
            assert_eq!(float_decimal(Role::F64, &canonical), Some(canonical));
        }
        assert_eq!(float_decimal(Role::F32, "1.5E+2"), Some("150.0".into()));
        assert!(float_decimal(Role::F32, "3.5e38").is_none());
        assert!(float_decimal(Role::F64, "1.8e308").is_none());
    }

    #[test]
    fn float_large_coefficient_and_exponent_cancel_before_rounding() {
        let zeros = "0".repeat(700_000);
        for role in [Role::F32, Role::F64] {
            for (source, expected) in [
                (format!("1{zeros}.0e-700000"), "1.0"),
                (format!("-0.{zeros}1e700001"), "-1.0"),
                (
                    format!("-{zeros}.0e999999999999999999999999999999999999999999999999"),
                    "-0.0",
                ),
            ] {
                assert_eq!(float_decimal(role, &source).as_deref(), Some(expected));
            }
        }
        let ones = "1".repeat(700_000);
        assert_eq!(
            float_decimal(Role::F32, &format!("1{ones}.0e-700000")),
            float_decimal(Role::F32, &format!("1.{ones}e0"))
        );
        let above_tie = format!("1.000000059604644775390625{zeros}1");
        assert_eq!(
            float_decimal(Role::F32, &above_tie)
                .unwrap()
                .parse::<f64>()
                .unwrap(),
            f64::from(f32::from_bits(0x3f800001))
        );
    }
}
