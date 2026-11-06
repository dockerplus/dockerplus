//! Shapes of the Docker Engine and Colima JSON we consume, plus the formatting
//! used to show them.

use serde::{Deserialize, Deserializer};
use std::collections::{BTreeMap, HashMap};

/// Docker sends `null` rather than `[]` for several list fields.
fn null_default<'de, D, T>(de: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(de)?.unwrap_or_default())
}

#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct Container {
    pub id: String,
    #[serde(deserialize_with = "null_default")]
    pub names: Vec<String>,
    pub image: String,
    #[serde(rename = "ImageID")]
    pub image_id: String,
    pub state: String,
    pub status: String,
    pub created: i64,
    #[serde(deserialize_with = "null_default")]
    pub ports: Vec<Port>,
    #[serde(deserialize_with = "null_default")]
    pub mounts: Vec<Mount>,
    #[serde(deserialize_with = "null_default")]
    pub labels: HashMap<String, String>,
    /// Only `networks` is filled in here: the list leaves out ports.
    #[serde(deserialize_with = "null_default")]
    pub network_settings: NetworkSettings,
}

impl Container {
    pub fn name(&self) -> &str {
        self.names.first().map_or(self.id.as_str(), |n| n.trim_start_matches('/'))
    }

    pub fn is_running(&self) -> bool {
        matches!(self.state.as_str(), "running" | "restarting" | "paused")
    }

    pub fn project(&self) -> &str {
        self.labels.get("com.docker.compose.project").map_or("", String::as_str)
    }

    /// Stopped containers stay attached, so they count too.
    pub fn uses_network(&self, network: &Network) -> bool {
        self.network_settings
            .networks
            .iter()
            .any(|(name, endpoint)| endpoint.network_id == network.id || (endpoint.network_id.is_empty() && *name == network.name))
    }
}

#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Port {
    #[serde(rename = "IP")]
    pub ip: String,
    #[serde(rename = "PrivatePort")]
    pub private_port: u16,
    #[serde(rename = "PublicPort")]
    pub public_port: Option<u16>,
    #[serde(rename = "Type")]
    pub kind: String,
}

#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct Mount {
    #[serde(rename = "Type")]
    pub kind: String,
    pub name: String,
    pub source: String,
    pub destination: String,
    #[serde(rename = "RW")]
    pub rw: bool,
}

#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct Image {
    pub id: String,
    #[serde(deserialize_with = "null_default")]
    pub repo_tags: Vec<String>,
    #[serde(deserialize_with = "null_default")]
    pub repo_digests: Vec<String>,
    pub created: i64,
    pub size: i64,
}

#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct Volume {
    pub name: String,
    pub driver: String,
    pub created_at: String,
    pub mountpoint: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct VolumeList {
    #[serde(deserialize_with = "null_default")]
    pub volumes: Vec<Volume>,
}

/// `GET /networks`.
#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct Network {
    pub name: String,
    pub id: String,
    pub created: String,
    pub driver: String,
    #[serde(rename = "IPAM")]
    pub ipam: Ipam,
    #[serde(deserialize_with = "null_default")]
    pub labels: HashMap<String, String>,
}

#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct Ipam {
    #[serde(deserialize_with = "null_default")]
    pub config: Vec<IpamConfig>,
}

#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct IpamConfig {
    pub subnet: String,
}

impl Network {
    /// The engine's own networks, which it refuses to remove.
    pub fn is_builtin(&self) -> bool {
        matches!(self.name.as_str(), "bridge" | "host" | "none")
    }

    pub fn project(&self) -> &str {
        self.labels.get("com.docker.compose.project").map_or("", String::as_str)
    }

    pub fn subnets(&self) -> String {
        let subnets: Vec<&str> = self.ipam.config.iter().map(|c| c.subnet.as_str()).filter(|s| !s.is_empty()).collect();
        subnets.join(", ")
    }
}

