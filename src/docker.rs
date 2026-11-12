use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result};
use bollard::container::LogOutput;
use bollard::models::{
    ContainerCreateBody, ContainerInspectResponse, ContainerSummary, HostConfig, ImageInspect,
    ImageSummary, PortBinding, RestartPolicy, RestartPolicyNameEnum,
};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, CreateImageOptionsBuilder, ListContainersOptionsBuilder,
    ListImagesOptionsBuilder, LogsOptionsBuilder, PruneBuildOptionsBuilder,
    PruneImagesOptionsBuilder, PruneVolumesOptionsBuilder, RemoveContainerOptionsBuilder,
    RemoveImageOptionsBuilder, StatsOptionsBuilder,
};
use bollard::Docker;
use futures_util::StreamExt;
use serde::Serialize;

use crate::util;

#[derive(Debug, Clone, Default, Serialize)]
pub struct ContainerLive {
    pub id: String,
    pub name: String,
    pub image: String,
    pub state: String,
    pub status: String,
    pub health: Option<String>,
    pub created: i64,
    pub cpu_pct: f64,
    pub mem_used: u64,
    pub mem_limit: u64,
    pub net_rx: u64,
    pub net_tx: u64,
    pub ports: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DfView {
    pub images_size: u64,
    pub images_count: i64,
    pub images_reclaimable: u64,
    pub containers_size: u64,
    pub containers_count: i64,
    pub containers_reclaimable: u64,
    pub volumes_size: u64,
    pub volumes_count: i64,
    pub volumes_reclaimable: u64,
    pub build_size: u64,
    pub build_count: i64,
    pub build_reclaimable: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PruneReport {
    pub kind: String,
    pub deleted: usize,
    pub reclaimed: u64,
}

struct StatsView {
    cpu_pct: f64,
    mem_used: u64,
    mem_limit: u64,
    net_rx: u64,
    net_tx: u64,
}

pub async fn connect(enabled: bool) -> (Option<Docker>, Option<String>) {
    if !enabled {
        return (
            None,
            Some("Integrasi Docker dimatikan lewat WORKAMON_DOCKER=0".into()),
        );
    }
    let docker = match Docker::connect_with_local_defaults() {
        Ok(docker) => docker,
        Err(error) => {
            return (
                None,
                Some(format!("Tidak dapat menyiapkan koneksi Docker: {error}")),
            )
        }
    };
    match tokio::time::timeout(Duration::from_secs(5), docker.ping()).await {
        Ok(Ok(_)) => (Some(docker), None),
        Ok(Err(error)) => (
            None,
            Some(format!("Docker daemon tidak merespons: {error}")),
        ),
        Err(_) => (
            None,
            Some("Docker daemon tidak merespons dalam 5 detik".into()),
        ),
    }
}

pub async fn collect(
    docker: &Docker,
    previous_cpu: &mut HashMap<String, (u64, u64)>,
) -> Result<Vec<ContainerLive>> {
    let options = ListContainersOptionsBuilder::default().all(true).build();
    let list = docker
        .list_containers(Some(options))
        .await
        .context("gagal membaca daftar container")?;

    let mut containers = Vec::with_capacity(list.len());
    for summary in list {
        let id = summary.id.clone().unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        let name = summary
            .names
            .as_ref()
            .and_then(|names| names.first())
            .map(|name| name.trim_start_matches('/').to_string())
            .unwrap_or_else(|| util::short_id(&id));
        let state = summary
            .state
            .map(|state| state.to_string())
            .unwrap_or_else(|| "unknown".into());
        let health = summary
            .health
            .as_ref()
            .and_then(|health| health.status)
            .map(|status| status.to_string())
            .filter(|status| !status.is_empty() && status != "none");

        let mut live = ContainerLive {
            id: id.clone(),
            name,
            image: summary.image.clone().unwrap_or_else(|| "—".into()),
            state: state.clone(),
            status: summary.status.clone().unwrap_or_default(),
            health,
            created: summary.created.unwrap_or(0),
            ports: format_ports(&summary),
            ..Default::default()
        };

        if state == "running" {
            if let Some(stats) = sample_stats(docker, &id, previous_cpu).await {
                live.cpu_pct = stats.cpu_pct;
                live.mem_used = stats.mem_used;
                live.mem_limit = stats.mem_limit;
                live.net_rx = stats.net_rx;
                live.net_tx = stats.net_tx;
            }
        }

        containers.push(live);
    }

    containers.sort_by(|a, b| {
        a.name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
    });
    Ok(containers)
}

async fn sample_stats(
    docker: &Docker,
    id: &str,
    previous_cpu: &mut HashMap<String, (u64, u64)>,
) -> Option<StatsView> {
    let options = StatsOptionsBuilder::default()
        .stream(false)
        .one_shot(true)
        .build();
    let stream = docker.stats(id, Some(options));
    tokio::pin!(stream);
    let stats = tokio::time::timeout(Duration::from_secs(4), stream.next())
        .await
        .ok()??
        .ok()?;

    let cpu = stats.cpu_stats.as_ref();
    let current_total = cpu
        .and_then(|cpu| cpu.cpu_usage.as_ref())
        .and_then(|usage| usage.total_usage)
        .unwrap_or(0);
    let system_total = cpu.and_then(|cpu| cpu.system_cpu_usage).unwrap_or(0);
    let online_cpus = cpu
        .and_then(|cpu| cpu.online_cpus)
        .unwrap_or(1)
        .max(1) as f64;

    let precpu_total = stats
        .precpu_stats
        .as_ref()
        .and_then(|cpu| cpu.cpu_usage.as_ref())
        .and_then(|usage| usage.total_usage)
        .unwrap_or(0);
    let presystem_total = stats
        .precpu_stats
        .as_ref()
        .and_then(|cpu| cpu.system_cpu_usage)
        .unwrap_or(0);

    let saved = previous_cpu.get(id).copied().unwrap_or((0, 0));
    let (prev_total, prev_system) = if current_total > precpu_total
        && system_total > presystem_total
        && precpu_total > 0
    {
        (precpu_total, presystem_total)
    } else {
        saved
    };
    previous_cpu.insert(id.to_string(), (current_total, system_total));

    let cpu_pct = if current_total > prev_total && system_total > prev_system {
        (current_total - prev_total) as f64 / (system_total - prev_system) as f64 * online_cpus * 100.0
    } else {
        0.0
    };

    let memory = stats.memory_stats.as_ref();
    let raw_usage = memory.and_then(|memory| memory.usage).unwrap_or(0);
    let cache = memory
        .and_then(|memory| memory.stats.as_ref())
        .and_then(|stats| {
            stats
                .get("total_inactive_file")
                .or_else(|| stats.get("inactive_file"))
                .or_else(|| stats.get("cache"))
                .copied()
        })
        .unwrap_or(0);
    let mem_used = raw_usage.saturating_sub(cache);
    let mem_limit = memory.and_then(|memory| memory.limit).unwrap_or(0);

    let (mut net_rx, mut net_tx) = (0u64, 0u64);
    if let Some(networks) = &stats.networks {
        for network in networks.values() {
            net_rx += network.rx_bytes.unwrap_or(0);
            net_tx += network.tx_bytes.unwrap_or(0);
        }
    }

    Some(StatsView {
        cpu_pct: cpu_pct.clamp(0.0, 100_000.0),
        mem_used,
        mem_limit,
        net_rx,
        net_tx,
    })
}

pub async fn inspect(docker: &Docker, id: &str) -> Result<ContainerInspectResponse> {
    docker
        .inspect_container(id, None)
        .await
        .with_context(|| format!("gagal memeriksa container {id}"))
}

pub async fn action(docker: &Docker, id: &str, action: &str) -> Result<()> {
    match action {
        "start" => docker.start_container(id, None).await?,
        "stop" => docker.stop_container(id, None).await?,
        "restart" => docker.restart_container(id, None).await?,
        other => anyhow::bail!("aksi tidak dikenal: {other}"),
    }
    Ok(())
}

pub async fn container_logs(docker: &Docker, id: &str, tail: usize) -> Result<String> {
    let tail_text = tail.to_string();
    let options = LogsOptionsBuilder::default()
        .stdout(true)
        .stderr(true)
        .timestamps(true)
        .tail(&tail_text)
        .build();
    let stream = docker.logs(id, Some(options));
    tokio::pin!(stream);

    let mut output = String::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(3), stream.next()).await {
            Ok(Some(Ok(line))) => {
                let text = match line {
                    LogOutput::StdErr { message }
                    | LogOutput::StdOut { message }
                    | LogOutput::StdIn { message }
                    | LogOutput::Console { message } => {
                        String::from_utf8_lossy(&message).to_string()
                    }
                };
                output.push_str(&text);
                if output.len() > 96 * 1024 {
                    break;
                }
            }
            Ok(Some(Err(error))) => return Err(error.into()),
            Ok(None) => break,
            Err(_) => break,
        }
    }

