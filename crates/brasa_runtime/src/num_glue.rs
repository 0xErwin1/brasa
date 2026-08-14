//! Fixed-decimal number rendering, shared by both backends so the two
//! cannot drift (spec: 05 — Stdlib de scripting, `toFixed`).
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
/// formatter may have rounded a longer expansion up to it. For an
/// arbitrary value that happens about one time in ten, so the exact
/// expansion — which writes over a thousand digits — is reached only
/// after a cheap arithmetic filter has ruled out the impostors.
fn tie_truncated_at(magnitude: f64, digits: i64) -> Option<String> {
    let candidate = format!("{magnitude:.*}", (digits + 1) as usize);
    let truncated = candidate.strip_suffix('5')?;

    if !is_binary_multiple(magnitude, digits + 1) {
        return None;
    }

    let exact = format!("{magnitude:.EXACT_DECIMALS$}");
    let rest = exact.strip_prefix(&candidate)?;
    rest.bytes()
        .all(|b| b == b'0')
        .then(|| truncated.trim_end_matches('.').to_string())
}

/// Whether `magnitude` is an exact multiple of `2^-places`: the
/// necessary condition every tie at `places - 1` decimals satisfies,
/// cheap enough to stand in front of the full-precision expansion.
///
/// It never rejects a genuine tie. A tie at `digits` means the exact
/// value is `K / 10^(digits+1)` with `K` an odd multiple of five, while
/// a `f64` is also `n / 2^k` in lowest terms. Equating the two gives
/// `n * 2^(digits+1) * 5^(digits+1) = K * 2^k`, and `n` is odd, so
/// `k <= digits + 1` and `K = n * 2^(digits+1-k) * 5^(digits+1)`. The
/// product `magnitude * 2^(digits+1)` is therefore `K / 5^(digits+1)`,
/// which is the integer `n * 2^(digits+1-k)`.
///
/// The product is computed exactly. Scaling by a power of two only
/// shifts the exponent, and scaling up cannot underflow. Neither can it
/// overflow where this is called: `places` is at most 18, so an infinite
/// product would need a magnitude above `f64::MAX / 2^18`, and every
/// `f64` at or above `2^52` is already a whole number whose decimals are
/// all zeros — such a value never renders the trailing `5` that reaches
/// here. That unreachability is load-bearing rather than incidental,
/// because `f64::INFINITY.fract()` is `NaN` and would answer no.
fn is_binary_multiple(magnitude: f64, places: i64) -> bool {
    (magnitude * 2f64.powi(places as i32)).fract() == 0.0
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

    /// The expansion the pre-filter guards, kept here as the reference
    /// the filtered implementation is held to.
    fn tie_truncated_unfiltered(magnitude: f64, digits: i64) -> Option<String> {
        let candidate = format!("{magnitude:.*}", (digits + 1) as usize);
        let truncated = candidate.strip_suffix('5')?;

        let exact = format!("{magnitude:.EXACT_DECIMALS$}");
        let rest = exact.strip_prefix(&candidate)?;
        rest.bytes()
            .all(|b| b == b'0')
            .then(|| truncated.trim_end_matches('.').to_string())
    }

    /// SplitMix64, so the sample below is a fixed one rather than
    /// whatever a dependency would hand out today.
    fn scrambled(state: &mut u64) -> u64 {
        *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);

        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Values shaped like the ones a report formats, mixed with raw bit
    /// patterns so the sample is not only the well-behaved ones.
    fn sampled_magnitudes(count: usize) -> Vec<f64> {
        let mut state = 0x5EED_1234_ABCD_0001;
        let mut out = Vec::with_capacity(count * 5);

        for _ in 0..count {
            let raw = f64::from_bits(scrambled(&mut state) & 0x7FEF_FFFF_FFFF_FFFF);
            let dyadic = (scrambled(&mut state) % 1_000_000) as f64
                / 2f64.powi((scrambled(&mut state) % 20) as i32);
            let cents = (scrambled(&mut state) % 1_000_000) as f64 / 100.0;
            let ratio =
                (scrambled(&mut state) % 100_000) as f64 / (1 + scrambled(&mut state) % 997) as f64;
            let half = (scrambled(&mut state) % 10_000) as f64 + 0.5;

            out.extend(
                [raw, dyadic, cents, ratio, half]
                    .into_iter()
                    .filter(|m| m.is_finite()),
            );
        }

        out
    }

    /// The pre-filter in front of the full-precision expansion is an
    /// optimization only: it must agree with the unfiltered expansion on
    /// every value, at every accepted width. A rejection it gets wrong
    /// would silently round a tie the formatter's way instead of
    /// `math.round`'s.
    #[test]
    fn the_tie_prefilter_decides_exactly_what_the_expansion_decides() {
        let magnitudes = sampled_magnitudes(2_000);
        let mut ties_seen = 0;

        for magnitude in magnitudes {
            for digits in 0..=MAX_DIGITS {
                let reference = tie_truncated_unfiltered(magnitude, digits);
                ties_seen += usize::from(reference.is_some());

                assert_eq!(
                    tie_truncated_at(magnitude, digits),
                    reference,
                    "magnitude {magnitude:e} at {digits} digits"
                );
            }
        }

        assert!(
            ties_seen > 1_000,
            "the sample must actually contain ties for the agreement to mean anything, saw {ties_seen}"
        );
    }

    /// The values the filter exists to reject: their short rendering
    /// ends in `5`, so they reach the filter, and none of them is a tie.
    #[test]
    fn a_trailing_five_that_is_not_a_tie_never_reaches_the_expansion() {
        for (magnitude, digits) in [
            (2.675, 2),
            (0.615, 2),
            (9.995, 2),
            (1.005, 2),
            (333.335, 2),
            (0.1 + 0.2, 1),
            (1.0 / 3.0, 15),
        ] {
            assert!(
                !is_binary_multiple(magnitude, digits + 1),
                "{magnitude} at {digits} digits should be filtered out"
            );
            assert_eq!(tie_truncated_at(magnitude, digits), None);
        }
    }

    /// The other side of the same coin: a genuine tie must pass the
    /// filter at the width it is a tie for.
    #[test]
    fn every_exact_tie_passes_the_prefilter() {
        for (magnitude, digits) in [
            (2.5, 0),
            (0.5, 0),
            (9.5, 0),
            (0.75, 1),
            (0.125, 2),
            (0.9375, 3),
            (0.03125, 4),
            (1.52587890625e-5, 15),
            (7.62939453125e-6, 16),
            (3.814697265625e-6, 17),
        ] {
            assert!(
                is_binary_multiple(magnitude, digits + 1),
                "{magnitude} at {digits} digits is a tie and must pass"
            );
            assert!(tie_truncated_at(magnitude, digits).is_some());
        }
    }

    /// Scaling by `2^(digits+1)` would overflow to infinity for a large
    /// enough magnitude, and `f64::INFINITY.fract()` is `NaN`, which the
    /// filter reads as "not a tie". These are the magnitudes that could
    /// overflow: they are whole numbers, so they never render a trailing
    /// `5` and never reach the filter at all.
    #[test]
    fn magnitudes_large_enough_to_overflow_the_prefilter_never_reach_it() {
        for magnitude in [f64::MAX, 1.0e308, f64::MAX / 2.0, 6.9e302] {
            for digits in 0..=MAX_DIGITS {
                assert!(!format!("{magnitude:.*}", (digits + 1) as usize).ends_with('5'));
                assert_eq!(tie_truncated_at(magnitude, digits), None);
            }
        }

        assert_eq!(float_to_fixed(f64::MAX, 0), format!("{:.0}", f64::MAX));
        assert_eq!(float_to_fixed(f64::MAX, 17), format!("{:.17}", f64::MAX));
    }

    #[test]
    fn the_accepted_range_is_closed_at_both_ends() {
        assert!(digits_in_range(0));
        assert!(digits_in_range(MAX_DIGITS));
        assert!(!digits_in_range(-1));
        assert!(!digits_in_range(MAX_DIGITS + 1));
    }
}