/// `GET /system/df`. The summaries are the engine's own, the same numbers
/// `docker system df` prints; shared layers make them hard to reckon from outside.
#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct DiskUsage {
    pub container_usage: UsageSummary,
    pub image_usage: UsageSummary,
    pub volume_usage: UsageSummary,
    pub build_cache_usage: UsageSummary,
    #[serde(deserialize_with = "null_default")]
    pub volumes: Vec<VolumeUsage>,
    #[serde(deserialize_with = "null_default")]
    pub images: Vec<ImageUsage>,
    #[serde(deserialize_with = "null_default")]
    pub containers: Vec<ContainerUsage>,
}

#[derive(Deserialize, Default, Clone, Copy)]
#[serde(rename_all = "PascalCase", default)]
pub struct UsageSummary {
    pub total_count: usize,
    pub active_count: usize,
    pub total_size: i64,
    pub reclaimable: i64,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct VolumeUsage {
    pub name: String,
    pub usage_data: Option<UsageData>,
}

#[derive(Deserialize, Default, Clone, Copy)]
#[serde(rename_all = "PascalCase", default)]
pub struct UsageData {
    pub size: i64,
    pub ref_count: i64,
}

/// One image as `/system/df` lists it, with the containers that use it.
#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct ImageUsage {
    pub id: String,
    #[serde(deserialize_with = "null_default")]
    pub repo_tags: Vec<String>,
    pub containers: i64,
}

/// One container as `/system/df` lists it. `SizeRw` is what removing it frees.
#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct ContainerUsage {
    #[serde(deserialize_with = "null_default")]
    pub names: Vec<String>,
    pub state: String,
    pub size_rw: i64,
}

impl ContainerUsage {
    /// What a container prune takes: anything not running, paused or restarting.
    fn is_stopped(&self) -> bool {
        matches!(self.state.as_str(), "created" | "exited" | "dead")
    }
}

/// What a prune would remove right now: names, with the space each frees where
/// that is known, and the total.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PrunePreview {
    pub items: Vec<(String, Option<i64>)>,
    pub size: i64,
}

/// What can be pruned.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Prune {
    Containers,
    Images,
    Volumes,
    BuildCache,
}

impl Prune {
    pub fn title(self) -> &'static str {
        match self {
            Self::Containers => "Containers",
            Self::Images => "Images",
            Self::Volumes => "Volumes",
            Self::BuildCache => "Build cache",
        }
    }
}

/// One row of the Storage page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StorageLine {
    pub kind: Prune,
    pub items: usize,
    pub active: usize,
    pub size: i64,
    pub reclaimable: i64,
}

impl DiskUsage {
    /// Newer engines leave a few figures out of their summaries: active
    /// containers and what volumes could free. Those come from the lists then.
    pub fn lines(&self) -> Vec<StorageLine> {
        let line = |kind, usage: UsageSummary| StorageLine {
            kind,
            items: usage.total_count,
            active: usage.active_count,
            size: usage.total_size,
            reclaimable: usage.reclaimable,
        };
        let mut containers = line(Prune::Containers, self.container_usage);
        let in_use = self.containers.iter().filter(|c| !c.is_stopped()).count();
        containers.active = containers.active.max(in_use);
        let mut volumes = line(Prune::Volumes, self.volume_usage);
        volumes.reclaimable = volumes.reclaimable.max(self.preview(Prune::Volumes).size);
        vec![
            containers,
            line(Prune::Images, self.image_usage),
            volumes,
            line(Prune::BuildCache, self.build_cache_usage),
        ]
    }

