use std::ffi::CString;
use std::path::Path;

use chrono::{Datelike, Local, TimeZone, Timelike};
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};

use crate::i18n::Lang;

const QUERY_SET: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

pub fn fmt_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    let rendered = if value >= 100.0 {
        format!("{value:.0}")
    } else if value >= 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.2}")
    };
    format!("{rendered} {}", UNITS[unit])
}

pub fn fmt_pct(value: f64) -> String {
    if !value.is_finite() {
        return "—".into();
    }
    if value >= 100.0 {
        format!("{value:.0}%")
    } else {
        format!("{value:.1}%")
    }
}

pub fn percent(used: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (used as f64 / total as f64 * 100.0).clamp(0.0, 100.0)
    }
}

pub fn fmt_duration(lang: Lang, total_secs: u64) -> String {
    let days = total_secs / 86_400;
    let hours = (total_secs % 86_400) / 3_600;
    let minutes = (total_secs % 3_600) / 60;
    let seconds = total_secs % 60;
    let (day, hour, minute, second) = match lang {
        Lang::En => ("d", "h", "m", "s"),
        Lang::Id => ("hari", "jam", "mnt", "dtk"),
    };

    if days > 0 {
        if hours > 0 {
            format!("{days} {day} {hours} {hour}")
        } else {
            format!("{days} {day}")
        }
    } else if hours > 0 {
        if minutes > 0 {
            format!("{hours} {hour} {minutes} {minute}")
        } else {
            format!("{hours} {hour}")
        }
    } else if minutes > 0 {
        format!("{minutes} {minute} {seconds} {second}")
    } else {
        format!("{seconds} {second}")
    }
}

pub fn fmt_datetime(lang: Lang, ts: i64) -> String {
    let Some(dt) = Local.timestamp_opt(ts, 0).single() else {
        return "—".into();
    };
    const MONTHS_EN: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    const MONTHS_ID: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "Mei", "Jun", "Jul", "Agu", "Sep", "Okt", "Nov", "Des",
    ];
    let months = match lang {
        Lang::En => MONTHS_EN,
        Lang::Id => MONTHS_ID,
    };
    format!(
        "{:02} {} {} {:02}:{:02}:{:02}",
        dt.day(),
        months[(dt.month0() as usize).min(11)],
        dt.year(),
        dt.hour(),
        dt.minute(),
        dt.second()
    )
}

pub fn fmt_clock(ts: i64) -> String {
    Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|dt| format!("{:02}:{:02}", dt.hour(), dt.minute()))
        .unwrap_or_else(|| "—".into())
}

pub fn fmt_day(ts: i64) -> String {
    Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|dt| format!("{:02}/{:02}", dt.day(), dt.month()))
        .unwrap_or_else(|| "—".into())
}

pub fn time_ago(lang: Lang, ts: i64) -> String {
    let delta = now() - ts;
    if delta <= 5 {
        return match lang {
            Lang::En => "just now".into(),
            Lang::Id => "baru saja".into(),
        };
    }
    if delta < 60 {
        match lang {
            Lang::En => format!("{delta}s ago"),
            Lang::Id => format!("{delta} dtk lalu"),
        }
    } else if delta < 3_600 {
        match lang {
            Lang::En => format!("{}m ago", delta / 60),
            Lang::Id => format!("{} mnt lalu", delta / 60),
        }
    } else if delta < 86_400 {
        match lang {
            Lang::En => format!("{}h ago", delta / 3_600),
            Lang::Id => format!("{} jam lalu", delta / 3_600),
        }
    } else {
        match lang {
            Lang::En => format!("{}d ago", delta / 86_400),
            Lang::Id => format!("{} hari lalu", delta / 86_400),
        }
    }
}

pub fn short_id(id: &str) -> String {
    id.chars().take(12).collect()
}

pub fn url_encode(value: &str) -> String {
    utf8_percent_encode(value, QUERY_SET).to_string()
}

/// Membaca pemakaian filesystem host lewat statvfs. Mengembalikan (used, total).
pub fn disk_usage(path: &Path) -> (u64, u64) {
    let Ok(path) = CString::new(path.to_string_lossy().as_bytes()) else {
        return (0, 0);
    };
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(path.as_ptr(), &mut stat) };
    if rc != 0 {
        return (0, 0);
    }
    let fragment = stat.f_frsize.max(1) as u64;
    let total = stat.f_blocks as u64 * fragment;
    let free = stat.f_bfree as u64 * fragment;
    (total.saturating_sub(free), total)
}

/// Membaca hostname host. Di dalam container, `/etc/hostname` berisi nama
/// container, jadi root host yang dipasang di `/host` dicoba lebih dulu.
pub fn read_hostname(host_root: &Path) -> Option<String> {
    let candidates = [host_root.join("etc/hostname"), Path::new("/etc/hostname").to_path_buf()];
    for candidate in candidates {
        if let Ok(raw) = std::fs::read_to_string(&candidate) {
            if let Some(name) = raw.lines().next().map(str::trim).filter(|name| !name.is_empty()) {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// Membaca `PRETTY_NAME` dari os-release host.
pub fn read_os_pretty_name(host_root: &Path) -> Option<String> {
    let candidates = [
        host_root.join("etc/os-release"),
        host_root.join("usr/lib/os-release"),
        Path::new("/etc/os-release").to_path_buf(),
    ];
    for candidate in candidates {
        let Ok(raw) = std::fs::read_to_string(&candidate) else {
            continue;
        };
        for line in raw.lines() {
            if let Some(value) = line.strip_prefix("PRETTY_NAME=") {
                let value = value.trim().trim_matches('"').trim();
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

pub fn read_cpu_temp(host_root: &Path) -> Option<f64> {
    let candidates = [
        host_root.join("sys/class/thermal/thermal_zone0/temp"),
        Path::new("/sys/class/thermal/thermal_zone0/temp").to_path_buf(),
        host_root.join("sys/class/thermal/thermal_zone1/temp"),
    ];
    for candidate in candidates {
        if let Ok(raw) = std::fs::read_to_string(&candidate) {
            if let Ok(value) = raw.trim().parse::<f64>() {
                let celsius = if value > 1_000.0 { value / 1_000.0 } else { value };
                if (0.0..200.0).contains(&celsius) {
                    return Some(celsius);
                }
            }
        }
    }
    None
}

/// Menghapus escape sequence ANSI agar log container tampil bersih di HTML.
pub fn strip_ansi(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some(next) = chars.next() {
                        if next == '\u{7}' {
                            break;
                        }
                        if next == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            }
        } else if ch == '\r' {
            if chars.peek() != Some(&'\n') {
                output.push('\n');
            }
        } else {
            output.push(ch);
        }
    }
    output
}

pub fn truncate_chars(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.to_string()
    } else {
        let mut out: String = value.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}
