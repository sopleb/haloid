//! Parsing the legacy API's `expiration` parameter.

use crate::DomainError;
use std::time::Duration;

/// Seconds in each ISO-8601 designator.
///
/// Years and months are calendar-ambiguous. We define them as 365 and 30 days
/// rather than pretending to anchor them to a date: an expiry is a retention
/// preference, not an appointment.
const SEC: u64 = 1;
const MIN: u64 = 60;
const HOUR: u64 = 3_600;
const DAY: u64 = 86_400;
const WEEK: u64 = 7 * DAY;
const MONTH: u64 = 30 * DAY;
const YEAR: u64 = 365 * DAY;

/// Parse the legacy API's `expiration` parameter into a retention period.
///
/// Accepts an ISO-8601 duration (`PT5M`, `P3D`) or, undocumented but widely
/// sent, a bare integer count of seconds — the legacy API has always cast
/// all-digit values to an int, so clients rely on it.
///
/// `Ok(None)` means "keep forever". Zero maps to that rather than to immediate
/// deletion: a client sending `0` wants no expiry, and reading it the other
/// way would destroy uploads.
///
/// Values above `max` are capped rather than refused. Rejecting an upload over
/// a metadata preference would be the worse failure.
pub fn parse_expiration(input: &str, max: Duration) -> Result<Option<Duration>, DomainError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    let seconds = if trimmed.bytes().all(|b| b.is_ascii_digit()) {
        // Saturate rather than erroring on an absurd digit string; the cap
        // below turns it into the instance maximum.
        trimmed.parse::<u64>().unwrap_or(u64::MAX)
    } else {
        parse_iso8601(trimmed)?
    };

    if seconds == 0 {
        return Ok(None);
    }
    Ok(Some(Duration::from_secs(seconds).min(max)))
}

fn invalid(reason: &str) -> DomainError {
    DomainError::invalid_param("expiration", reason)
}

/// A small ISO-8601 duration parser.
///
/// Hand-rolled on purpose: the obvious crates model components as `f32`, which
/// loses precision on large second counts, and none of them take a position on
/// what a month is — which we have to decide anyway.
fn parse_iso8601(input: &str) -> Result<u64, DomainError> {
    if !input.is_ascii() {
        return Err(invalid("not an ISO-8601 duration"));
    }
    let upper = input.to_ascii_uppercase();
    let body = upper
        .strip_prefix('P')
        .ok_or_else(|| invalid("ISO-8601 durations start with P"))?;

    // The T separator decides what M means: months before it, minutes after.
    let (date_part, time_part) = match body.split_once('T') {
        Some((date, time)) => {
            if time.is_empty() {
                return Err(invalid("nothing after the T separator"));
            }
            (date, Some(time))
        }
        None => (body, None),
    };

    if date_part.is_empty() && time_part.is_none() {
        return Err(invalid("duration has no components"));
    }

    let mut total: u64 = 0;
    let mut components = 0usize;
    total = total.saturating_add(sum_components(date_part, true, &mut components)?);
    if let Some(time) = time_part {
        total = total.saturating_add(sum_components(time, false, &mut components)?);
    }

    if components == 0 {
        return Err(invalid("duration has no components"));
    }
    Ok(total)
}