    /// What pruning `kind` would remove, read from a `/system/df` of that kind.
    pub fn preview(&self, kind: Prune) -> PrunePreview {
        match kind {
            Prune::Containers => {
                let stopped: Vec<&ContainerUsage> = self.containers.iter().filter(|c| c.is_stopped()).collect();
                PrunePreview {
                    items: stopped.iter().map(|c| (container_label(c), Some(c.size_rw))).collect(),
                    size: stopped.iter().map(|c| c.size_rw.max(0)).sum(),
                }
            }
            // Images share layers, so one image's size says little about what
            // removing it frees; only the engine's total is given.
            Prune::Images => PrunePreview {
                items: self.images.iter().filter(|i| i.containers == 0).map(|i| (image_label(i), None)).collect(),
                size: self.image_usage.reclaimable,
            },
            Prune::Volumes => {
                let unused: Vec<(&str, i64)> = self
                    .volumes
                    .iter()
                    .filter_map(|v| v.usage_data.filter(|u| u.ref_count == 0).map(|u| (v.name.as_str(), u.size)))
                    .collect();
                PrunePreview {
                    items: unused.iter().map(|(name, size)| (name.to_string(), Some(*size))).collect(),
                    size: unused.iter().map(|(_, size)| size.max(&0)).sum(),
                }
            }
            Prune::BuildCache => PrunePreview { items: Vec::new(), size: self.build_cache_usage.reclaimable },
        }
    }
}

fn container_label(container: &ContainerUsage) -> String {
    container.names.first().map_or("<unnamed>", |name| name.trim_start_matches('/')).to_string()
}

fn image_label(image: &ImageUsage) -> String {
    match image.repo_tags.first() {
        Some(tag) if image.repo_tags.len() > 1 => format!("{tag} (+{} tags)", image.repo_tags.len() - 1),
        Some(tag) => tag.clone(),
        None => format!("<none> {}", short_id(&image.id)),
    }
}

/// Bytes a prune really freed. Size before against size after, because the
/// figure the engine reports can be far off: 115 MB for images that took 426 MB.
pub fn freed_bytes(kind: Prune, before: &[StorageLine], after: &[StorageLine], reported: i64) -> i64 {
    let size = |lines: &[StorageLine]| lines.iter().find(|line| line.kind == kind).map(|line| line.size);
    match (size(before), size(after)) {
        (Some(before), Some(after)) if before > after => before - after,
        _ => reported,
    }
}

/// `GET /containers/{id}/stats`, the fields a live reading needs.
#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct ContainerStats {
    pub cpu_stats: CpuStats,
    pub memory_stats: MemoryStats,
}

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct CpuStats {
    pub cpu_usage: CpuUsage,
    pub system_cpu_usage: u64,
    pub online_cpus: u64,
}

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct CpuUsage {
    pub total_usage: u64,
}

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct MemoryStats {
    pub usage: u64,
    pub limit: u64,
    pub stats: HashMap<String, u64>,
}

impl MemoryStats {
    /// The number `docker stats` shows: page cache the kernel can drop at will
    /// does not count as used.
    pub fn used(&self) -> u64 {
        let cache = self.stats.get("inactive_file").copied().unwrap_or(0);
        self.usage.saturating_sub(cache)
    }
}

/// CPU, memory and limit summed over some containers.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StatsReading {
    /// `None` until a container has two samples to compare.
    pub cpu: Option<f64>,
    pub memory: u64,
    pub limit: u64,
}

/// Turns one-shot stats into readings. The engine reports running totals, so CPU
/// use is the change since the previous sample of the same container.
#[derive(Default)]
pub struct StatsMeter {
    last: HashMap<String, (u64, u64)>,
}

impl StatsMeter {
    pub fn read(&mut self, samples: &[(String, ContainerStats)]) -> StatsReading {
        let mut reading = StatsReading::default();
        let mut seen = HashMap::new();
        for (id, stats) in samples {
            let cpu = &stats.cpu_stats;
            let now = (cpu.cpu_usage.total_usage, cpu.system_cpu_usage);
            if let Some(before) = self.last.get(id) {
                let busy = now.0.saturating_sub(before.0) as f64;
                let total = now.1.saturating_sub(before.1) as f64;
                if total > 0.0 {
                    let percent = busy / total * cpu.online_cpus.max(1) as f64 * 100.0;
                    reading.cpu = Some(reading.cpu.unwrap_or(0.0) + percent);
                }
            }
            seen.insert(id.clone(), now);
            reading.memory += stats.memory_stats.used();
            reading.limit = reading.limit.max(stats.memory_stats.limit);
        }
        self.last = seen;
        reading
    }
}

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "PascalCase", default)]
pub struct EngineVersion {
    pub version: String,
    pub api_version: String,
    pub kernel_version: String,
}

