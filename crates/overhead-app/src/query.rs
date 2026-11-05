//! What the user tells us about when and where the recording was made, and
//! how much that narrows the ADS-B search.

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Date {
    pub y: i32,
    pub m: u32,
    pub d: u32,
}

impl Date {
    pub fn parse(s: &str) -> Option<Date> {
        let s = s.trim();
        let mut it = s.split(['-', '.', '/']);
        let (a, b, c) = (it.next()?, it.next()?, it.next()?);
        if it.next().is_some() {
            return None;
        }
        // YYYY-MM-DD, or DD.MM.YYYY as written in Poland.
        let (y, m, d) = if a.len() == 4 { (a, b, c) } else if c.len() == 4 { (c, b, a) } else { return None };
        let (y, m, d): (i32, u32, u32) = (y.parse().ok()?, m.parse().ok()?, d.parse().ok()?);
        if !(1..=12).contains(&m) || d == 0 || d > days_in_month(y, m) || !(2000..=2100).contains(&y) {
            return None;
        }
        Some(Date { y, m, d })
    }

    /// Days since 1970-01-01.
    pub fn days(&self) -> i64 {
        // Howard Hinnant's days_from_civil.
        let y = if self.m <= 2 { self.y - 1 } else { self.y } as i64;
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = y - era * 400;
        let m = self.m as i64;
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + self.d as i64 - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    pub fn iso(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.y, self.m, self.d)
    }

    pub fn weekday(&self) -> &'static str {
        ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][self.days().rem_euclid(7) as usize]
    }

    pub fn weekday_long(&self) -> &'static str {
        ["Thursday", "Friday", "Saturday", "Sunday", "Monday", "Tuesday", "Wednesday"][self.days().rem_euclid(7) as usize]
    }
}

fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// "HH:MM" or "HH:MM:SS" (or "HH") -> seconds of day.
pub fn parse_clock(s: &str) -> Option<u32> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() > 3 {
        return None;
    }
    let mut v = [0u32; 3];
    for (i, p) in parts.iter().enumerate() {
        if p.is_empty() || p.len() > 2 {
            return None;
        }
        v[i] = p.parse().ok()?;
    }
    if v[0] > 23 || v[1] > 59 || v[2] > 59 {
        return None;
    }
    Some(v[0] * 3600 + v[1] * 60 + v[2])
}

/// "+01:00", "-5", "UTC", "UTC+2" -> minutes east of UTC.
pub fn parse_offset(s: &str) -> Option<i32> {
    let s = s.trim();
    let s = s.strip_prefix("UTC").or_else(|| s.strip_prefix("GMT")).unwrap_or(s).trim();
    if s.is_empty() {
        return Some(0);
    }
    let (sign, rest) = match s.as_bytes()[0] {
        b'+' => (1, &s[1..]),
        b'-' => (-1, &s[1..]),
        _ => (1, s),
    };
    let (h, m) = match rest.split_once(':') {
        Some((h, m)) => (h.parse::<i32>().ok()?, m.parse::<i32>().ok()?),
        None => (rest.parse::<i32>().ok()?, 0),
    };
    if h > 14 || m > 59 {
        return None;
    }
    Some(sign * (h * 60 + m))
}

pub fn format_offset(min: i32) -> String {
    let sign = if min < 0 { '-' } else { '+' };
    format!("UTC{sign}{:02}:{:02}", min.abs() / 60, min.abs() % 60)
}

/// UTC offset (minutes east) of this computer's time zone at a local date and
/// clock time, so a summer recording gets summer time even when the app runs
/// in winter. Falls back to the current offset if the date is invalid.
pub fn local_offset_on(day: Date, sec_of_day: u32) -> i32 {
    use chrono::{Local, NaiveDate, Offset, TimeZone};
    let t = NaiveDate::from_ymd_opt(day.y, day.m, day.d)
        .and_then(|d| d.and_hms_opt(sec_of_day / 3600, sec_of_day / 60 % 60, sec_of_day % 60));
    match t.and_then(|t| Local.from_local_datetime(&t).earliest()) {
        Some(t) => t.offset().fix().local_minus_utc() / 60,
        None => local_offset_now(),
    }
}

