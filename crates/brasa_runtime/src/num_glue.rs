//! Fixed-decimal number rendering, shared by both backends so the two
//! cannot drift (`docs/spec/05-stdlib.md`, `toFixed`).
//!
//! Rust's `{:.N}` formatting is the correctly-rounded rendering of the
//! exact binary value, so it is the base: a `f64` is a binary fraction
//! with a finite decimal expansion, and the formatter reads that
//! expansion rather than an approximation of it. Scaling by `10^digits`
//! and rounding the product instead — the obvious implementation —
//! introduces its own representation error and gets digits wrong:
//! `2.675 * 100.0` is exactly `267.5` even though `2.675` is really
//! `2.67499999999999982…`, so that route answers `2.68` where the
//! stored value rounds to `2.67`.
//!
//! The formatter disagrees with the rest of the stdlib in exactly one
//! place: an exact tie, which it breaks to even while `math.round`
//! breaks away from zero (`format!("{:.0}", 2.5)` is `"2"`, but
//! `math.round(2.5)` is `3.0`). Two disagreeing rounding rules in one
//! stdlib is a trap, so a tie — and only a tie — is detected and
//! corrected by incrementing the truncated decimal string, which needs
//! no float arithmetic and so cannot reintroduce the error above.

/// The largest `digits` accepted, past which the decimals carry no
/// information a `f64` can back.
pub const MAX_DIGITS: i64 = 17;

/// Enough fractional digits to write any finite `f64` exactly: the
/// smallest subnormal needs 1074, and nothing needs more.
const EXACT_DECIMALS: usize = 1080;

/// Whether `digits` is inside the accepted range. The caller reports
/// the panic, since each backend words and raises it its own way.
pub fn digits_in_range(digits: i64) -> bool {
    (0..=MAX_DIGITS).contains(&digits)
}

/// An integer with exactly `digits` decimals: the fractional part is
/// always zeros, so this is exact for every `i64` rather than going
/// through a lossy `as f64`.
pub fn int_to_fixed(v: i64, digits: i64) -> String {
    if digits == 0 {
        return v.to_string();
    }
    format!("{v}.{:0>width$}", "", width = digits as usize)
}

/// A float with exactly `digits` decimals, never in exponent form.
/// Non-finite values render as they do everywhere else (`NaN`, `inf`,
/// `-inf`): there is no decimal expansion to give them.
pub fn float_to_fixed(v: f64, digits: i64) -> String {
    if !v.is_finite() {
        return v.to_string();
    }

    let width = digits as usize;
    let magnitude = v.abs();

    let rendered = if let Some(truncated) = tie_truncated_at(magnitude, digits) {
        bump_last_digit(&truncated)
    } else {
        format!("{magnitude:.width$}")
    };

    // A rendered magnitude of zero drops the sign: a report column
    // showing `-0.00` for a tiny negative would read as a distinct
    // value rather than as zero.
    if v.is_sign_negative() && rendered.bytes().any(|b| b.is_ascii_digit() && b != b'0') {
        return format!("-{rendered}");
    }
    rendered
}

/// The magnitude truncated at `digits`, when its exact decimal
/// expansion terminates in a `5` one place further — the one case where
/// the formatter's tie-to-even rule would disagree with `math.round`.
///
/// A rendering at `digits + 1` ending in `5` is only a candidate: the
/// formatter may have rounded a longer expansion up to it. The exact
/// expansion settles it.
fn tie_truncated_at(magnitude: f64, digits: i64) -> Option<String> {
    let candidate = format!("{magnitude:.*}", (digits + 1) as usize);
    let truncated = candidate.strip_suffix('5')?;

    let exact = format!("{magnitude:.EXACT_DECIMALS$}");
    let rest = exact.strip_prefix(&candidate)?;
    rest.bytes()
        .all(|b| b == b'0')
        .then(|| truncated.trim_end_matches('.').to_string())
}