/// `GET /containers/{id}/json`, only the parts the details sheet shows.
#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "PascalCase", default)]
pub struct ContainerDetails {
    pub id: String,
    pub name: String,
    pub created: String,
    pub path: String,
    #[serde(deserialize_with = "null_default")]
    pub args: Vec<String>,
    pub image: String,
    pub state: ContainerState,
    pub config: ContainerConfig,
    pub host_config: HostConfig,
    #[serde(deserialize_with = "null_default")]
    pub mounts: Vec<Mount>,
    pub network_settings: NetworkSettings,
}

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "PascalCase", default)]
pub struct ContainerState {
    pub status: String,
    pub exit_code: i64,
}

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "PascalCase", default)]
pub struct ContainerConfig {
    pub image: String,
    pub working_dir: String,
    #[serde(deserialize_with = "null_default")]
    pub labels: HashMap<String, String>,
}

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "PascalCase", default)]
pub struct HostConfig {
    pub restart_policy: RestartPolicy,
}

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "PascalCase", default)]
pub struct RestartPolicy {
    pub name: String,
}

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "PascalCase", default)]
pub struct NetworkSettings {
    /// "80/tcp" to its host bindings, or null when the port is not published.
    #[serde(deserialize_with = "null_default")]
    pub ports: BTreeMap<String, Option<Vec<PortBinding>>>,
    #[serde(deserialize_with = "null_default")]
    pub networks: BTreeMap<String, Endpoint>,
}

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "PascalCase", default)]
pub struct PortBinding {
    pub host_ip: String,
    pub host_port: String,
}

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Endpoint {
    #[serde(rename = "IPAddress")]
    pub ip_address: String,
    #[serde(rename = "NetworkID")]
    pub network_id: String,
}

/// `colima status --json`.
#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct ColimaStatus {
    pub display_name: String,
    pub driver: String,
    pub arch: String,
    pub runtime: String,
    pub mount_type: String,
    pub ip_address: String,
    pub docker_socket: String,
    pub kubernetes: bool,
    pub cpu: i64,
    pub memory: i64,
    pub disk: i64,
}

/// One line of `colima list --json`.
#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct ColimaProfile {
    pub name: String,
    pub status: String,
    pub arch: String,
    pub cpus: i64,
    pub memory: i64,
    pub disk: i64,
    pub runtime: String,
}

pub fn human_size(bytes: i64) -> String {
    scaled(bytes, 1000.0, ["B", "KB", "MB", "GB", "TB"])
}

/// Memory in binary units, like `docker stats` and the sizes Colima reports.
pub fn human_memory(bytes: i64) -> String {
    scaled(bytes, 1024.0, ["B", "KiB", "MiB", "GiB", "TiB"])
}

