//! 时间戳的唯一出口。
//!
//! 全项目只以 Unix 秒（`i64`）保存时间，展示时才转成本地时区的字符串。
//! 这样存储层不含时区信息，也就不会出现「换了时区，历史记录跟着变」的问题。

use chrono::{Local, TimeZone, Utc};

/// 当前时间（Unix 秒，UTC 基准）。
pub fn now_unix() -> i64 {
    Utc::now().timestamp()
}

/// 本地时区的 `YYYY-MM-DD HH:MM`。
///
/// 时间戳超出本地时区可表示的范围时返回原始秒数，而不是伪造一个日期。
pub fn format_local_minute(unix: i64) -> String {
    match Local.timestamp_opt(unix, 0).single() {
        Some(time) => time.format("%Y-%m-%d %H:%M").to_string(),
        None => format!("{unix}"),
    }
}

/// 本地时区的 `YYYY-MM-DD`。
pub fn format_local_date(unix: i64) -> String {
    match Local.timestamp_opt(unix, 0).single() {
        Some(time) => time.format("%Y-%m-%d").to_string(),
        None => format!("{unix}"),
    }
}

/// 相对当前时间的口语化描述，超过 30 天退回到日期。
pub fn format_relative(unix: i64, now: i64) -> String {
    let elapsed = now - unix;
    if elapsed < 0 {
        // 系统时钟回拨或数据来自未来：不做「-3 分钟前」这种噪音。
        return format_local_minute(unix);
    }
    match elapsed {
        0..=59 => "刚刚".to_string(),
        60..=3599 => format!("{} 分钟前", elapsed / 60),
        3600..=86_399 => format!("{} 小时前", elapsed / 3600),
        86_400..=2_592_000 => format!("{} 天前", elapsed / 86_400),
        _ => format_local_date(unix),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_buckets_are_ordered() {
        let now = 1_700_000_000;
        assert_eq!(format_relative(now, now), "刚刚");
        assert_eq!(format_relative(now - 90, now), "1 分钟前");
        assert_eq!(format_relative(now - 7_200, now), "2 小时前");
        assert_eq!(format_relative(now - 172_800, now), "2 天前");
    }

    #[test]
    fn future_timestamp_falls_back_to_absolute_time() {
        let now = 1_700_000_000;
        let text = format_relative(now + 600, now);
        assert_eq!(text, format_local_minute(now + 600));
    }
}