fn sum_components(part: &str, is_date: bool, components: &mut usize) -> Result<u64, DomainError> {
    let mut total: u64 = 0;
    let mut digits: Option<u64> = None;

    for ch in part.chars() {
        if let Some(digit) = ch.to_digit(10) {
            digits = Some(
                digits
                    .unwrap_or(0)
                    .saturating_mul(10)
                    .saturating_add(u64::from(digit)),
            );
            continue;
        }

        let value = digits
            .take()
            .ok_or_else(|| invalid("every ISO-8601 designator needs a number before it"))?;
        let multiplier = match (ch, is_date) {
            ('Y', true) => YEAR,
            ('M', true) => MONTH,
            ('W', true) => WEEK,
            ('D', true) => DAY,
            ('H', false) => HOUR,
            ('M', false) => MIN,
            ('S', false) => SEC,
            _ => return Err(invalid("unknown ISO-8601 designator")),
        };
        total = total.saturating_add(value.saturating_mul(multiplier));
        *components += 1;
    }

    // Digits with no designator after them: "P5", "PT5M3".
    if digits.is_some() {
        return Err(invalid("trailing number with no designator"));
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    const YEAR: Duration = Duration::from_secs(365 * 86_400);

    fn parse(input: &str) -> Result<Option<Duration>, DomainError> {
        parse_expiration(input, YEAR)
    }

    fn secs(input: &str) -> u64 {
        parse(input)
            .unwrap_or_else(|e| panic!("{input:?} failed to parse: {e}"))
            .unwrap_or_else(|| panic!("{input:?} parsed as no-expiration"))
            .as_secs()
    }

    #[test]
    fn parses_the_documented_iso8601_examples() {
        // The two forms the legacy API's documentation shows.
        assert_eq!(secs("PT5M"), 300);
        assert_eq!(secs("P3D"), 259_200);
    }

    #[test]
    fn parses_bare_integer_seconds() {
        // Undocumented, but the legacy API casts all-digit values to an int, so
        // clients send them and they must work.
        assert_eq!(secs("3600"), 3600);
        assert_eq!(secs("1"), 1);
    }

    #[test]
    fn parses_each_iso8601_component() {
        assert_eq!(secs("PT30S"), 30);
        assert_eq!(secs("PT15M"), 900);
        assert_eq!(secs("PT2H"), 7200);
        assert_eq!(secs("P1D"), 86_400);
        assert_eq!(secs("P1W"), 604_800);
        // Months and years are calendar-ambiguous; we define them as 30 and
        // 365 days rather than pretending otherwise.
        assert_eq!(secs("P1M"), 30 * 86_400);
        assert_eq!(secs("P1Y"), 365 * 86_400);
    }

    #[test]
    fn parses_combined_components() {
        assert_eq!(secs("P1DT2H3M4S"), 86_400 + 7200 + 180 + 4);
        assert_eq!(secs("PT1H30M"), 5400);
    }

    #[test]
    fn is_case_insensitive_about_designators() {
        // curl users and hand-written configs produce both.
        assert_eq!(secs("pt5m"), 300);
        assert_eq!(secs("p3d"), 259_200);
    }

    #[test]
    fn zero_means_no_expiration_not_expire_immediately() {
        // The distinction that matters: a client sending 0 wants the file kept
        // forever, not deleted on arrival.
        assert_eq!(parse("0").unwrap(), None);
        assert_eq!(parse("PT0S").unwrap(), None);
        assert_eq!(parse("P0D").unwrap(), None);
        assert_eq!(parse("PT0H0M0S").unwrap(), None);
    }

    #[test]
    fn absent_and_blank_mean_no_expiration() {
        assert_eq!(parse("").unwrap(), None);
        assert_eq!(parse("   ").unwrap(), None);
    }

    #[test]
    fn surrounding_whitespace_is_tolerated() {
        assert_eq!(secs("  PT5M  "), 300);
        assert_eq!(secs("\t3600\n"), 3600);
    }

    #[test]
    fn values_above_the_maximum_are_capped_not_rejected() {
        // A client asking for ten years gets the instance maximum, because
        // refusing the upload over a metadata preference would be worse.
        assert_eq!(parse("P10Y").unwrap(), Some(YEAR));
        assert_eq!(parse("999999999999").unwrap(), Some(YEAR));
    }

    #[test]
    fn rejects_negative_values() {
        for input in ["-1", "-3600", "-PT5M", "PT-5M"] {
            assert!(parse(input).is_err(), "{input:?} was accepted");
        }
    }

    #[test]
    fn rejects_malformed_input() {
        for input in [
            "garbage",
            "P",
            "PT",
            "T5M",
            "5M",
            "P5",
            "PT5",
            "P1DT",
            "PT5M3",
            "P1Z",
            "1.5",
            "0x10",
            "PT5M PT5M",
            "∞",
        ] {
            assert!(parse(input).is_err(), "{input:?} was accepted");
        }
    }

    #[test]
    fn errors_name_the_offending_field() {
        let error = parse("garbage").unwrap_err();
        match error {
            DomainError::InvalidParam { field, .. } => assert_eq!(field, "expiration"),
            other => panic!("expected InvalidParam, got {other:?}"),
        }
    }

    #[test]
    fn absurd_component_values_do_not_overflow() {
        // 99999999999999999999 years must not panic in release or debug.
        for input in ["P99999999999999999999Y", "PT99999999999999999999S"] {
            let result = parse(input);
            assert!(
                result.is_err() || result.unwrap() == Some(YEAR),
                "{input:?} neither capped nor rejected"
            );
        }
    }
}