fn scaled(bytes: i64, step: f64, units: [&str; 5]) -> String {
    if bytes < 0 {
        return "—".into();
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= step && unit < units.len() - 1 {
        value /= step;
        unit += 1;
    }
    match unit {
        0 => format!("{bytes} B"),
        _ if value >= 100.0 => format!("{value:.0} {}", units[unit]),
        _ => format!("{value:.1} {}", units[unit]),
    }
}

/// Memory and disk sizes that Colima reports in binary units.
pub fn human_gib(bytes: i64) -> String {
    let gib = bytes as f64 / (1u64 << 30) as f64;
    if (gib - gib.round()).abs() < 0.05 { format!("{gib:.0} GiB") } else { format!("{gib:.1} GiB") }
}

pub fn relative_time(now: i64, then: i64) -> String {
    if then <= 0 {
        return "—".into();
    }
    let delta = (now - then).max(0);
    let (value, unit) = match delta {
        0..=59 => return "just now".into(),
        60..=3_599 => (delta / 60, "min"),
        3_600..=86_399 => (delta / 3_600, "hour"),
        86_400..=2_591_999 => (delta / 86_400, "day"),
        2_592_000..=31_535_999 => (delta / 2_592_000, "month"),
        _ => (delta / 31_536_000, "year"),
    };
    format!("{value} {unit}{} ago", if value == 1 { "" } else { "s" })
}

pub fn ports_text(ports: &[Port]) -> String {
    let mut parts: Vec<String> = ports
        .iter()
        .map(|p| match p.public_port {
            Some(public) => format!("{public}→{}/{}", p.private_port, p.kind),
            None => format!("{}/{}", p.private_port, p.kind),
        })
        .collect();
    // Docker lists IPv4 and IPv6 bindings separately; show each mapping once.
    parts.sort();
    parts.dedup();
    parts.join(", ")
}

pub fn short_id(id: &str) -> &str {
    let hex = id.strip_prefix("sha256:").unwrap_or(id);
    &hex[..hex.len().min(12)]
}

/// Splits `repo:tag`, taking care of registries with a port (`host:5000/app`).
pub fn split_image_ref(reference: &str) -> (&str, &str) {
    let name_start = reference.rfind('/').map_or(0, |i| i + 1);
    match reference[name_start..].rfind(':') {
        Some(i) => (&reference[..name_start + i], &reference[name_start + i + 1..]),
        None => (reference, ""),
    }
}

/// Parses Docker's RFC 3339 `CreatedAt` into Unix seconds, without pulling in a
/// date library for this one use.
pub fn parse_rfc3339(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let num = |range: std::ops::Range<usize>| text.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);

    let tail = text[19..].trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    let offset = match tail.as_bytes().first() {
        Some(sign @ (b'+' | b'-')) => {
            let hours = tail.get(1..3)?.parse::<i64>().ok()?;
            let minutes = tail.get(4..6)?.parse::<i64>().ok()?;
            (hours * 3600 + minutes * 60) * if *sign == b'+' { 1 } else { -1 }
        }
        _ => 0,
    };

    // Days from civil, Howard Hinnant's algorithm.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_json_with_nulls() {
        let json = r#"{"Id":"abc","Names":["/web"],"Image":"nginx:alpine","ImageID":"sha256:1",
            "State":"running","Status":"Up 2 minutes","Created":1700000000,
            "Ports":[{"IP":"0.0.0.0","PrivatePort":80,"PublicPort":18080,"Type":"tcp"},
                     {"IP":"::","PrivatePort":80,"PublicPort":18080,"Type":"tcp"},
                     {"PrivatePort":443,"Type":"tcp"}],
            "Labels":{"com.docker.compose.project":"demo"},"Mounts":null}"#;
        let c: Container = serde_json::from_str(json).unwrap();
        assert_eq!(c.name(), "web");
        assert!(c.is_running());
        assert_eq!(c.project(), "demo");
        assert_eq!(ports_text(&c.ports), "18080→80/tcp, 443/tcp");
    }

    #[test]
    fn networks_and_their_containers() {
        let json = r#"[{"Name":"shop_default","Id":"n1","Created":"2026-10-22T03:44:17.988366422Z","Driver":"bridge",
            "IPAM":{"Driver":"default","Options":null,"Config":[{"Subnet":"172.18.0.0/16","Gateway":"172.18.0.1"}]},
            "Labels":{"com.docker.compose.project":"shop"}},
            {"Name":"host","Id":"n2","Driver":"host","IPAM":{"Config":null},"Labels":{}}]"#;
        let networks: Vec<Network> = serde_json::from_str(json).unwrap();
        assert_eq!((networks[0].subnets(), networks[0].project()), ("172.18.0.0/16".to_string(), "shop"));
        assert_eq!(networks[1].subnets(), "");
        assert!(networks[1].is_builtin() && !networks[0].is_builtin());

        let c: Container = serde_json::from_str(
            r#"{"Id":"c","State":"exited","NetworkSettings":{"Networks":{"shop_default":{"NetworkID":"n1","IPAddress":""}}}}"#,
        )
        .unwrap();
        assert!(c.uses_network(&networks[0]) && !c.uses_network(&networks[1]));
        let bare: Container = serde_json::from_str(r#"{"Id":"d","NetworkSettings":null}"#).unwrap();
        assert!(!bare.uses_network(&networks[0]));
    }

    #[test]
    fn image_with_null_tags_is_dangling() {
        let i: Image = serde_json::from_str(r#"{"Id":"sha256:abcdef0123456789","RepoTags":null,"Created":1,"Size":1000}"#).unwrap();
        assert!(i.repo_tags.is_empty());
        assert_eq!(short_id(&i.id), "abcdef012345");
    }

    #[test]
    fn empty_volume_list_is_null() {
        let list: VolumeList = serde_json::from_str(r#"{"Volumes":null,"Warnings":null}"#).unwrap();
        assert!(list.volumes.is_empty());
    }

    #[test]
    fn storage_reads_the_engine_summaries() {
        let json = r#"{"ImageUsage":{"ActiveCount":1,"Reclaimable":414,"TotalCount":3,"TotalSize":440,"Items":[]},
            "ContainerUsage":{"TotalCount":2,"TotalSize":8},
            "VolumeUsage":{},
            "BuildCacheUsage":{"Reclaimable":1,"TotalCount":4,"TotalSize":7},
            "Volumes":null}"#;
        let usage: DiskUsage = serde_json::from_str(json).unwrap();
        let lines = usage.lines();
        assert_eq!((lines[1].items, lines[1].active, lines[1].size, lines[1].reclaimable), (3, 1, 440, 414));
        assert_eq!((lines[0].items, lines[0].active, lines[0].size), (2, 0, 8));
        assert_eq!(lines[2].items, 0);
        assert_eq!(lines[3].size, 7);
    }

    /// The shape Docker Engine 29.9 sends: no active count for containers and
    /// nothing reclaimable for volumes in the summaries.
    #[test]
    fn missing_summary_figures_come_from_the_lists() {
        let json = r#"{"ContainerUsage":{"Reclaimable":5,"TotalCount":2,"TotalSize":9},
            "VolumeUsage":{"ActiveCount":1,"TotalCount":3,"TotalSize":700},
            "Containers":[{"Names":["/web"],"State":"running","SizeRw":4},{"Names":["/old"],"State":"exited","SizeRw":5}],
            "Volumes":[{"Name":"used","UsageData":{"RefCount":1,"Size":100}},
                       {"Name":"data","UsageData":{"RefCount":0,"Size":500}},
                       {"Name":"empty","UsageData":{"RefCount":0,"Size":-1}}]}"#;
        let usage: DiskUsage = serde_json::from_str(json).unwrap();
        let lines = usage.lines();
        assert_eq!(lines[0].active, 1);
        assert_eq!(lines[2].reclaimable, 500);
    }

    #[test]
    fn previews_name_what_a_prune_takes() {
        let json = r#"{"ImageUsage":{"Reclaimable":300},
            "Images":[{"Id":"sha256:aaaaaaaaaaaaaaaa","RepoTags":["nginx:alpine","web:1"],"Containers":0},
                      {"Id":"sha256:bbbbbbbbbbbbbbbb","RepoTags":null,"Containers":0},
                      {"Id":"sha256:cccccccccccccccc","RepoTags":["db:16"],"Containers":2}],
            "Containers":[{"Names":["/old"],"State":"exited","SizeRw":5},{"Names":["/up"],"State":"paused","SizeRw":7}],
            "Volumes":[{"Name":"data","UsageData":{"RefCount":0,"Size":500}},{"Name":"used","UsageData":{"RefCount":2,"Size":9}}]}"#;
        let usage: DiskUsage = serde_json::from_str(json).unwrap();

        let images = usage.preview(Prune::Images);
        let names: Vec<&str> = images.items.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["nginx:alpine (+1 tags)", "<none> bbbbbbbbbbbb"]);
        assert_eq!(images.size, 300);
        assert_eq!(usage.preview(Prune::Containers).items, [("old".to_string(), Some(5))]);
        assert_eq!(usage.preview(Prune::Volumes), PrunePreview { items: vec![("data".into(), Some(500))], size: 500 });
        assert!(usage.preview(Prune::BuildCache).items.is_empty());
    }

    #[test]
    fn freed_space_is_measured_not_taken_from_the_report() {
        let line = |size| StorageLine { kind: Prune::Images, items: 1, active: 0, size, reclaimable: 0 };
        assert_eq!(freed_bytes(Prune::Images, &[line(440)], &[line(14)], 115), 426);
        assert_eq!(freed_bytes(Prune::Images, &[line(440)], &[line(440)], 7), 7);
        assert_eq!(freed_bytes(Prune::Images, &[], &[line(14)], 7), 7);
    }

    fn stats(busy: u64, system: u64, used: u64) -> ContainerStats {
        ContainerStats {
            cpu_stats: CpuStats { cpu_usage: CpuUsage { total_usage: busy }, system_cpu_usage: system, online_cpus: 2 },
            memory_stats: MemoryStats {
                usage: used + 50,
                limit: 2_000,
                stats: HashMap::from([("inactive_file".to_string(), 50)]),
            },
        }
    }

    #[test]
    fn cpu_needs_two_samples_and_scales_by_core() {
        let mut meter = StatsMeter::default();
        let first = meter.read(&[("a".into(), stats(100, 1_000, 300))]);
        assert_eq!((first.cpu, first.memory, first.limit), (None, 300, 2_000));
        let second = meter.read(&[("a".into(), stats(150, 2_000, 300))]);
        assert_eq!(second.cpu, Some(10.0));
    }

    #[test]
    fn readings_add_up_across_containers() {
        let mut meter = StatsMeter::default();
        meter.read(&[("a".into(), stats(0, 0, 1)), ("b".into(), stats(0, 0, 2))]);
        let both = meter.read(&[("a".into(), stats(10, 100, 1)), ("b".into(), stats(5, 100, 2))]);
        assert_eq!((both.cpu, both.memory), (Some(30.0), 3));
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1_500), "1.5 KB");
        assert_eq!(human_size(245_000_000), "245 MB");
        assert_eq!(human_size(-1), "—");
        assert_eq!(human_gib(2_147_483_648), "2 GiB");
        assert_eq!(human_gib(107_374_182_400), "100 GiB");
        assert_eq!(human_memory(120_000_000), "114 MiB");
        assert_eq!(human_memory(1 << 30), "1.0 GiB");
    }

    #[test]
    fn relative_times() {
        let now = 1_000_000;
        assert_eq!(relative_time(now, now - 5), "just now");
        assert_eq!(relative_time(now, now - 60), "1 min ago");
        assert_eq!(relative_time(now, now - 7_200), "2 hours ago");
        assert_eq!(relative_time(now, now - 86_400 * 3), "3 days ago");
        assert_eq!(relative_time(now, 0), "—");
    }

    #[test]
    fn image_refs() {
        assert_eq!(split_image_ref("nginx:alpine"), ("nginx", "alpine"));
        assert_eq!(split_image_ref("nginx"), ("nginx", ""));
        assert_eq!(split_image_ref("host:5000/app"), ("host:5000/app", ""));
        assert_eq!(split_image_ref("host:5000/team/app:v2"), ("host:5000/team/app", "v2"));
    }

    #[test]
    fn rfc3339() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2026-11-06T03:25:40Z"), Some(1_793_935_540));
        assert_eq!(parse_rfc3339("2026-11-06T10:25:40+07:00"), Some(1_793_935_540));
        assert_eq!(parse_rfc3339("2026-11-06T03:25:40.123456789Z"), Some(1_793_935_540));
        assert_eq!(parse_rfc3339("nonsense"), None);
    }
}