    let cleaned = util::strip_ansi(&output);
    let lines: Vec<&str> = cleaned.lines().collect();
    let start = lines.len().saturating_sub(tail.min(400));
    Ok(lines[start..].join("\n"))
}

pub async fn df(docker: &Docker) -> Result<DfView> {
    let usage = docker
        .df(None)
        .await
        .context("gagal membaca pemakaian disk Docker")?;
    let images = usage.image_usage.unwrap_or_default();
    let containers = usage.container_usage.unwrap_or_default();
    let volumes = usage.volume_usage.unwrap_or_default();
    let build = usage.build_cache_usage.unwrap_or_default();

    Ok(DfView {
        images_size: images.total_size.unwrap_or(0).max(0) as u64,
        images_count: images.total_count.unwrap_or(0),
        images_reclaimable: images.reclaimable.unwrap_or(0).max(0) as u64,
        containers_size: containers.total_size.unwrap_or(0).max(0) as u64,
        containers_count: containers.total_count.unwrap_or(0),
        containers_reclaimable: containers.reclaimable.unwrap_or(0).max(0) as u64,
        volumes_size: volumes.total_size.unwrap_or(0).max(0) as u64,
        volumes_count: volumes.total_count.unwrap_or(0),
        volumes_reclaimable: volumes.reclaimable.unwrap_or(0).max(0) as u64,
        build_size: build.total_size.unwrap_or(0).max(0) as u64,
        build_count: build.total_count.unwrap_or(0),
        build_reclaimable: build.reclaimable.unwrap_or(0).max(0) as u64,
    })
}

