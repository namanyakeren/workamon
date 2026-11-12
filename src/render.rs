use anyhow::Result;
use minijinja::value::Serde;
use minijinja::{Environment, Value};

use crate::i18n::{self, Lang};
use crate::util;

pub struct Renderer {
    env: Environment<'static>,
}

impl Renderer {
    pub fn new() -> Result<Self> {
        let mut env = Environment::new();
        env.add_template("base.html", include_str!("../templates/base.html"))?;
        env.add_template("login.html", include_str!("../templates/login.html"))?;
        env.add_template("dashboard.html", include_str!("../templates/dashboard.html"))?;
        env.add_template("containers.html", include_str!("../templates/containers.html"))?;
        env.add_template("container.html", include_str!("../templates/container.html"))?;
        env.add_template("container_new.html", include_str!("../templates/container_new.html"))?;
        env.add_template("images.html", include_str!("../templates/images.html"))?;
        env.add_template("image.html", include_str!("../templates/image.html"))?;
        env.add_template("metrics.html", include_str!("../templates/metrics.html"))?;
        env.add_template("logs.html", include_str!("../templates/logs.html"))?;
        env.add_template("maintenance.html", include_str!("../templates/maintenance.html"))?;
        env.add_template("error.html", include_str!("../templates/error.html"))?;
        Ok(Self { env })
    }

    pub fn render(&self, template: &str, context: &serde_json::Value) -> Result<String> {
        let template = self.env.get_template(template)?;
        Ok(template.render(Value::from(Serde(context)))?)
    }
}

pub fn line_chart(
    points: &[(i64, f64)],
    width: u32,
    height: u32,
    y_min: Option<f64>,
    y_max: Option<f64>,
    unit: &str,
    color_class: &str,
    lang: Lang,
) -> String {
    let points: Vec<(i64, f64)> = points
        .iter()
        .copied()
        .filter(|(_, value)| value.is_finite())
        .collect();
    if points.is_empty() {
        return format!(
            "<div class=\"chart-empty\">{}</div>",
            i18n::t(lang, "no_samples_range")
        );
    }

    let pad_left = 46.0;
    let pad_right = 12.0;
    let pad_top = 14.0;
    let pad_bottom = 22.0;
    let w = width as f64;
    let h = height as f64;
    let plot_w = (w - pad_left - pad_right).max(10.0);
    let plot_h = (h - pad_top - pad_bottom).max(10.0);

    let t0 = points.first().map(|point| point.0).unwrap_or(0);
    let mut t1 = points.last().map(|point| point.0).unwrap_or(t0 + 60);
    if t1 <= t0 {
        t1 = t0 + 60;
    }

    let min_value = points.iter().map(|point| point.1).fold(f64::MAX, f64::min);
    let max_value = points.iter().map(|point| point.1).fold(f64::MIN, f64::max);
    let lo = y_min.unwrap_or(0.0).min(min_value);
    let hi = y_max.unwrap_or_else(|| nice_max(max_value)).max(max_value);
    let hi = if hi <= lo { lo + 1.0 } else { hi };

    let x = |ts: i64| pad_left + (ts - t0) as f64 / (t1 - t0) as f64 * plot_w;
    let y = |value: f64| pad_top + (1.0 - (value - lo) / (hi - lo)) * plot_h;

    let mut path = String::new();
    for (index, (ts, value)) in points.iter().enumerate() {
        let command = if index == 0 { 'M' } else { 'L' };
        path.push_str(&format!("{command}{:.2} {:.2} ", x(*ts), y(*value)));
    }

    let base_y = pad_top + plot_h;
    let mut area = format!(
        "M{:.2} {:.2} L{:.2} {:.2} ",
        x(t0),
        base_y,
        x(points[0].0),
        y(points[0].1)
    );
    for (ts, value) in points.iter().skip(1) {
        area.push_str(&format!("L{:.2} {:.2} ", x(*ts), y(*value)));
    }
    area.push_str(&format!(
        "L{:.2} {:.2} Z",
        x(points.last().map(|point| point.0).unwrap_or(t1)),
        base_y
    ));

    let mut grid = String::new();
    let ticks = 4;
    for index in 0..=ticks {
        let ratio = index as f64 / ticks as f64;
        let value = lo + (hi - lo) * ratio;
        let y_pos = y(value);
        grid.push_str(&format!(
            "<line x1=\"{pad_left:.1}\" y1=\"{y_pos:.2}\" x2=\"{:.1}\" y2=\"{y_pos:.2}\"/>",
            w - pad_right
        ));
        grid.push_str(&format!(
            "<text x=\"{:.1}\" y=\"{:.2}\" text-anchor=\"end\" dominant-baseline=\"middle\">{}</text>",
            pad_left - 8.0,
            y_pos,
            axis_label(value, unit)
        ));
    }

    let mid_ts = t0 + (t1 - t0) / 2;
    let date_format = (t1 - t0) > 86_400;
    let time_label = |ts: i64| {
        if date_format {
            util::fmt_day(ts)
        } else {
            util::fmt_clock(ts)
        }
    };
    let last = points.last().copied().unwrap_or((t0, lo));
    let last_label = axis_label(last.1, unit);
    let chart_aria = if lang == Lang::En { "chart" } else { "grafik" };

    format!(
        "<svg class=\"chart {color_class}\" viewBox=\"0 0 {width} {height}\" role=\"img\" aria-label=\"{chart_aria}\">\
         <g class=\"chart-grid\">{grid}</g>\
         <path class=\"chart-area\" d=\"{area}\"/>\
         <path class=\"chart-line\" d=\"{path}\"/>\
         <circle class=\"chart-dot\" cx=\"{:.2}\" cy=\"{:.2}\" r=\"2.6\"/>\
         <text x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"start\">{}</text>\
         <text x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"middle\">{}</text>\
         <text x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"end\">{}</text>\
         <text class=\"chart-last\" x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"end\">{last_label}</text>\
         </svg>",
        x(last.0),
        y(last.1),
        pad_left,
        h - 6.0,
        time_label(t0),
        pad_left + plot_w / 2.0,
        h - 6.0,
        time_label(mid_ts),
        w - pad_right,
        h - 6.0,
        time_label(t1),
        w - pad_right,
        pad_top + 4.0,
    )
}

