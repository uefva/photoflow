//! 时间格式化（不引第三方依赖）。
//!
//! 用到三处：归档日志的月份与每行时间戳、体检里文件的日期。
//! 一律按**本地时间**——Python 版用的是 `datetime.now()` / `fromtimestamp()`，
//! 差 8 小时会让体检里照片的日期整体偏移一天，看着像 bug。
//!
//! 本地偏移只在 Windows 上通过 `GetTimeZoneInformation` 取；其他平台按 UTC
//! （开发和 CI 都在 Linux 容器里，那里不需要本地时区）。

use std::time::{SystemTime, UNIX_EPOCH};

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64
}

/// 本地时间相对 UTC 的偏移（秒）。取不到就按 0（UTC）算。
#[cfg(windows)]
fn local_offset_secs() -> i64 {
    #[repr(C)]
    struct TimeZoneInformation {
        bias: i32,
        standard_name: [u16; 32],
        standard_date: [u16; 8],
        standard_bias: i32,
        daylight_name: [u16; 32],
        daylight_date: [u16; 8],
        daylight_bias: i32,
    }

    extern "system" {
        fn GetTimeZoneInformation(tzi: *mut TimeZoneInformation) -> u32;
    }

    let mut tzi: TimeZoneInformation = unsafe { std::mem::zeroed() };
    let id = unsafe { GetTimeZoneInformation(&mut tzi) };
    // 2 = TIME_ZONE_ID_DAYLIGHT：夏令时期间要加上 daylight_bias
    let bias = if id == 2 {
        tzi.bias + tzi.daylight_bias
    } else {
        tzi.bias
    };
    -(bias as i64) * 60
}

#[cfg(not(windows))]
fn local_offset_secs() -> i64 {
    0
}

fn local_secs() -> i64 {
    now_secs() + local_offset_secs()
}

pub fn current_month() -> String {
    let (y, m, _) = civil_from_days(local_secs() / 86_400);
    format!("{:04}-{:02}", y, m)
}

pub fn current_datetime() -> String {
    let secs = local_secs();
    let (y, m, d) = civil_from_days(secs / 86_400);
    let rem = secs % 86_400;
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        y,
        m,
        d,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// 把文件修改时间格式成 `YYYY-MM-DD`（本地时间）。
pub fn date_of(t: SystemTime) -> String {
    let secs = t
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
        + local_offset_secs();
    let (y, m, d) = civil_from_days(secs / 86_400);
    format!("{:04}-{:02}-{:02}", y, m, d)
}

/// Howard Hinnant 的 civil_from_days：把「1970-01-01 起的天数」转成 (年, 月, 日)。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_783), (2024, 3, 1)); // 2024 是闰年
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert_eq!(civil_from_days(20_730), (2026, 10, 4)); // 2026-10-01 是第 20727 天
    }

    #[test]
    fn formats_look_right() {
        let m = current_month();
        assert_eq!(m.len(), 7, "{m}");
        assert_eq!(&m[4..5], "-", "{m}");

        let dt = current_datetime();
        assert_eq!(dt.len(), 19, "{dt}");

        let d = date_of(SystemTime::UNIX_EPOCH);
        assert_eq!(d.len(), 10, "{d}");
    }
}