pub async fn prune(docker: &Docker, kind: &str) -> Result<PruneReport> {
    let (deleted, reclaimed) = match kind {
        "containers" => {
            let result = docker.prune_containers(None).await?;
            (
                result.containers_deleted.map(|items| items.len()).unwrap_or(0),
                result.space_reclaimed.unwrap_or(0),
            )
        }
        "images" => {
            let options = PruneImagesOptionsBuilder::default().build();
            let result = docker.prune_images(Some(options)).await?;
            (
                result.images_deleted.map(|items| items.len()).unwrap_or(0),
                result.space_reclaimed.unwrap_or(0),
            )
        }
        "volumes" => {
            let options = PruneVolumesOptionsBuilder::default().build();
            let result = docker.prune_volumes(Some(options)).await?;
            (
                result.volumes_deleted.map(|items| items.len()).unwrap_or(0),
                result.space_reclaimed.unwrap_or(0),
            )
        }
        "build" => {
            let options = PruneBuildOptionsBuilder::default().all(true).build();
            let result = docker.prune_build(Some(options)).await?;
            (
                result.caches_deleted.map(|items| items.len()).unwrap_or(0),
                result.space_reclaimed.unwrap_or(0),
            )
        }
        "networks" => {
            let result = docker.prune_networks(None).await?;
            (
                result.networks_deleted.map(|items| items.len()).unwrap_or(0),
                0,
            )
        }
        other => anyhow::bail!("jenis prune tidak dikenal: {other}"),
    };

    Ok(PruneReport {
        kind: kind.to_string(),
        deleted,
        reclaimed: reclaimed.max(0) as u64,
    })
}

#[derive(Debug, Clone, Default)]
pub struct NewContainer {
    pub name: String,
    pub image: String,
    pub cmd: Vec<String>,
    pub env: Vec<String>,
    /// (container port/protocol, host port)
    pub ports: Vec<(String, String)>,
    /// Docker bind strings such as /host/path:/container/path:ro
    pub binds: Vec<String>,
    pub restart: String,
    pub start: bool,
}

pub async fn create_container(docker: &Docker, spec: &NewContainer) -> Result<String> {
    let options = CreateContainerOptionsBuilder::default()
        .name(&spec.name)
        .build();

    let mut exposed_ports = Vec::new();
    let mut port_bindings = HashMap::new();
    for (container_port, host_port) in &spec.ports {
        exposed_ports.push(container_port.clone());
        port_bindings.insert(
            container_port.clone(),
            Some(vec![PortBinding {
                host_ip: None,
                host_port: Some(host_port.clone()),
            }]),
        );
    }

    let restart_name = match spec.restart.as_str() {
        "always" => Some(RestartPolicyNameEnum::ALWAYS),
        "unless-stopped" => Some(RestartPolicyNameEnum::UNLESS_STOPPED),
        "on-failure" => Some(RestartPolicyNameEnum::ON_FAILURE),
        "no" | "" => Some(RestartPolicyNameEnum::NO),
        _ => None,
    };

    let host_config = HostConfig {
        binds: if spec.binds.is_empty() {
            None
        } else {
            Some(spec.binds.clone())
        },
        port_bindings: if port_bindings.is_empty() {
            None
        } else {
            Some(port_bindings)
        },
        restart_policy: restart_name.map(|name| RestartPolicy {
            name: Some(name),
            maximum_retry_count: None,
        }),
        ..Default::default()
    };

    let body = ContainerCreateBody {
        image: Some(spec.image.clone()),
        cmd: if spec.cmd.is_empty() {
            None
        } else {
            Some(spec.cmd.clone())
        },
        env: if spec.env.is_empty() {
            None
        } else {
            Some(spec.env.clone())
        },
        exposed_ports: if exposed_ports.is_empty() {
            None
        } else {
            Some(exposed_ports)
        },
        host_config: Some(host_config),
        ..Default::default()
    };

    let created = docker
        .create_container(Some(options), body)
        .await
        .context("gagal membuat container")?;

    if spec.start {
        docker
            .start_container(&created.id, None)
            .await
            .context("container dibuat tetapi gagal dijalankan")?;
    }

    Ok(created.id)
}

