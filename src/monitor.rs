use std::collections::HashMap;
use std::time::{Duration, Instant};

use sysinfo::{LoadAvg, System};
use tracing::{error, info, warn};

use crate::db::{ContainerSample, SystemSample};
use crate::docker;
use crate::state::{AppState, LiveSnapshot};
use crate::util;

pub async fn run(state: AppState) {
    let sample_interval = Duration::from_secs(state.cfg.sample_secs);
    let container_every = (state.cfg.container_sample_secs / state.cfg.sample_secs).max(1);
    let host_root = state.cfg.resolved_host_root();
    let disk_path = host_root.display().to_string();

    let Ok(mut system) = System::new() else {
        error!(target: "workamon::monitor", "gagal menginisialisasi pembacaan sistem");
        return;
    };
    system.refresh_memory();
    system.refresh_cpu_usage();

    let mut previous_cpu: HashMap<String, (u64, u64)> = HashMap::new();
    let mut last_docker_error: Option<String> = None;
    let mut last_maintenance = Instant::now() - Duration::from_secs(3_600);
    let mut tick: u64 = 0;

    info!(
        target: "workamon::monitor",
        "monitor dimulai: sampel {} detik, riwayat container {} detik, disk {}",
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

        let mut containers = Vec::new();
        let mut docker_ok = state.docker.is_some();
        let mut docker_error = None;

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
            docker_error = state.docker_startup_error.clone();
        }

        let running = containers
            .iter()
            .filter(|container| container.state == "running")
            .count();
        let sample = SystemSample {
            ts: util::now(),
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

        if let Err(error) = state.db.insert_system(&sample) {
            error!(target: "workamon::db", "gagal menyimpan metrik sistem: {error:#}");
        }

        if tick % container_every == 0 {
            for container in &containers {
                if container.state != "running" {
                    continue;
                }
                let history = ContainerSample {
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
                if let Err(error) = state.db.insert_container(&history) {
                    error!(target: "workamon::db", "gagal menyimpan metrik container: {error:#}");
                }
            }
        }

        let hostname = System::host_name().unwrap_or_else(|_| "unknown".into());
        let os = System::long_os_version().unwrap_or_else(|_| "Linux".into());
        let cpu_count = system.cpus().len();
        {
            let mut live = state.live.write().await;
            *live = LiveSnapshot {
                system: Some(sample),
                containers,
                docker_ok,
                docker_error,
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
        let sleep_for = sample_interval.saturating_sub(elapsed);
        tokio::time::sleep(sleep_for).await;
    }
}