/// Adds one at the last digit of a fixed-point decimal string, carrying
/// through the decimal point and growing the integer part when every
/// digit is a nine.
fn bump_last_digit(text: &str) -> String {
    let mut digits: Vec<u8> = text.bytes().collect();

    for index in (0..digits.len()).rev() {
        match digits[index] {
            b'.' => continue,
            b'9' => digits[index] = b'0',
            digit => {
                digits[index] = digit + 1;
                return String::from_utf8(digits).expect("ascii digits stay ascii");
            }
        }
    }

    let mut carried = vec![b'1'];
    carried.extend(digits);
    String::from_utf8(carried).expect("ascii digits stay ascii")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimals_are_exactly_what_was_asked_for() {
        assert_eq!(float_to_fixed(1000.0, 2), "1000.00");
        // Really 333.33499999999997953…, so the correctly rounded
        // answer is down. The obvious scale-then-round implementation
        // answers "333.34" here, which is what this pins against.
        assert_eq!(float_to_fixed(333.335, 2), "333.33");
        assert_eq!(float_to_fixed(2.675, 2), "2.67");
        assert_eq!(float_to_fixed(0.615, 2), "0.61");
        assert_eq!(float_to_fixed(1.005, 2), "1.00");
        assert_eq!(float_to_fixed(0.5, 2), "0.50");
        assert_eq!(float_to_fixed(12.1, 2), "12.10");
        assert_eq!(float_to_fixed(1.0, 0), "1");
        assert_eq!(float_to_fixed(0.0, 3), "0.000");
    }

    /// The whole reason this does not defer to `{:.N}`: that rounds
    /// ties to even, and `math.round` does not.
    #[test]
    fn ties_round_away_from_zero_like_math_round() {
        assert_eq!(float_to_fixed(2.5, 0), "3");
        assert_eq!(float_to_fixed(3.5, 0), "4");
        assert_eq!(float_to_fixed(0.5, 0), "1");
        assert_eq!(float_to_fixed(-2.5, 0), "-3");
        assert_eq!(float_to_fixed(0.125, 2), "0.13");

        assert_eq!(format!("{:.0}", 2.5f64), "2", "the rule being avoided");
    }

    /// Correcting a tie carries like ordinary addition, including when
    /// every digit it passes is a nine.
    ///
    /// The carry can only run through the integer part: an exact tie is
    /// a binary fraction, and a fractional `…95` that terminates would
    /// need a denominator divisible by 5, which no binary fraction has.
    #[test]
    fn a_corrected_tie_carries_through_the_decimal_point() {
        assert_eq!(float_to_fixed(0.75, 1), "0.8");
        assert_eq!(float_to_fixed(0.9375, 3), "0.938");
        assert_eq!(float_to_fixed(9.5, 0), "10");
        assert_eq!(float_to_fixed(99.5, 0), "100");
        assert_eq!(float_to_fixed(-9.5, 0), "-10");
    }

    /// A literal one place past the rendered width is almost never an
    /// exact tie — it is a shade under, and the correctly rounded answer
    /// goes DOWN. The scale-then-round implementation these pin against
    /// answers up on all three.
    #[test]
    fn a_near_tie_rounds_by_the_value_not_by_the_literal() {
        assert_eq!(float_to_fixed(2.675, 2), "2.67");
        assert_eq!(float_to_fixed(0.615, 2), "0.61");
        assert_eq!(float_to_fixed(9.995, 2), "9.99");
    }

    /// Every decimal shown is one the value actually holds: no
    /// fabricated zeros, and no drift at the far end of the range.
    #[test]
    fn the_decimals_shown_are_the_ones_the_value_has() {
        assert_eq!(float_to_fixed(123.456, 17), "123.45600000000000307");
        assert_eq!(
            float_to_fixed(1.0e12, 17),
            "1000000000000.00000000000000000"
        );
    }

    #[test]
    fn a_value_too_small_to_show_still_shows_a_number() {
        assert_eq!(float_to_fixed(0.000001, 2), "0.00");
        assert_eq!(float_to_fixed(-0.000001, 2), "0.00");
        assert_eq!(float_to_fixed(-0.006, 2), "-0.01");
    }

    #[test]
    fn large_magnitudes_never_reach_exponent_form() {
        assert_eq!(float_to_fixed(1.0e21, 1), "1000000000000000000000.0");
        assert!(!float_to_fixed(1.0e40, 2).contains('e'));
    }

    #[test]
    fn non_finite_values_render_as_themselves() {
        assert_eq!(float_to_fixed(f64::NAN, 2), "NaN");
        assert_eq!(float_to_fixed(f64::INFINITY, 2), "inf");
        assert_eq!(float_to_fixed(f64::NEG_INFINITY, 2), "-inf");
    }

    #[test]
    fn integers_are_exact_at_the_edges_of_i64() {
        assert_eq!(int_to_fixed(5, 2), "5.00");
        assert_eq!(int_to_fixed(-5, 0), "-5");
        assert_eq!(int_to_fixed(i64::MAX, 1), "9223372036854775807.0");
        assert_eq!(int_to_fixed(i64::MIN, 1), "-9223372036854775808.0");
    }

    #[test]
    fn the_accepted_range_is_closed_at_both_ends() {
        assert!(digits_in_range(0));
        assert!(digits_in_range(MAX_DIGITS));
        assert!(!digits_in_range(-1));
        assert!(!digits_in_range(MAX_DIGITS + 1));
    }
}
