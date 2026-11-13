use serde_json::Value;

pub fn unix_millis_iso(millis: i64) -> Option<String> {
    let seconds = millis.div_euclid(1000);
    let days = seconds.div_euclid(86400);
    let seconds = seconds.rem_euclid(86400);
    let z = days.checked_add(719468)?;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = year + i64::from(month <= 2);
    if !(0..=9999).contains(&year) {
        return None;
    }
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        seconds / 3600,
        seconds % 3600 / 60,
        seconds % 60,
        millis.rem_euclid(1000)
    ))
}

pub fn parse_reset_time(value: &Value) -> Option<String> {
    if let Some(s) = value.as_str() {
        if s.is_empty() {
            return None;
        }
        if s.bytes().all(|b| b.is_ascii_digit()) {
            return s.parse::<i64>().ok().and_then(|n| {
                unix_millis_iso(if n < 1_000_000_000_000 {
                    n.checked_mul(1000)?
                } else {
                    n
                })
            });
        }
        return parse_iso(s);
    }
    let n = value.as_i64()?;
    if n == 0 {
        return None;
    }
    unix_millis_iso(if n < 1_000_000_000_000 {
        n.checked_mul(1000)?
    } else {
        n
    })
}

fn parse_iso(s: &str) -> Option<String> {
    let b = s.as_bytes();
    if b.len() < 10 || b.get(4) != Some(&b'-') || b.get(7) != Some(&b'-') {
        return None;
    }
    let n = |a: usize, z: usize| -> Option<i64> {
        std::str::from_utf8(b.get(a..z)?).ok()?.parse().ok()
    };
    let year = n(0, 4)?;
    let month = n(5, 7)?;
    let day = n(8, 10)?;
    if !(1..=12).contains(&month) {
        return None;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let max = match month {
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if day < 1 || day > max {
        return None;
    }
    let mut hour = 0;
    let mut minute = 0;
    let mut second = 0;
    let mut milli = 0;
    let mut offset = 0;
    if b.len() > 10 {
        if b.len() < 19 || b[10] != b'T' && b[10] != b' ' || b[13] != b':' || b[16] != b':' {
            return None;
        }
        hour = n(11, 13)?;
        minute = n(14, 16)?;
        second = n(17, 19)?;
        if hour > 23 || minute > 59 || second > 59 {
            return None;
        }
        let mut p = 19;
        if b.get(p) == Some(&b'.') {
            p += 1;
            let start = p;
            while b.get(p).is_some_and(u8::is_ascii_digit) {
                p += 1;
            }
            if p == start {
                return None;
            }
            let mut fraction = b[start..p]
                .iter()
                .take(3)
                .fold(0i64, |a, c| a * 10 + (c - b'0') as i64);
            for _ in 0..3usize.saturating_sub(p - start) {
                fraction *= 10;
            }
            milli = fraction;
        }
        if p < b.len() {
            match b[p] {
                b'Z' | b'z' if p + 1 == b.len() => {}
                b'+' | b'-' if p + 6 == b.len() && b[p + 3] == b':' => {
                    let h = n(p + 1, p + 3)?;
                    let m = n(p + 4, p + 6)?;
                    if h > 23 || m > 59 {
                        return None;
                    }
                    offset = (h * 60 + m) * 60 * if b[p] == b'+' { 1 } else { -1 };
                }
                _ => return None,
            }
        }
    }
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    unix_millis_iso(((days * 86400 + hour * 3600 + minute * 60 + second - offset) * 1000) + milli)
}

pub fn reset_epoch_millis(value: &Value) -> Option<i64> {
    let normalized = parse_reset_time(value)?;
    let b = normalized.as_bytes();
    if b.len() != 24 {
        return None;
    }
    let n = |a: usize, z: usize| -> Option<i64> {
        std::str::from_utf8(b.get(a..z)?).ok()?.parse().ok()
    };
    let year = n(0, 4)?;
    let month = n(5, 7)?;
    let day = n(8, 10)?;
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some((days * 86400 + n(11, 13)? * 3600 + n(14, 16)? * 60 + n(17, 19)?) * 1000 + n(20, 23)?)
}

pub fn to_finite_number(value: &Value, fallback: f64) -> f64 {
    value
        .as_f64()
        .or_else(|| {
            value
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .and_then(|s| s.trim().parse::<f64>().ok())
        })
        .filter(|n| n.is_finite())
        .unwrap_or(fallback)
}

pub fn normalize_cloud_code_project_id(project: &Value) -> Option<&str> {
    project
        .as_str()
        .or_else(|| project["id"].as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn finite_values_and_project_ids() {
        assert_eq!(to_finite_number(&json!(" 2.5 "), 0.0), 2.5);
        assert_eq!(to_finite_number(&json!("NaN"), 4.0), 4.0);
        assert_eq!(
            normalize_cloud_code_project_id(&json!({"id":" abc "})),
            Some("abc")
        );
        assert_eq!(normalize_cloud_code_project_id(&json!("  ")), None);
        assert_eq!(
            parse_reset_time(&json!("2026-01-02T03:04:05+07:00")).as_deref(),
            Some("2026-01-01T20:04:05.000Z")
        );
        assert_eq!(parse_reset_time(&json!("not a date")), None);
        assert_eq!(parse_reset_time(&json!("2026-02-30")), None);
    }
}