/// The system's current UTC offset in minutes.
pub fn local_offset_now() -> i32 {
    use chrono::{Local, Offset};
    Local::now().offset().fix().local_minus_utc() / 60
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeMode {
    Unknown,
    Exact,
    Approx,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TimeHint {
    Unknown,
    /// Local clock time of the first sample (seconds of day).
    ExactStart(u32),
    /// Recording happened somewhere inside these local hours.
    Window { from: u32, to: u32 },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RegionHint {
    pub city: String,
    pub state: String,
    pub country: String,
}

impl RegionHint {
    pub fn finest(&self) -> Option<(&'static str, &str)> {
        if !self.city.trim().is_empty() {
            Some(("city", self.city.trim()))
        } else if !self.state.trim().is_empty() {
            Some(("state", self.state.trim()))
        } else if !self.country.trim().is_empty() {
            Some(("country", self.country.trim()))
        } else {
            None
        }
    }

    /// "city, state, country" (the parts given), as one place-name query.
    pub fn label(&self) -> String {
        [self.city.trim(), self.state.trim(), self.country.trim()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Everything the ADS-B stage needs besides the detections.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchQuery {
    pub day: Date,
    /// Last day of a 2–3 day range (inclusive).
    pub day_end: Option<Date>,
    pub time: TimeHint,
    pub utc_offset_min: i32,
    /// True when the offset was not typed by the user but taken from this
    /// computer's time zone on the recording day: the matcher should allow
    /// an hour or two of slack for it.
    pub utc_offset_guessed: bool,
    /// What the user typed in the UTC field: an offset ("+02:00") or an IANA
    /// zone ("Europe/Warsaw"). `None` = automatic: the ADS-B search then takes
    /// the zone(s) of the region on that day from the time zone database.
    pub tz: Option<String>,
    pub region: RegionHint,
    pub duration_s: f64,
}

#[allow(dead_code)] // used by the ADS-B source
impl SearchQuery {
    pub fn days(&self) -> i64 {
        self.day_end.map_or(1, |e| e.days() - self.day.days() + 1)
    }

    pub fn span_label(&self) -> String {
        match self.day_end {
            Some(e) => format!("{} – {}", self.day.iso(), e.iso()),
            None => format!("{} {}", self.day.weekday(), self.day.iso()),
        }
    }
}

/// First day in the adsb.lol history (`globe_history_2023`, tag
/// `v2023.03.10-planes-readsb-prod-0`; checked 2026-11-04).
pub const ARCHIVE_FIRST_DAY: Date = Date { y: 2023, m: 3, d: 10 };

/// Validation of the day fields: Ok(query days) or a message.
pub fn validate_days(day: &str, day_end: &str) -> Result<(Date, Option<Date>), String> {
    if day.trim().is_empty() {
        return Err("Recording day is required".into());
    }
    let d0 = Date::parse(day).ok_or("Use YYYY-MM-DD, e.g. 2025-06-14")?;
    if d0.days() < ARCHIVE_FIRST_DAY.days() {
        return Err(format!("No aircraft data before {}", ARCHIVE_FIRST_DAY.iso()));
    }
    if day_end.trim().is_empty() {
        return Ok((d0, None));
    }
    let d1 = Date::parse(day_end).ok_or("End day: use YYYY-MM-DD")?;
    let n = d1.days() - d0.days() + 1;
    if n < 1 {
        return Err("End day is before the first day".into());
    }
    if n > 3 {
        return Err(format!("At most 3 days ({n} given)"));
    }
    Ok((d0, if n == 1 { None } else { Some(d1) }))
}

/// Rough size of the ADS-B search, for the "more info = faster" meter.
pub struct SearchSpace {
    #[allow(dead_code)]
    pub hours: f64,
    #[allow(dead_code)]
    pub area_km2: f64,
    pub hours_label: String,
    pub area_label: String,
    /// 0 (nothing known) ..= 1 (day, exact time and city).
    pub precision: f64,
    /// How many times smaller than the "day only" search.
    pub speedup: f64,
}

pub const AREA_COVERAGE_KM2: f64 = 10_000_000.0;
pub const AREA_COUNTRY_KM2: f64 = 300_000.0;
pub const AREA_STATE_KM2: f64 = 30_000.0;
pub const AREA_CITY_KM2: f64 = 2_000.0;

pub fn search_space(days: i64, time: TimeHint, region: &RegionHint, duration_s: f64) -> SearchSpace {
    let rec_h = (duration_s / 3600.0).max(0.1);
    let (hours, hours_label) = match time {
        TimeHint::Unknown => (24.0 * days as f64, format!("{} h (whole day{})", 24 * days, if days > 1 { "s" } else { "" })),
        TimeHint::ExactStart(_) => (rec_h + 0.2, format!("{:.1} h (recording ± 6 min)", rec_h + 0.2)),
        TimeHint::Window { from, to } => {
            let span = if to > from { (to - from) as f64 / 3600.0 } else { 24.0 - (from - to) as f64 / 3600.0 };
            let h = (span + rec_h) * days as f64;
            (h, format!("{h:.0} h (window{})", if days > 1 { " × days" } else { "" }))
        }
    };
    let (area_km2, area_label) = match region.finest() {
        None => (AREA_COVERAGE_KM2, "all of adsb.lol coverage".to_string()),
        Some(("city", _)) => (AREA_CITY_KM2, "~2 000 km² (city ± 25 km)".to_string()),
        Some(("state", _)) => (AREA_STATE_KM2, "~30 000 km² (region)".to_string()),
        Some(_) => (AREA_COUNTRY_KM2, "~300 000 km² (country)".to_string()),
    };
    let worst = 24.0 * 3.0 * AREA_COVERAGE_KM2;
    let best = (rec_h + 0.2) * AREA_CITY_KM2;
    let work = hours * area_km2;
    let precision = (1.0 - (work / best).ln() / (worst / best).ln()).clamp(0.0, 1.0);
    let speedup = (24.0 * AREA_COVERAGE_KM2) / work;
    SearchSpace { hours, area_km2, hours_label, area_label, precision, speedup }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(Date::parse("2025-06-14"), Some(Date { y: 2025, m: 6, d: 14 }));
        assert_eq!(Date::parse("14.06.2025"), Some(Date { y: 2025, m: 6, d: 14 }));
        assert_eq!(Date::parse("2025-02-29"), None);
        assert_eq!(Date::parse("2024-02-29").map(|d| d.days()), Some(19782));
        assert_eq!(Date { y: 1970, m: 1, d: 1 }.weekday(), "Thu");
        assert_eq!(Date { y: 2026, m: 11, d: 3 }.weekday(), "Tue");
    }

    #[test]
    fn ranges() {
        assert!(validate_days("", "").is_err());
        assert!(validate_days("2025-06-14", "2025-06-16").is_ok());
        assert!(validate_days("2025-06-14", "2025-06-17").is_err());
        assert!(validate_days("2025-06-14", "2025-06-13").is_err());
        assert!(validate_days("2022-08-19", "").is_err());
        assert!(validate_days("2023-03-09", "2023-03-11").is_err());
        assert!(validate_days("2023-03-10", "").is_ok());
    }

    #[test]
    fn clock_and_offset() {
        assert_eq!(parse_clock("14:05"), Some(14 * 3600 + 300));
        assert_eq!(parse_clock("7"), Some(7 * 3600));
        assert_eq!(parse_clock("24:00"), None);
        assert_eq!(parse_offset("+01:00"), Some(60));
        assert_eq!(parse_offset("UTC-5"), Some(-300));
        assert_eq!(parse_offset("UTC"), Some(0));
    }

    #[cfg(unix)]
    #[test]
    fn offset_follows_the_recording_day() {
        // Only meaningful where the zone has DST; the CI/dev box is Europe/Warsaw.
        // SAFETY: setting TZ before any other thread reads the environment in this test.
        unsafe { std::env::set_var("TZ", "Europe/Warsaw") };
        assert_eq!(local_offset_on(Date { y: 2025, m: 6, d: 14 }, 12 * 3600), 120);
        assert_eq!(local_offset_on(Date { y: 2025, m: 12, d: 14 }, 12 * 3600), 60);
        // DST starts at 02:00 local on 2025-03-30.
        assert_eq!(local_offset_on(Date { y: 2025, m: 3, d: 30 }, 3600), 60);
        assert_eq!(local_offset_on(Date { y: 2025, m: 3, d: 30 }, 4 * 3600), 120);
    }

    #[test]
    fn more_info_is_more_precise() {
        let r0 = RegionHint::default();
        let r1 = RegionHint { city: "Kraków".into(), ..Default::default() };
        let a = search_space(1, TimeHint::Unknown, &r0, 3600.0);
        let b = search_space(1, TimeHint::Window { from: 14 * 3600, to: 17 * 3600 }, &r0, 3600.0);
        let c = search_space(1, TimeHint::ExactStart(14 * 3600), &r1, 3600.0);
        assert!(a.precision < b.precision && b.precision < c.precision);
        assert!(c.speedup > 1000.0);
    }
}
