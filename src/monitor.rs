use std::collections::HashMap;
use std::time::{Duration, Instant};

use sysinfo::{LoadAvg, System};
use tracing::{error, info, warn};

use crate::db::{ContainerSample, SystemSample};
use crate::docker;
use crate::host::{NetPrev, NetworkSample, NetworkSnapshot, ServicePrev};
use crate::state::{AppState, LiveSnapshot};
use crate::util;

const MAX_LIVE_POINTS: usize = 1_800;

pub async fn run(state: AppState) {
    // Data live diperbarui paling lambat setiap 5 detik. Sampel database tetap
    // mengikuti WORKAMON_SAMPLE_SECS agar riwayat tidak membengkak.
    let live_interval_secs = state.cfg.sample_secs.min(5).max(1);
    let live_interval = Duration::from_secs(live_interval_secs);
    // Statistik Docker diperbarui setiap 10 detik agar halaman container tetap
    // terasa live tanpa membanjiri daemon. Penulisan database memakai interval
    // terpisah supaya riwayat tetap mengikuti WORKAMON_SAMPLE_SECS.
    let docker_every = (10 / live_interval_secs).max(1);
    let db_every = (state.cfg.sample_secs / live_interval_secs).max(1);
    let container_every = (state.cfg.container_sample_secs / live_interval_secs).max(1);
    let host_root = state.cfg.resolved_host_root();
    let disk_path = host_root.display().to_string();

    let Ok(mut system) = System::new() else {
        error!(target: "workamon::monitor", "gagal menginisialisasi pembacaan sistem");
        return;
    };
    system.refresh_memory();
    system.refresh_cpu_usage();

    let mut previous_cpu: HashMap<String, (u64, u64)> = HashMap::new();
    let mut previous_services: HashMap<String, ServicePrev> = HashMap::new();
    let mut previous_network: HashMap<String, NetPrev> = HashMap::new();
    let mut containers = Vec::new();
    let mut services = Vec::new();
    let mut network: Option<NetworkSnapshot> = None;
    let mut network_history: Vec<NetworkSample> = Vec::with_capacity(MAX_LIVE_POINTS);
    let mut network_addresses: HashMap<String, Vec<String>> = HashMap::new();
    let mut history: Vec<SystemSample> = Vec::with_capacity(MAX_LIVE_POINTS);
    let mut docker_ok = state.docker.is_some();
    let mut docker_error = if state.docker.is_some() {
        None
    } else {
        state.docker_startup_error.clone()
    };
    let mut services_ok = false;
    let mut services_error = state.host.disabled_message();
    let mut network_ok;
    let mut network_error: Option<String>;
    let mut last_docker_error: Option<String> = None;
    let mut last_services_error: Option<String> = None;
    let mut last_network_error: Option<String> = None;
    let mut last_network_address_refresh = Instant::now() - Duration::from_secs(60);
    let mut last_maintenance = Instant::now() - Duration::from_secs(3_600);
    let mut tick: u64 = 0;

    info!(
        target: "workamon::monitor",
        "monitor dimulai: data live {} detik, sampel database {} detik, riwayat container {} detik, disk {}",
        live_interval_secs,
        state.cfg.sample_secs,
        state.cfg.container_sample_secs,
        disk_path
    );

    loop {
        let cycle_start = Instant::now();
        system.refresh_cpu_usage();
        system.refresh_memory();

        let load = System::load_average().unwrap_or(LoadAvg {
            one: 0.0,
            five: 0.0,
            fifteen: 0.0,
        });
        let (disk_used, disk_total) = util::disk_usage(&host_root);
        let mem_total = system.total_memory();
        let mem_used = system.used_memory().min(mem_total);
        let swap_total = system.total_swap();
        let swap_used = system.used_swap().min(swap_total);
        let temp_c = util::read_cpu_temp(&host_root);
        let uptime_secs = System::uptime().unwrap_or(0);
        let ts = util::now();

        let refresh_docker = tick % docker_every == 0;
        if refresh_docker {
            if let Some(docker) = &state.docker {
                match tokio::time::timeout(
                    Duration::from_secs(20),
                    docker::collect(docker, &mut previous_cpu),
                )
                .await
                {
                    Ok(Ok(list)) => {
                        containers = list;
                        docker_ok = true;
                        if last_docker_error.is_some() {
                            info!(target: "workamon::docker", "koneksi Docker pulih");
                        }
                        last_docker_error = None;
                        docker_error = None;
                    }
                    Ok(Err(error)) => {
                        let message = format!("{error:#}");
                        docker_ok = false;
                        if last_docker_error.as_deref() != Some(message.as_str()) {
                            warn!(target: "workamon::docker", "gagal memantau container: {message}");
                        }
                        last_docker_error = Some(message.clone());
                        docker_error = Some(message);
                    }
                    Err(_) => {
                        let message = "permintaan Docker melewati batas waktu 20 detik".to_string();
                        docker_ok = false;
                        if last_docker_error.as_deref() != Some(message.as_str()) {
                            warn!(target: "workamon::docker", "{message}");
                        }
                        last_docker_error = Some(message.clone());
                        docker_error = Some(message);
                    }
                }
            } else {
                docker_ok = false;
                docker_error = state.docker_startup_error.clone();
            }
        }

        if refresh_docker {
            if state.host.enabled() {
                match tokio::time::timeout(
                    Duration::from_secs(20),
                    state.host.list_services(&mut previous_services),
                )
                .await
                {
                    Ok(Ok(list)) => {
                        services = list;
                        services_ok = true;
                        if last_services_error.is_some() {
                            info!(target: "workamon::host", "pembacaan service systemd pulih");
                        }
                        last_services_error = None;
                        services_error = None;
                    }
                    Ok(Err(error)) => {
                        let message = format!("{error:#}");
                        services_ok = false;
                        if last_services_error.as_deref() != Some(message.as_str()) {
                            warn!(target: "workamon::host", "gagal membaca service systemd: {message}");
                        }
                        last_services_error = Some(message.clone());
                        services_error = Some(message);
                    }
                    Err(_) => {
                        let message =
                            "pembacaan service systemd melewati batas waktu 20 detik".to_string();
                        services_ok = false;
                        if last_services_error.as_deref() != Some(message.as_str()) {
                            warn!(target: "workamon::host", "{message}");
                        }
                        last_services_error = Some(message.clone());
                        services_error = Some(message);
                    }
                }
            } else {
                services_ok = false;
                services_error = state.host.disabled_message();
            }
        }

        match state.host.network_snapshot(&mut previous_network).await {
            Ok(mut snapshot) => {
                if last_network_address_refresh.elapsed() >= Duration::from_secs(60) {
                    last_network_address_refresh = Instant::now();
                    if let Ok(addresses) = state.host.network_addresses().await {
                        network_addresses = addresses;
                    }
                }
                for interface in &mut snapshot.interfaces {
                    if let Some(addresses) = network_addresses.get(&interface.name) {
                        interface.addresses = addresses.clone();
                    }
                }
                network_history.push(NetworkSample {
                    ts,
                    rx_rate: snapshot.rx_rate,
                    tx_rate: snapshot.tx_rate,
                });
                if network_history.len() > MAX_LIVE_POINTS {
                    let excess = network_history.len() - MAX_LIVE_POINTS;
                    network_history.drain(0..excess);
                }
                network = Some(snapshot);
                network_ok = true;
                if last_network_error.is_some() {
                    info!(target: "workamon::host", "pembacaan network host pulih");
                }
                last_network_error = None;
                network_error = None;
            }
            Err(error) => {
                let message = format!("{error:#}");
                network_ok = false;
                if last_network_error.as_deref() != Some(message.as_str()) {
                    warn!(target: "workamon::host", "gagal membaca network host: {message}");
                }
                last_network_error = Some(message.clone());
                network_error = Some(message);
            }
        }

        let running = containers
            .iter()
            .filter(|container| container.state == "running")
            .count();
        let sample = SystemSample {
            ts,
            cpu_pct: system.global_cpu_usage() as f64,
            mem_used,
            mem_total,
            swap_used,
            swap_total,
            disk_used,
            disk_total,
            load1: load.one,
            load5: load.five,
            load15: load.fifteen,
            temp_c,
            containers_running: running as u32,
            containers_total: containers.len() as u32,
        };

        history.push(sample.clone());
        if history.len() > MAX_LIVE_POINTS {
            let excess = history.len() - MAX_LIVE_POINTS;
            history.drain(0..excess);
        }

        if tick % db_every == 0 {
            if let Err(error) = state.db.insert_system(&sample) {
                error!(target: "workamon::db", "gagal menyimpan metrik sistem: {error:#}");
            }
        }

        if tick % container_every == 0 {
            for container in &containers {
                if container.state != "running" {
                    continue;
                }
                let history_row = ContainerSample {
                    ts: sample.ts,
                    container_id: container.id.clone(),
                    name: container.name.clone(),
                    cpu_pct: container.cpu_pct,
                    mem_used: container.mem_used,
                    mem_limit: container.mem_limit,
                    net_rx: container.net_rx,
                    net_tx: container.net_tx,
                    state: container.state.clone(),
                };
                if let Err(error) = state.db.insert_container(&history_row) {
                    error!(target: "workamon::db", "gagal menyimpan metrik container: {error:#}");
                }
            }
        }

        let hostname = util::read_hostname(&host_root)
            .or_else(|| System::host_name().ok())
            .unwrap_or_else(|| "unknown".into());
        let os = util::read_os_pretty_name(&host_root)
            .or_else(|| System::long_os_version().ok())
            .unwrap_or_else(|| "Linux".into());
        let cpu_count = system.cpus().len();
        {
            let mut live = state.live.write().await;
            *live = LiveSnapshot {
                system: Some(sample),
                history: history.clone(),
                containers: containers.clone(),
                services: services.clone(),
                services_ok,
                services_error: services_error.clone(),
                network: network.clone(),
                network_history: network_history.clone(),
                network_ok,
                network_error: network_error.clone(),
                docker_ok,
                docker_error: docker_error.clone(),
                last_update: util::now(),
                hostname,
                os,
                cpu_count,
                uptime_secs,
                disk_path: disk_path.clone(),
            };
        }

        if last_maintenance.elapsed() >= Duration::from_secs(120) {
            last_maintenance = Instant::now();
            let cutoff = util::now() - state.cfg.log_retention_days * 86_400;
            match state.db.cleanup_logs_before(cutoff) {
                Ok(deleted) if deleted > 0 => {
                    info!(
                        target: "workamon::db",
                        "menghapus {deleted} log lebih lama dari {} hari",
                        state.cfg.log_retention_days
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    error!(target: "workamon::db", "gagal membersihkan log lama: {error:#}")
                }
            }

            match state.db.trim_to_limit() {
                Ok(report)
                    if report.deleted_system > 0
                        || report.deleted_containers > 0
                        || report.deleted_logs > 0 =>
                {
                    warn!(
                        target: "workamon::db",
                        "database mencapai batas; menghapus {} metrik sistem, {} metrik container, {} log ({} -> {})",
                        report.deleted_system,
                        report.deleted_containers,
                        report.deleted_logs,
                        util::fmt_bytes(report.before_bytes),
                        util::fmt_bytes(report.after_bytes)
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    error!(target: "workamon::db", "gagal memangkas database: {error:#}")
                }
            }
        }

        tick = tick.wrapping_add(1);
        let elapsed = cycle_start.elapsed();
        let sleep_for = live_interval.saturating_sub(elapsed);
        tokio::time::sleep(sleep_for).await;
    }
}
