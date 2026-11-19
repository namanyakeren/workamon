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
        env.add_template("apps.html", include_str!("../templates/apps.html"))?;
        env.add_template("service.html", include_str!("../templates/service.html"))?;
        env.add_template("app_new.html", include_str!("../templates/app_new.html"))?;
        env.add_template("packages.html", include_str!("../templates/packages.html"))?;
        env.add_template("network.html", include_str!("../templates/network.html"))?;
        env.add_template("terminal.html", include_str!("../templates/terminal.html"))?;
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

    fn base_context(active: &str, title: &str) -> serde_json::Value {
        json!({
            "active": active,
            "page_title": title,
            "username": "admin",
            "version": "0.3.7",
            "db_max_mb": 10,
            "default_creds": false,
            "lang": "en",
            "lang_other": "id",
            "lang_other_short": "ID",
            "t": i18n::dict(Lang::En),
            "docker_ok": true,
            "docker_error": null,
            "flash": null,
        })
    }

    #[test]
    fn renders_apps_template() {
        let renderer = Renderer::new().expect("template harus valid");
        let mut context = base_context("apps", "Apps");
        context["rows"] = json!([
            {
                "kind": "service",
                "name": "nginx.service",
                "unit": "nginx.service",
                "description": "A high performance web server",
                "href": "/apps/service?unit=nginx.service",
                "status": "active",
                "status_class": "ok",
                "active": true,
                "enabled": true,
                "enabled_text": "enabled",
                "enabled_class": "ok",
                "sub_state": "running",
                "cpu_text": "1.2%",
                "mem_text": "10 MB",
                "net_text": "down 1 KB/s",
                "disk_text": "read 1 KB/s"
            },
            {
                "kind": "container",
                "name": "web",
                "id": "abc123",
                "href": "/containers/abc123",
                "description": "nginx:latest",
                "status": "running",
                "status_class": "ok",
                "running": true,
                "cpu_text": "0.5%",
                "mem_text": "20 MB",
                "net_text": "down 1 MB",
                "disk_text": "—"
            }
        ]);
        context["q"] = json!("");
        context["kind"] = json!("all");
        context["services_total"] = json!(1);
        context["services_active"] = json!(1);
        context["containers_total"] = json!(1);
        context["containers_running"] = json!(1);
        context["services_ok"] = json!(true);
        context["services_error"] = json!(null);
        context["host_ok"] = json!(true);
        context["host_error"] = json!(null);
        let html = renderer.render("apps.html", &context).expect("render");
        assert!(html.contains("nginx.service"));
        assert!(html.contains("web"));
    }

    #[test]
    fn renders_service_template() {
        let renderer = Renderer::new().expect("template harus valid");
        let mut context = base_context("apps", "Service");
        context["s"] = json!({
            "name": "nginx.service",
            "unit": "nginx.service",
            "description": "A high performance web server",
            "status": "active",
            "status_class": "ok",
            "active": true,
            "enabled": true,
            "enabled_text": "enabled",
            "enabled_class": "ok",
            "sub_state": "running",
            "unit_file_state": "enabled",
            "main_pid": 123,
            "cpu_text": "1.2%",
            "cpu_pct": 1.2,
            "mem_text": "10 MB",
            "mem_pct": 2.0,
            "net_text": "down 1 KB/s",
            "disk_text": "read 1 KB/s"
        });
        context["unit"] = json!("nginx.service");
        context["logs"] = json!("nginx started");
        context["host_ok"] = json!(true);
        context["host_error"] = json!(null);
        context["services_ok"] = json!(true);
        context["services_error"] = json!(null);
        let html = renderer.render("service.html", &context).expect("render");
        assert!(html.contains("nginx started"));
    }

    #[test]
    fn renders_network_template() {
        let renderer = Renderer::new().expect("template harus valid");
        let mut context = base_context("network", "Network");
        context["network"] = json!({
            "rx_rate_text": "1 MB/s",
            "tx_rate_text": "2 MB/s",
            "rx_total_text": "10 GB",
            "tx_total_text": "20 GB",
            "default_gateway": "192.168.1.1",
            "dns_text": "1.1.1.1",
            "interface_count": 1
        });
        context["interfaces"] = json!([{
            "name": "eth0",
            "state": "up",
            "state_class": "ok",
            "addresses": "192.168.1.2/24",
            "rx_text": "10 GB",
            "tx_text": "20 GB",
            "rx_rate_text": "1 MB/s",
            "tx_rate_text": "2 MB/s"
        }]);
        context["cloudflare"] = json!([{
            "kind": "service",
            "name": "cloudflared.service",
            "description": "Cloudflare Tunnel",
            "state": "active",
            "state_class": "ok",
            "cpu_text": "0.1%",
            "mem_text": "30 MB",
            "net_text": "down 1 KB/s"
        }]);
        context["rx_chart"] = json!("<svg></svg>");
        context["tx_chart"] = json!("<svg></svg>");
        context["network_ok"] = json!(true);
        context["network_error"] = json!(null);
        context["host_ok"] = json!(true);
        context["host_error"] = json!(null);
        let html = renderer.render("network.html", &context).expect("render");
        assert!(html.contains("eth0"));
        assert!(html.contains("cloudflared.service"));
    }

    #[test]
    fn renders_packages_template() {
        let renderer = Renderer::new().expect("template harus valid");
        let mut context = base_context("apps", "Packages");
        context["rows"] = json!([{
            "name": "nginx",
            "version": "1.24.0",
            "architecture": "arm64",
            "summary": "Web server"
        }]);
        context["q"] = json!("");
        context["total"] = json!(1);
        context["page"] = json!(1);
        context["total_pages"] = json!(1);
        context["prev_link"] = json!(null);
        context["next_link"] = json!(null);
        context["host_ok"] = json!(true);
        context["host_error"] = json!(null);
        context["error"] = json!(null);
        let html = renderer.render("packages.html", &context).expect("render");
        assert!(html.contains("nginx"));
    }

    #[test]
    fn renders_terminal_template() {
        let renderer = Renderer::new().expect("template harus valid");
        let mut context = base_context("terminal", "Terminal");
        context["host_ok"] = json!(true);
        context["host_error"] = json!(null);
        let html = renderer.render("terminal.html", &context).expect("render");
        assert!(html.contains("xterm.js"));
    }

    #[test]
    fn renders_app_new_template() {
        let renderer = Renderer::new().expect("template harus valid");
        let mut context = base_context("apps", "Install");
        context["catalog"] = json!([{
            "package": "nginx",
            "title": "nginx",
            "description": "Web server",
            "service": "nginx.service",
            "port": "80"
        }]);
        context["host_ok"] = json!(true);
        context["host_error"] = json!(null);
        let html = renderer.render("app_new.html", &context).expect("render");
        assert!(html.contains("nginx.service"));
    }

    #[test]
    fn renders_image_detail_template() {
        let renderer = Renderer::new().expect("template harus valid");
        let context = json!({
            "active": "images",
            "page_title": "Images",
            "username": "admin",
            "version": "0.3.7",
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
