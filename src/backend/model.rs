//! Shapes of the Docker Engine and Colima JSON we consume, plus the formatting
//! used to show them.

use serde::{Deserialize, Deserializer};
use std::collections::HashMap;

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

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct DiskUsage {
    #[serde(deserialize_with = "null_default")]
    pub volumes: Vec<VolumeUsage>,
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
}

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "PascalCase", default)]
pub struct EngineVersion {
    pub version: String,
    pub api_version: String,
    pub kernel_version: String,
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
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 0 {
        return "—".into();
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    match unit {
        0 => format!("{bytes} B"),
        _ if value >= 100.0 => format!("{value:.0} {}", UNITS[unit]),
        _ => format!("{value:.1} {}", UNITS[unit]),
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
    fn sizes() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1_500), "1.5 KB");
        assert_eq!(human_size(245_000_000), "245 MB");
        assert_eq!(human_size(-1), "—");
        assert_eq!(human_gib(2_147_483_648), "2 GiB");
        assert_eq!(human_gib(107_374_182_400), "100 GiB");
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