fn nice_max(value: f64) -> f64 {
    if !value.is_finite() || value <= 0.0 {
        return 1.0;
    }
    let magnitude = 10f64.powf(value.log10().floor());
    let normalized = value / magnitude;
    let nice = if normalized <= 1.0 {
        1.0
    } else if normalized <= 2.0 {
        2.0
    } else if normalized <= 2.5 {
        2.5
    } else if normalized <= 5.0 {
        5.0
    } else {
        10.0
    };
    nice * magnitude
}

fn axis_label(value: f64, unit: &str) -> String {
    match unit {
        "percent" => format!("{value:.0}%"),
        "bytes" => util::fmt_bytes(value.max(0.0) as u64),
        _ => format!("{value:.0}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renders_image_detail_template() {
        let renderer = Renderer::new().expect("template harus valid");
        let context = json!({
            "active": "images",
            "page_title": "Images",
            "username": "admin",
            "version": "0.2.0",
            "db_max_mb": 10,
            "default_creds": false,
            "lang": "en",
            "lang_other": "id",
            "lang_other_short": "ID",
            "t": i18n::dict(Lang::En),
            "docker_ok": true,
            "docker_error": null,
            "flash": null,
            "reference": "nginx:latest",
            "image": {
                "id": "sha256:abc",
                "id_short": "abc",
                "reference": "nginx:latest",
                "tags": ["nginx:latest"],
                "tag_text": "nginx:latest",
                "digests": ["nginx@sha256:def"],
                "size_text": "1 MB",
                "created_text": "2026-01-01 00:00:00",
                "architecture": "amd64",
                "os": "linux",
                "author": "nginx",
                "entrypoint": "/docker-entrypoint.sh",
                "cmd": "nginx -g 'daemon off;'",
                "env": ["PATH=/usr/bin"],
                "exposed_ports": ["80/tcp"],
                "layers": 3,
                "used_by": [{"id": "abc123", "name": "web", "state_text": "running"}]
            }
        });
        let html = renderer.render("image.html", &context).expect("render");
        assert!(html.contains("nginx:latest"));
        assert!(html.contains("web"));
    }
}
