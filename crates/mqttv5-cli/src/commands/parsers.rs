use anyhow::Result;

pub fn parse_duration_secs(s: &str) -> Result<u64, String> {
    if let Ok(secs) = s.parse::<u64>() {
        return Ok(secs);
    }
    humantime::parse_duration(s)
        .map(|d| d.as_secs())
        .map_err(|e| e.to_string())
}

#[allow(clippy::cast_possible_truncation)]
pub fn parse_duration_millis(s: &str) -> Result<u64, String> {
    if let Ok(secs) = s.parse::<u64>() {
        return Ok(secs * 1000);
    }
    humantime::parse_duration(s)
        .map(|d| d.as_millis() as u64)
        .map_err(|e| e.to_string())
}

#[allow(clippy::cast_sign_loss)]
pub fn calculate_wait_until(time_str: &str) -> Result<std::time::Duration> {
    use time::macros::format_description;
    use time::{OffsetDateTime, PrimitiveDateTime, Time};

    let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());

    let datetime_format = format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]");
    if let Ok(target) = PrimitiveDateTime::parse(time_str, datetime_format) {
        let target = target.assume_offset(now.offset());
        let duration = target - now;
        if duration.is_negative() {
            anyhow::bail!("Scheduled time '{time_str}' is in the past");
        }
        return Ok(std::time::Duration::from_secs(
            duration.whole_seconds() as u64
        ));
    }

    let format_hms = format_description!("[hour]:[minute]:[second]");
    let format_hm = format_description!("[hour]:[minute]");

    let target_time = Time::parse(time_str, format_hms)
        .or_else(|_| Time::parse(time_str, format_hm))
        .ok();

    if let Some(target_time) = target_time {
        let today = now.date();
        let target_datetime = today.with_time(target_time).assume_offset(now.offset());

        let duration = if target_datetime > now {
            target_datetime - now
        } else {
            let tomorrow = today
                .next_day()
                .ok_or_else(|| anyhow::anyhow!("Cannot schedule for tomorrow (date overflow)"))?;
            tomorrow.with_time(target_time).assume_offset(now.offset()) - now
        };

        return Ok(std::time::Duration::from_secs(
            duration.whole_seconds() as u64
        ));
    }

    anyhow::bail!(
        "Invalid time format '{time_str}'. Use HH:MM, HH:MM:SS, or ISO 8601 (2025-01-15T14:30:00)"
    );
}

/// Converts u64 seconds to u32 for MQTT protocol fields, saturating at `u32::MAX`.
/// MQTT session expiry and will delay are u32 seconds (~136 years max).
/// Values exceeding `u32::MAX` are clamped to `u32::MAX` rather than wrapping.
#[allow(clippy::cast_possible_truncation)]
pub const fn duration_secs_to_u32(secs: u64) -> u32 {
    if secs > u32::MAX as u64 {
        u32::MAX
    } else {
        secs as u32
    }
}
