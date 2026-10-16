//! Parse visible filter fields independently of scan exclusion policy.
use space_core::Timestamp;
use space_store::QueryFilter;

#[derive(Clone, Default, Debug)]
pub struct Fields {
    pub name: String,
    pub path: String,
    pub extension: String,
    pub min_size: String,
    pub max_size: String,
    pub modified_after: String,
    pub modified_before: String,
    pub include_unknown_size: bool,
    pub include_unknown_time: bool,
}
fn literal(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}
fn bytes(value: &str) -> Result<Option<u64>, String> {
    if value.trim().is_empty() {
        return Ok(None);
    }
    let value = value
        .trim()
        .parse::<u64>()
        .map_err(|_| "Size must be a whole nonnegative byte count.")?;
    if value > i64::MAX as u64 {
        return Err("Size exceeds the supported SQLite integer range.".into());
    }
    Ok(Some(value))
}
fn date(value: &str, upper: bool) -> Result<Option<Timestamp>, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let time = if value.len() == 10 {
        let date = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .map_err(|_| "Date must be YYYY-MM-DD or RFC3339 with timezone.")?;
        if upper {
            date.and_hms_nano_opt(23, 59, 59, 999_999_999)
        } else {
            date.and_hms_opt(0, 0, 0)
        }
        .ok_or("Invalid calendar date.")?
        .and_utc()
    } else {
        chrono::DateTime::parse_from_rfc3339(value)
            .map_err(|_| "Date must be YYYY-MM-DD or RFC3339 with timezone.")?
            .to_utc()
    };
    Timestamp::new(time.timestamp(), time.timestamp_subsec_nanos())
        .map(Some)
        .map_err(|e| e.to_string())
}
impl Fields {
    pub fn apply_to(&self, applied: &mut QueryFilter) -> Result<(), String> {
        let next = self.parse()?;
        *applied = next;
        Ok(())
    }
    pub fn parse(&self) -> Result<QueryFilter, String> {
        if [&self.name, &self.path, &self.extension]
            .iter()
            .any(|value| value.len() > 4096)
        {
            return Err(
                "Name, path and extension filters must each be at most 4096 UTF-8 bytes.".into(),
            );
        }
        let min_size = bytes(&self.min_size)?;
        let max_size = bytes(&self.max_size)?;
        if min_size.zip(max_size).is_some_and(|(a, b)| a > b) {
            return Err("Minimum size exceeds maximum size.".into());
        }
        let modified_after = date(&self.modified_after, false)?;
        let modified_before = date(&self.modified_before, true)?;
        if modified_after
            .zip(modified_before)
            .is_some_and(|(a, b)| (a.seconds, a.nanos) > (b.seconds, b.nanos))
        {
            return Err("Modification date range is reversed.".into());
        }
        Ok(QueryFilter {
            name: literal(&self.name),
            path: literal(&self.path),
            extension: literal(self.extension.trim().trim_start_matches('.')),
            min_size,
            max_size,
            modified_after,
            modified_before,
            include_unknown_size: self.include_unknown_size,
            include_unknown_time: self.include_unknown_time,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ranges_and_literal_text_are_not_silently_reinterpreted() {
        let f = Fields {
            name: " a%_ ".into(),
            extension: ".RS".into(),
            min_size: "10".into(),
            max_size: "2".into(),
            ..Fields::default()
        };
        assert!(f.parse().is_err());
        let p = Fields {
            max_size: "20".into(),
            ..f
        }
        .parse()
        .unwrap();
        assert_eq!(p.name.as_deref(), Some(" a%_ "));
        assert_eq!(p.extension.as_deref(), Some("RS"));
    }
    #[test]
    fn utc_dates_are_inclusive_and_timezone_aware() {
        let lower = date("2026-10-16", false).unwrap().unwrap();
        let upper = date("2026-10-16", true).unwrap().unwrap();
        assert_eq!(upper.seconds - lower.seconds, 86399);
        assert_eq!(upper.nanos, 999_999_999);
        assert_eq!(
            date("2026-10-16T01:00:00+01:00", false).unwrap(),
            Some(lower)
        );
        assert!(date("2026-02-30", false).is_err());
        assert!(date("2026-10-16T12:00:00", false).is_err());
    }
    #[test]
    fn invalid_date_and_unknown_toggle_preserve_applied_configuration() {
        let mut applied = QueryFilter {
            name: Some("kept".into()),
            ..Default::default()
        };
        let original = applied.clone();
        let draft = Fields {
            modified_after: "invalid".into(),
            include_unknown_size: true,
            include_unknown_time: true,
            ..Default::default()
        };
        assert!(draft.apply_to(&mut applied).is_err());
        assert_eq!(applied, original);
    }
    #[test]
    fn text_limits_are_utf8_bytes_before_applying() {
        for text in ["a".repeat(4096), "é".repeat(2048)] {
            assert!(
                Fields {
                    name: text.clone(),
                    ..Fields::default()
                }
                .parse()
                .is_ok()
            );
            assert!(
                Fields {
                    name: format!("{text}a"),
                    ..Fields::default()
                }
                .parse()
                .is_err()
            );
        }
    }
    #[test]
    fn invalid_sizes_never_become_empty_filters() {
        for bad in ["-1", "1.5", "10MB", "18446744073709551615"] {
            assert!(bytes(bad).is_err());
        }
        assert_eq!(bytes(" 0 ").unwrap(), Some(0));
    }
}