pub async fn remove_container(docker: &Docker, id: &str) -> Result<()> {
    let options = RemoveContainerOptionsBuilder::default()
        .force(true)
        .v(false)
        .build();
    docker
        .remove_container(id, Some(options))
        .await
        .with_context(|| format!("gagal menghapus container {id}"))
}

pub async fn list_images(docker: &Docker) -> Result<Vec<ImageSummary>> {
    let options = ListImagesOptionsBuilder::default().all(true).build();
    docker
        .list_images(Some(options))
        .await
        .context("gagal membaca daftar image")
}

pub async fn inspect_image(docker: &Docker, reference: &str) -> Result<ImageInspect> {
    docker
        .inspect_image(reference)
        .await
        .with_context(|| format!("gagal memeriksa image {reference}"))
}

pub async fn pull_image(docker: &Docker, reference: &str) -> Result<String> {
    let (from_image, tag) = split_reference(reference);
    let mut builder = CreateImageOptionsBuilder::default().from_image(&from_image);
    if let Some(tag) = tag.as_deref() {
        builder = builder.tag(tag);
    }
    let options = builder.build();

    let mut stream = docker.create_image(Some(options), None, None);
    let mut last_status = String::new();
    let mut last_id: Option<String> = None;
    while let Some(item) = stream.next().await {
        let info = item.context("gagal menarik image")?;
        if let Some(detail) = info.error_detail {
            let message = detail
                .message
                .unwrap_or_else(|| "registry menolak permintaan".into());
            anyhow::bail!("{message}");
        }
        if let Some(status) = info.status {
            if !status.trim().is_empty() {
                last_status = status;
            }
        }
        if let Some(id) = info.id {
            if !id.trim().is_empty() {
                last_id = Some(id);
            }
        }
    }

    Ok(last_id
        .or_else(|| Some(from_image))
        .filter(|_| last_status.is_empty())
        .unwrap_or(last_status))
}

pub async fn remove_image(docker: &Docker, reference: &str, force: bool) -> Result<()> {
    let options = RemoveImageOptionsBuilder::default().force(force).build();
    docker
        .remove_image(reference, Some(options), None)
        .await
        .with_context(|| format!("gagal menghapus image {reference}"))?;
    Ok(())
}

fn split_reference(reference: &str) -> (String, Option<String>) {
    let reference = reference.trim();
    if reference.is_empty() {
        return (String::new(), None);
    }
    if reference.contains('@') {
        return (reference.to_string(), None);
    }
    let last_slash = reference.rfind('/');
    let last_colon = reference.rfind(':');
    if let Some(colon) = last_colon {
        if last_slash.map_or(true, |slash| colon > slash) {
            return (
                reference[..colon].to_string(),
                Some(reference[colon + 1..].to_string()),
            );
        }
    }
    (reference.to_string(), Some("latest".to_string()))
}

fn format_ports(summary: &ContainerSummary) -> String {
    let Some(ports) = &summary.ports else {
        return "—".into();
    };
    let mut mapped = Vec::new();
    let mut exposed = Vec::new();
    for port in ports {
        let protocol = port
            .typ
            .map(|kind| kind.to_string())
            .filter(|kind| !kind.is_empty())
            .unwrap_or_else(|| "tcp".into());
        match port.public_port {
            Some(public) => mapped.push(format!("{public}→{}/{}", port.private_port, protocol)),
            None => exposed.push(format!("{}/{}", port.private_port, protocol)),
        }
    }
    if !mapped.is_empty() {
        mapped.sort();
        mapped.join(", ")
    } else {
        exposed.sort();
        exposed.dedup();
        if exposed.is_empty() {
            "—".into()
        } else {
            exposed.join(", ")
        }
    }
}
