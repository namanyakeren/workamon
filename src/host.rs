use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::Value;
use tokio::process::Command;

use crate::config::Config;

const MAX_OUTPUT_BYTES: usize = 128 * 1024;

/// Akses ke host melalui `nsenter`. Mode ini membutuhkan container
/// privileged dengan `pid: host`; tanpa itu, fitur systemd/apt/terminal host
/// akan melaporkan bahwa akses host dimatikan.
#[derive(Clone)]
pub struct HostAccess {
    enabled: bool,
    host_root: PathBuf,
    terminal_user: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct HostOutput {
    pub success: bool,
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ServiceLive {
    pub unit: String,
    pub description: String,
    pub load_state: String,
    pub active_state: String,
    pub sub_state: String,
    pub unit_file_state: String,
    pub main_pid: u32,
    pub control_group: String,
    pub cpu_pct: f64,
    pub mem_used: u64,
    pub mem_max: u64,
    pub disk_read: u64,
    pub disk_write: u64,
    pub net_rx: u64,
    pub net_tx: u64,
    pub disk_read_rate: f64,
    pub disk_write_rate: f64,
    pub net_rx_rate: f64,
    pub net_tx_rate: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct NetworkInterface {
    pub name: String,
    pub state: String,
    pub addresses: Vec<String>,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_rate: f64,
    pub tx_rate: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct NetworkSample {
    pub ts: i64,
    pub rx_rate: f64,
    pub tx_rate: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct NetworkSnapshot {
    pub interfaces: Vec<NetworkInterface>,
    pub rx_rate: f64,
    pub tx_rate: f64,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub default_gateway: String,
    pub dns_servers: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PackageLive {
    pub name: String,
    pub version: String,
    pub architecture: String,
    pub summary: String,
}

pub struct ServicePrev {
    at: Instant,
    cpu_nsec: u64,
    io_read: u64,
    io_write: u64,
    net_rx: u64,
    net_tx: u64,
}

pub struct NetPrev {
    at: Instant,
    rx: u64,
    tx: u64,
}

impl HostAccess {
    pub fn new(cfg: &Config) -> Self {
        Self {
            enabled: cfg.host_exec,
            host_root: cfg.resolved_host_root(),
            terminal_user: cfg.terminal_user.clone(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn disabled_message(&self) -> Option<String> {
        if self.enabled {
            None
        } else {
            Some(
                "Akses host dimatikan. Set WORKAMON_HOST_EXEC=1 dan jalankan container \
                 dengan pid: host + privileged untuk mengelola systemd, paket, dan terminal host."
                    .into(),
            )
        }
    }

    async fn run_with_env(
        &self,
        program: &str,
        args: &[&str],
        envs: &[(&str, &str)],
        timeout: Duration,
    ) -> Result<HostOutput> {
        if !self.enabled {
            bail!("akses host dimatikan (set WORKAMON_HOST_EXEC=1)");
        }

        let mut command = Command::new("nsenter");
        command.args([
            "-t", "1", "-m", "-u", "-i", "-n", "-p", "--", program,
        ]);
        command.args(args);
        for (key, value) in envs {
            command.env(key, value);
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let output = tokio::time::timeout(timeout, command.output())
            .await
            .with_context(|| format!("{program} melewati batas waktu"))??;

        Ok(HostOutput {
            success: output.status.success(),
            status: output.status.code(),
            stdout: cap_output(String::from_utf8_lossy(&output.stdout).into_owned()),
            stderr: cap_output(String::from_utf8_lossy(&output.stderr).into_owned()),
        })
    }

    async fn run(
        &self,
        program: &str,
        args: &[&str],
        timeout: Duration,
    ) -> Result<HostOutput> {
        self.run_with_env(program, args, &[], timeout).await
    }

    async fn apt(&self, args: &[&str], timeout: Duration) -> Result<HostOutput> {
        self.run_with_env(
            "apt-get",
            args,
            &[
                ("DEBIAN_FRONTEND", "noninteractive"),
                ("APT_LISTCHANGES_FRONTEND", "none"),
            ],
            timeout,
        )
        .await
    }

    pub async fn list_services(
        &self,
        previous: &mut HashMap<String, ServicePrev>,
    ) -> Result<Vec<ServiceLive>> {
        let units = self.service_units().await?;
        if units.is_empty() {
            return Ok(Vec::new());
        }

        let mut args: Vec<String> = vec![
            "show".into(),
            "--no-pager".into(),
            "--property=Id,Description,LoadState,ActiveState,SubState,UnitFileState,MainPID,ControlGroup,MemoryCurrent,MemoryMax,CPUUsageNSec,IOReadBytes,IOWriteBytes,IPIngressBytes,IPEgressBytes".into(),
            "--".into(),
        ];
        args.extend(units.iter().cloned());
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = self
            .run("systemctl", &arg_refs, Duration::from_secs(20))
            .await?;
        if !output.success {
            bail!("systemctl show gagal: {}", first_error(&output));
        }

        let now = Instant::now();
        let mut services = Vec::new();
        for block in output.stdout.split("\n\n") {
            let fields = parse_properties(block);
            let unit = fields.get("Id").cloned().unwrap_or_default();
            if unit.is_empty() || !unit.ends_with(".service") {
                continue;
            }
            let load_state = fields.get("LoadState").cloned().unwrap_or_default();
            if load_state == "not-found" {
                continue;
            }

            let cpu_nsec = parse_u64(fields.get("CPUUsageNSec"));
            let io_read = parse_u64(fields.get("IOReadBytes"));
            let io_write = parse_u64(fields.get("IOWriteBytes"));
            let net_rx = parse_u64(fields.get("IPIngressBytes"));
            let net_tx = parse_u64(fields.get("IPEgressBytes"));

            let (cpu_pct, disk_read_rate, disk_write_rate, net_rx_rate, net_tx_rate) =
                match previous.get(&unit) {
                    Some(prev) => {
                        let elapsed = now.duration_since(prev.at).as_secs_f64();
                        if elapsed <= 0.0 {
                            (0.0, 0.0, 0.0, 0.0, 0.0)
                        } else {
                            (
                                delta_rate(cpu_nsec, prev.cpu_nsec, elapsed) * 100.0,
                                delta_rate(io_read, prev.io_read, elapsed),
                                delta_rate(io_write, prev.io_write, elapsed),
                                delta_rate(net_rx, prev.net_rx, elapsed),
                                delta_rate(net_tx, prev.net_tx, elapsed),
                            )
                        }
                    }
                    None => (0.0, 0.0, 0.0, 0.0, 0.0),
                };

            previous.insert(
                unit.clone(),
                ServicePrev {
                    at: now,
                    cpu_nsec,
                    io_read,
                    io_write,
                    net_rx,
                    net_tx,
                },
            );

            services.push(ServiceLive {
                unit,
                description: fields
                    .get("Description")
                    .cloned()
                    .unwrap_or_else(|| "—".into()),
                load_state,
                active_state: fields
                    .get("ActiveState")
                    .cloned()
                    .unwrap_or_else(|| "unknown".into()),
                sub_state: fields
                    .get("SubState")
                    .cloned()
                    .unwrap_or_else(|| "unknown".into()),
                unit_file_state: fields
                    .get("UnitFileState")
                    .cloned()
                    .unwrap_or_else(|| "—".into()),
                main_pid: parse_u64(fields.get("MainPID")) as u32,
                control_group: fields
                    .get("ControlGroup")
                    .cloned()
                    .unwrap_or_default(),
                cpu_pct: cpu_pct.clamp(0.0, 100_000.0),
                mem_used: parse_u64(fields.get("MemoryCurrent")),
                mem_max: parse_u64(fields.get("MemoryMax")),
                disk_read: io_read,
                disk_write: io_write,
                net_rx,
                net_tx,
                disk_read_rate,
                disk_write_rate,
                net_rx_rate,
                net_tx_rate,
            });
        }

        services.sort_by(|a, b| {
            (a.active_state != "active", a.unit.to_ascii_lowercase())
                .cmp(&(b.active_state != "active", b.unit.to_ascii_lowercase()))
        });
        Ok(services)
    }

    async fn service_units(&self) -> Result<Vec<String>> {
        let mut units = Vec::new();
        let mut last_error = None;

        match self
            .run(
                "systemctl",
                &[
                    "list-unit-files",
                    "--type=service",
                    "--no-legend",
                    "--no-pager",
                    "--plain",
                ],
                Duration::from_secs(15),
            )
            .await
        {
            Ok(output) if output.success => {
                for line in output.stdout.lines() {
                    if let Some(unit) = line.split_whitespace().next() {
                        if is_service_unit(unit) {
                            units.push(unit.to_string());
                        }
                    }
                }
            }
            Ok(output) => last_error = Some(first_error(&output)),
            Err(error) => last_error = Some(format!("{error:#}")),
        }

        match self
            .run(
                "systemctl",
                &[
                    "list-units",
                    "--type=service",
                    "--all",
                    "--no-legend",
                    "--no-pager",
                    "--plain",
                ],
                Duration::from_secs(15),
            )
            .await
        {
            Ok(output) if output.success => {
                for line in output.stdout.lines() {
                    if let Some(unit) = line.split_whitespace().next() {
                        if is_service_unit(unit) {
                            units.push(unit.to_string());
                        }
                    }
                }
            }
            Ok(output) => {
                if last_error.is_none() {
                    last_error = Some(first_error(&output));
                }
            }
            Err(error) => {
                if last_error.is_none() {
                    last_error = Some(format!("{error:#}"));
                }
            }
        }

        if units.is_empty() {
            if let Some(error) = last_error {
                bail!("systemctl tidak dapat membaca daftar service: {error}");
            }
        }

        units.sort();
        units.dedup();
        Ok(units)
    }

    pub async fn service_action(&self, unit: &str, action: &str) -> Result<HostOutput> {
        if !valid_unit_name(unit) {
            bail!("nama unit service tidak valid");
        }
        let args: Vec<&str> = match action {
            "start" | "stop" | "restart" => vec![action, unit],
            "enable" => vec!["enable", "--now", unit],
            "disable" => vec!["disable", "--now", unit],
            other => bail!("aksi service tidak dikenal: {other}"),
        };
        let output = self
            .run("systemctl", &args, Duration::from_secs(60))
            .await?;
        if !output.success {
            bail!(
                "systemctl {action} {unit} gagal: {}",
                first_error(&output)
            );
        }
        Ok(output)
    }

    pub async fn service_logs(&self, unit: &str, lines: usize) -> Result<String> {
        if !valid_unit_name(unit) {
            bail!("nama unit service tidak valid");
        }
        let count = lines.clamp(10, 500).to_string();
        let output = self
            .run(
                "journalctl",
                &[
                    "-u",
                    unit,
                    "-n",
                    &count,
                    "--no-pager",
                    "--output=short-iso",
                ],
                Duration::from_secs(20),
            )
            .await?;
        if !output.success {
            bail!(
                "journalctl -u {unit} gagal: {}",
                first_error(&output)
            );
        }
        Ok(output.stdout)
    }

    pub async fn list_packages(&self) -> Result<Vec<PackageLive>> {
        let format =
            "-f=${Package}\t${Version}\t${Architecture}\t${Status}\t${binary:Summary}\n";
        let output = self
            .run("dpkg-query", &["-W", format], Duration::from_secs(20))
            .await?;
        if !output.success {
            bail!("dpkg-query gagal: {}", first_error(&output));
        }

        let mut packages = Vec::new();
        for line in output.stdout.lines() {
            let parts: Vec<&str> = line.splitn(5, '\t').collect();
            if parts.len() < 5 || !parts[3].contains("installed") {
                continue;
            }
            packages.push(PackageLive {
                name: parts[0].to_string(),
                version: parts[1].to_string(),
                architecture: parts[2].to_string(),
                summary: parts[4].to_string(),
            });
        }
        packages.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(packages)
    }

    pub async fn install_package(
        &self,
        package: &str,
        service: Option<&str>,
        start_service: bool,
        update_index: bool,
    ) -> Result<String> {
        if !valid_package_name(package) {
            bail!("nama paket tidak valid");
        }
        if let Some(unit) = service {
            if !unit.is_empty() && !valid_unit_name(unit) {
                bail!("nama unit service tidak valid");
            }
        }

        let mut log = String::new();
        if update_index {
            let output = self.apt(&["update"], Duration::from_secs(600)).await?;
            log.push_str(&output.stdout);
            if !output.success {
                bail!("apt-get update gagal: {}", first_error(&output));
            }
        }

        let output = self
            .apt(
                &[
                    "install",
                    "-y",
                    "-o",
                    "Dpkg::Options::=--force-confdef",
                    "-o",
                    "Dpkg::Options::=--force-confold",
                    package,
                ],
                Duration::from_secs(900),
            )
            .await?;
        log.push_str(&output.stdout);
        if !output.success {
            bail!("apt-get install {package} gagal: {}", first_error(&output));
        }

        if let Some(unit) = service.filter(|unit| !unit.is_empty()) {
            if start_service {
                match self.service_action(unit, "enable").await {
                    Ok(output) => log.push_str(&output.stdout),
                    Err(error) => {
                        log.push_str(&format!("\nPeringatan: gagal mengaktifkan {unit}: {error:#}"));
                    }
                }
            }
        }

        Ok(cap_output(log))
    }

    pub async fn remove_package(&self, package: &str, purge: bool) -> Result<String> {
        if !valid_package_name(package) {
            bail!("nama paket tidak valid");
        }
        let action = if purge { "purge" } else { "remove" };
        let output = self
            .apt(&[action, "-y", package], Duration::from_secs(600))
            .await?;
        if !output.success {
            bail!("apt-get {action} {package} gagal: {}", first_error(&output));
        }
        Ok(cap_output(output.stdout))
    }

    pub async fn network_snapshot(
        &self,
        previous: &mut HashMap<String, NetPrev>,
    ) -> Result<NetworkSnapshot> {
        let raw = std::fs::read_to_string(self.host_root.join("proc/net/dev"))
            .context("gagal membaca /proc/net/dev host")?;
        let now = Instant::now();
        let mut interfaces = Vec::new();

        for line in raw.lines().skip(2) {
            let Some((name, rest)) = line.split_once(':') else {
                continue;
            };
            let name = name.trim().to_string();
            if name.is_empty() {
                continue;
            }
            let fields: Vec<&str> = rest.split_whitespace().collect();
            if fields.len() < 16 {
                continue;
            }
            let rx = fields[0].parse::<u64>().unwrap_or(0);
            let tx = fields[8].parse::<u64>().unwrap_or(0);
            let state = std::fs::read_to_string(
                self.host_root
                    .join(format!("sys/class/net/{name}/operstate")),
            )
            .map(|value| value.trim().to_string())
            .unwrap_or_else(|_| "unknown".into());

            let (rx_rate, tx_rate) = match previous.get(&name) {
                Some(prev) => {
                    let elapsed = now.duration_since(prev.at).as_secs_f64();
                    if elapsed > 0.0 {
                        (
                            rx.saturating_sub(prev.rx) as f64 / elapsed,
                            tx.saturating_sub(prev.tx) as f64 / elapsed,
                        )
                    } else {
                        (0.0, 0.0)
                    }
                }
                None => (0.0, 0.0),
            };
            previous.insert(
                name.clone(),
                NetPrev {
                    at: now,
                    rx,
                    tx,
                },
            );

            interfaces.push(NetworkInterface {
                name,
                state,
                addresses: Vec::new(),
                rx_bytes: rx,
                tx_bytes: tx,
                rx_rate,
                tx_rate,
            });
        }

        interfaces.sort_by(|a, b| {
            (a.name == "lo", a.name.to_ascii_lowercase())
                .cmp(&(b.name == "lo", b.name.to_ascii_lowercase()))
        });
        let rx_bytes = interfaces
            .iter()
            .filter(|interface| interface.name != "lo")
            .map(|interface| interface.rx_bytes)
            .sum();
        let tx_bytes = interfaces
            .iter()
            .filter(|interface| interface.name != "lo")
            .map(|interface| interface.tx_bytes)
            .sum();
        let rx_rate = interfaces
            .iter()
            .filter(|interface| interface.name != "lo")
            .map(|interface| interface.rx_rate)
            .sum();
        let tx_rate = interfaces
            .iter()
            .filter(|interface| interface.name != "lo")
            .map(|interface| interface.tx_rate)
            .sum();

        Ok(NetworkSnapshot {
            interfaces,
            rx_rate,
            tx_rate,
            rx_bytes,
            tx_bytes,
            default_gateway: self.default_gateway().unwrap_or_default(),
            dns_servers: self.dns_servers(),
        })
    }

    pub async fn network_addresses(&self) -> Result<HashMap<String, Vec<String>>> {
        if !self.enabled {
            return Ok(HashMap::new());
        }
        let output = self
            .run("ip", &["-j", "addr", "show"], Duration::from_secs(8))
            .await?;
        if !output.success {
            bail!("ip addr gagal: {}", first_error(&output));
        }
        let value: Value =
            serde_json::from_str(&output.stdout).context("output ip addr bukan JSON")?;
        let mut addresses = HashMap::new();
        if let Some(items) = value.as_array() {
            for item in items {
                let Some(name) = item.get("ifname").and_then(Value::as_str) else {
                    continue;
                };
                let mut list = Vec::new();
                if let Some(addr_info) = item.get("addr_info").and_then(Value::as_array) {
                    for address in addr_info {
                        let Some(local) = address.get("local").and_then(Value::as_str) else {
                            continue;
                        };
                        let prefix = address
                            .get("prefixlen")
                            .and_then(Value::as_u64)
                            .unwrap_or(0);
                        list.push(format!("{local}/{prefix}"));
                    }
                }
                addresses.insert(name.to_string(), list);
            }
        }
        Ok(addresses)
    }

    fn default_gateway(&self) -> Option<String> {
        let raw = std::fs::read_to_string(self.host_root.join("proc/net/route")).ok()?;
        for line in raw.lines().skip(1) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 3 || fields[1] != "00000000" {
                continue;
            }
            let value = u32::from_str_radix(fields[2], 16).ok()?;
            let bytes = value.to_le_bytes();
            return Some(format!("{}.{}.{}.{}", bytes[0], bytes[1], bytes[2], bytes[3]));
        }
        None
    }

    fn dns_servers(&self) -> Vec<String> {
        let Ok(raw) = std::fs::read_to_string(self.host_root.join("etc/resolv.conf")) else {
            return Vec::new();
        };
        raw.lines()
            .filter_map(|line| {
                let mut parts = line.split_whitespace();
                match (parts.next(), parts.next()) {
                    (Some("nameserver"), Some(value)) => Some(value.to_string()),
                    _ => None,
                }
            })
            .collect()
    }

    pub fn shell_command(&self) -> (String, Vec<String>) {
        if self.enabled {
            (
                "nsenter".into(),
                vec![
                    "-t".into(),
                    "1".into(),
                    "-m".into(),
                    "-u".into(),
                    "-i".into(),
                    "-n".into(),
                    "-p".into(),
                    // Setelah masuk mount namespace host, mulai shell di root
                    // host, bukan di cwd container yang mungkin tidak terlihat.
                    "--wd=/".into(),
                    "--".into(),
                    "/bin/bash".into(),
                    "-l".into(),
                ],
            )
        } else {
            ("/bin/sh".into(), vec!["-l".into()])
        }
    }

    pub fn terminal_environment(&self) -> Vec<(String, String)> {
        if !self.enabled { return Vec::new(); }
        let mut users = Vec::new();
        if let Ok(passwd) = std::fs::read_to_string(self.host_root.join("etc/passwd")) {
            for line in passwd.lines() {
                let f: Vec<&str> = line.split(':').collect();
                if f.len() >= 7 && f[2].parse::<u32>().unwrap_or(0) >= 1000
                    && f[2].parse::<u32>().unwrap_or(0) < 60000
                    && f[5].starts_with('/') && f[6] != "/usr/sbin/nologin" && f[6] != "/bin/false" {
                    users.push((f[0].to_string(), f[5].to_string()));
                }
            }
        }
        users.sort_by(|a, b| a.0.cmp(&b.0));
        let selected = self.terminal_user.as_deref()
            .and_then(|wanted| users.iter().find(|(name, _)| name == wanted))
            .cloned()
            .or_else(|| users.iter().find(|(_, home)| {
                [".local/bin/opencode", ".opencode/bin/opencode", ".npm-global/bin/opencode", ".bun/bin/opencode"]
                    .iter().any(|suffix| self.host_root.join(home.trim_start_matches('/')).join(suffix).is_file())
            }).cloned())
            .or_else(|| users.first().cloned())
            .unwrap_or_else(|| ("root".into(), "/root".into()));
        let mut paths = vec!["/usr/local/sbin", "/usr/local/bin", "/usr/sbin", "/usr/bin", "/sbin", "/bin"]
            .into_iter().map(String::from).collect::<Vec<_>>();
        let mut homes = vec![selected.1.clone()];
        homes.extend(users.iter().map(|(_, home)| home.clone()));
        homes.sort(); homes.dedup();
        for home in homes {
            for suffix in [".local/bin", ".opencode/bin", ".npm-global/bin", ".cargo/bin", ".bun/bin", ".deno/bin"] {
                let path = format!("{home}/{suffix}");
                if self.host_root.join(path.trim_start_matches('/')).is_dir() { paths.push(path); }
            }
        }
        paths.sort(); paths.dedup();
        vec![("PATH".into(), paths.join(":")), ("HOME".into(), selected.1),
             ("USER".into(), selected.0.clone()), ("LOGNAME".into(), selected.0)]
    }
}

fn is_service_unit(unit: &str) -> bool {
    unit.ends_with(".service") && !unit.contains("@.") && !unit.starts_with('-')
}

fn parse_properties(block: &str) -> HashMap<String, String> {
    let mut fields = HashMap::new();
    for line in block.lines() {
        if let Some((key, value)) = line.split_once('=') {
            fields.insert(key.to_string(), value.to_string());
        }
    }
    fields
}

fn parse_u64(value: Option<&String>) -> u64 {
    value
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value != u64::MAX)
        .unwrap_or(0)
}

fn delta_rate(current: u64, previous: u64, elapsed: f64) -> f64 {
    if elapsed <= 0.0 || current < previous {
        0.0
    } else {
        (current - previous) as f64 / elapsed
    }
}

fn first_error(output: &HostOutput) -> String {
    let stderr = output.stderr.trim();
    if !stderr.is_empty() {
        return stderr.to_string();
    }
    let stdout = output.stdout.trim();
    if !stdout.is_empty() {
        return stdout.to_string();
    }
    format!("exit status {:?}", output.status)
}

fn cap_output(mut text: String) -> String {
    if text.len() > MAX_OUTPUT_BYTES {
        let mut end = MAX_OUTPUT_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n… output dipotong");
    }
    text
}

pub fn valid_unit_name(unit: &str) -> bool {
    !unit.is_empty()
        && unit.len() <= 256
        && (unit.ends_with(".service") || unit.ends_with(".socket"))
        && unit
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '@' | ':' | '\\'))
}

pub fn valid_package_name(package: &str) -> bool {
    let mut chars = package.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_lowercase()
        && package.len() <= 128
        && chars.all(|ch| {
            ch.is_ascii_lowercase()
                || ch.is_ascii_digit()
                || matches!(ch, '+' | '-' | '.' | ':')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_unit_names() {
        assert!(valid_unit_name("nginx.service"));
        assert!(valid_unit_name("getty@tty1.service"));
        assert!(valid_unit_name("cockpit.socket"));
        assert!(!valid_unit_name("nginx"));
        assert!(!valid_unit_name("../../etc/passwd.service"));
        assert!(!valid_unit_name("nginx.service;reboot"));
    }

    #[test]
    fn validates_package_names() {
        assert!(valid_package_name("nginx"));
        assert!(valid_package_name("libc6:arm64"));
        assert!(!valid_package_name("-o"));
        assert!(!valid_package_name("Nginx"));
        assert!(!valid_package_name("nginx;reboot"));
    }
}
