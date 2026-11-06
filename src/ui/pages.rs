//! Turns Docker objects into table rows. Pure data, so it needs no window.

use super::table::{ARROW_CLOSED, ARROW_OPEN, Cell, Column, Row, Tone};
use crate::backend::model::*;
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Page {
    #[default]
    Colima,
    Containers,
    Images,
    Volumes,
    Networks,
    Storage,
}

impl Page {
    pub fn title(self) -> &'static str {
        match self {
            Self::Colima => "Colima",
            Self::Containers => "Containers",
            Self::Images => "Images",
            Self::Volumes => "Volumes",
            Self::Networks => "Networks",
            Self::Storage => "Storage",
        }
    }

    /// The Colima page sits under a "Colima" heading in the sidebar.
    pub fn sidebar_title(self) -> &'static str {
        match self {
            Self::Colima => "Overview",
            other => other.title(),
        }
    }
}

pub const CONTAINER_COLUMNS: &[Column] = &[
    Column::new("", 22.0, 22.0).fixed(),
    Column::new("Name", 180.0, 90.0),
    Column::new("Image", 190.0, 90.0),
    Column::new("Status", 170.0, 70.0),
    Column::new("Ports", 150.0, 50.0).mono(),
    Column::new("Created", 96.0, 60.0),
];

pub const IMAGE_COLUMNS: &[Column] = &[
    Column::new("Repository", 260.0, 90.0),
    Column::new("Tag", 110.0, 50.0),
    Column::new("ID", 110.0, 70.0).mono(),
    Column::new("Size", 80.0, 50.0),
    Column::new("Created", 100.0, 60.0),
    Column::new("Used by", 70.0, 50.0),
];

pub const VOLUME_COLUMNS: &[Column] = &[
    Column::new("Name", 320.0, 110.0),
    Column::new("Driver", 80.0, 50.0),
    Column::new("Size", 80.0, 50.0),
    Column::new("Used by", 180.0, 60.0),
    Column::new("Created", 100.0, 60.0),
];

pub const NETWORK_COLUMNS: &[Column] = &[
    Column::new("Name", 240.0, 110.0),
    Column::new("Driver", 80.0, 50.0),
    Column::new("Subnet", 140.0, 70.0).mono(),
    Column::new("Containers", 80.0, 50.0),
    Column::new("Project", 120.0, 60.0),
    Column::new("Created", 100.0, 60.0),
];

pub const STORAGE_COLUMNS: &[Column] = &[
    Column::new("Type", 220.0, 120.0),
    Column::new("Items", 90.0, 50.0),
    Column::new("In use", 90.0, 50.0),
    Column::new("Size", 110.0, 60.0),
    Column::new("Can be freed", 120.0, 60.0),
];

const PROJECT_PREFIX: &str = "project:";

/// The row key of a Compose project's header.
pub fn project_key(name: &str) -> String {
    format!("{PROJECT_PREFIX}{name}")
}

pub fn project_of_key(key: &str) -> Option<&str> {
    key.strip_prefix(PROJECT_PREFIX)
}

fn status_dot(container: &Container) -> Cell {
    let tone = match container.state.as_str() {
        "running" => Tone::Green,
        "paused" | "restarting" | "created" | "removing" => Tone::Orange,
        "dead" => Tone::Red,
        _ if failed_exit(container) => Tone::Red,
        _ => Tone::Gray,
    };
    let rank = match tone {
        Tone::Green => 0,
        Tone::Orange => 1,
        Tone::Red => 2,
        _ => 3,
    };
    Cell { text: "●".into(), sort: Some(rank), tone }
}

/// "Exited (137) 2 minutes ago" is a failure, "Exited (0) …" is not.
fn failed_exit(container: &Container) -> bool {
    container.state == "exited" && !container.status.starts_with("Exited (0)")
}

pub fn container_rows<'a>(containers: impl IntoIterator<Item = &'a Container>, now: i64) -> Vec<Row> {
    containers
        .into_iter()
        .map(|c| {
            Row::new(c.id.clone(), vec![
                status_dot(c),
                Cell::text(c.name()),
                Cell::text(&c.image),
                Cell::text(&c.status),
                Cell::dim(&ports_text(&c.ports)),
                Cell::dim(&relative_time(now, c.created)).sorted_by(c.created),
            ])
        })
        .collect()
}

const NAME_COLUMN: usize = 1;
const INDENT: &str = "    ";

/// Puts container rows under a header for each Compose project, then the rest
/// under their own. With no projects at all, the list stays as it was. Counts
/// come from every container, so a filter cannot make a project look smaller.
pub fn group_by_project(rows: Vec<Row>, containers: &[Container], collapsed: &HashSet<String>) -> Vec<Row> {
    let mut projects: BTreeMap<&str, Vec<Row>> = BTreeMap::new();
    let mut others = Vec::new();
    for mut row in rows {
        let project = containers.iter().find(|c| c.id == row.key).map_or("", Container::project);
        if project.is_empty() {
            others.push(row);
        } else {
            row.cells[NAME_COLUMN].text.insert_str(0, INDENT);
            projects.entry(project).or_default().push(row);
        }
    }
    if projects.is_empty() {
        return others;
    }

    let mut grouped = Vec::new();
    for (name, children) in projects {
        let open = !collapsed.contains(name);
        let members: Vec<&Container> = containers.iter().filter(|c| c.project() == name).collect();
        grouped.push(project_header(name, &members, open));
        if open {
            grouped.extend(children);
        }
    }
    if !others.is_empty() {
        let blank = || Cell::dim("");
        grouped.push(Row::header("", vec![
            blank(),
            Cell { text: "Other containers".to_string(), sort: None, tone: Tone::Strong },
            blank(),
            blank(),
            blank(),
            blank(),
        ]));
        grouped.extend(others.into_iter().map(|mut row| {
            row.cells[NAME_COLUMN].text.insert_str(0, INDENT);
            row
        }));
    }
    grouped
}

fn project_header(name: &str, members: &[&Container], open: bool) -> Row {
    let running = members.iter().filter(|c| c.is_running()).count();
    // A one-shot service that finished cleanly is not a problem to flag.
    let unwell = members.iter().any(|c| !c.is_running() && !c.status.starts_with("Exited (0)"));
    let tone = match (running, unwell) {
        (0, _) => Tone::Dim,
        (_, true) => Tone::Orange,
        (_, false) => Tone::Green,
    };
    let count = format!("{} {}", members.len(), plural(members.len(), "container"));
    Row::header(project_key(name), vec![
        Cell::dim(if open { ARROW_OPEN } else { ARROW_CLOSED }),
        Cell { text: name.to_string(), sort: None, tone: Tone::Strong },
        Cell::dim(&count),
        Cell { text: format!("{running} of {} running", members.len()), sort: None, tone },
        Cell::dim(""),
        Cell::dim(""),
    ])
}

pub fn storage_rows(lines: &[StorageLine]) -> Vec<Row> {
    lines
        .iter()
        .map(|line| {
            Row::new(line.kind.title(), vec![
                Cell::text(line.kind.title()),
                Cell::dim(&line.items.to_string()).sorted_by(line.items as i64),
                Cell::dim(&line.active.to_string()).sorted_by(line.active as i64),
                Cell::text(&human_size(line.size)).sorted_by(line.size),
                Cell::text(&human_size(line.reclaimable)).sorted_by(line.reclaimable),
            ])
        })
        .collect()
}

pub fn storage_summary(lines: &[StorageLine], on_mac: Option<u64>) -> String {
    let freeable: i64 = lines.iter().map(|line| line.reclaimable).sum();
    let mut text = format!("{} can be freed", human_size(freeable));
    if let Some(bytes) = on_mac {
        text.push_str(&format!(", VM disk takes {} on this Mac", human_size(bytes as i64)));
    }
    text
}

/// Title and details for the confirmation before pruning, naming what goes.
/// None when there is nothing to remove.
pub fn prune_prompt(kind: Prune, preview: &PrunePreview) -> Option<(String, String)> {
    let count = preview.items.len();
    let size = human_size(preview.size);
    let shown: Vec<(String, Option<i64>)> = preview.items.iter().map(|item| (item.name.clone(), item.size)).collect();
    let list = name_list(&shown);
    let prompt = match kind {
        _ if count == 0 && kind != Prune::BuildCache => return None,
        Prune::BuildCache if preview.size <= 0 => return None,
        Prune::Containers => (
            "Remove stopped containers?".to_string(),
            format!("{count} stopped {}, {size}:\n{list}", plural(count, "container")),
        ),
        Prune::Images => (
            "Remove unused images?".to_string(),
            format!(
                "{count} {} no container uses, tagged ones included, {size}:\n{list}\n\nThey can be pulled again.",
                plural(count, "image")
            ),
        ),
        Prune::Volumes => (
            "Remove unused volumes?".to_string(),
            format!(
                "{count} {} no container uses, named ones included, {size}:\n{list}\n\nTheir data cannot be recovered.",
                plural(count, "volume")
            ),
        ),
        Prune::BuildCache => {
            ("Clear the build cache?".to_string(), format!("{size}. The next builds will take longer."))
        }
    };
    Some(prompt)
}

/// What to say when a prune would remove nothing.
pub fn prune_nothing(kind: Prune) -> (String, String) {
    let detail = match kind {
        Prune::Containers => "No container is stopped.",
        Prune::Images => "Every image is used by a container.",
        Prune::Volumes => "Every volume is used by a container.",
        Prune::BuildCache => "The build cache is empty.",
    };
    ("Nothing to remove".to_string(), detail.to_string())
}

/// A bulleted list for a prompt, sizes where known, the long tail summed up.
fn name_list(items: &[(String, Option<i64>)]) -> String {
    const SHOWN: usize = 8;
    let mut lines: Vec<String> = items
        .iter()
        .take(SHOWN)
        .map(|(name, size)| match size {
            Some(bytes) if *bytes > 0 => format!("• {name} ({})", human_size(*bytes)),
            _ => format!("• {name}"),
        })
        .collect();
    if items.len() > SHOWN {
        lines.push(format!("• and {} more", items.len() - SHOWN));
    }
    lines.join("\n")
}

/// One message for a batch in which every item was tried. A lone item keeps
/// the engine's own words; a batch names each item that failed and why.
pub fn batch_error(total: usize, failures: &[(String, String)]) -> Option<String> {
    const SHOWN: usize = 6;
    match failures {
        [] => None,
        [(_, error)] if total == 1 => Some(error.clone()),
        _ => {
            let mut text = format!("{} of {total} could not be done:", failures.len());
            for (name, error) in failures.iter().take(SHOWN) {
                text.push_str(&format!("\n• {name}: {error}"));
            }
            if failures.len() > SHOWN {
                text.push_str(&format!("\n• and {} more", failures.len() - SHOWN));
            }
            Some(text)
        }
    }
}

/// The freed space stays with the VM: its disk file on the Mac never shrinks,
/// not even after `fstrim`. Saying "freed" alone would promise more than that.
pub fn prune_result(bytes: i64) -> (String, String) {
    (
        format!("{} freed inside the VM", human_size(bytes)),
        "New images and containers reuse this space. The VM's disk file on this Mac keeps its current size.".to_string(),
    )
}

pub fn stats_text(label: &str, reading: &StatsReading) -> String {
    let cpu = reading.cpu.map_or("CPU …".to_string(), |cpu| format!("CPU {cpu:.1}%"));
    let memory = human_memory(reading.memory as i64);
    if reading.limit == 0 {
        return format!("{label}: {cpu}, RAM {memory}");
    }
    format!("{label}: {cpu}, RAM {memory} of {}", human_gib(reading.limit as i64))
}

pub fn vm_usage_text(used: u64, total: u64) -> String {
    let percent = (used * 100).checked_div(total).unwrap_or(0);
    format!("{} of {} ({percent}%)", human_memory(used as i64), human_gib(total as i64))
}

/// Said from here on, since pulls and builds start failing soon after.
const VM_DISK_NEARLY_FULL: u64 = 90;

pub fn vm_disk_warning(disk: Option<(u64, u64)>) -> Option<String> {
    let (used, total) = disk?;
    let percent = (used * 100).checked_div(total)?;
    (percent >= VM_DISK_NEARLY_FULL).then(|| format!("VM disk {percent}% full"))
}

pub fn image_rows(images: &[Image], containers: &[Container], now: i64) -> Vec<Row> {
    let mut rows = Vec::new();
    for image in images {
        let users = containers.iter().filter(|c| c.image_id == image.id).count();
        let row = |key: String, repository: &str, tag: &str| {
            Row::new(key, vec![
                Cell::text(repository),
                Cell::text(tag),
                Cell::dim(short_id(&image.id)),
                Cell::text(&human_size(image.size)).sorted_by(image.size),
                Cell::dim(&relative_time(now, image.created)).sorted_by(image.created),
                Cell::dim(&if users == 0 { String::new() } else { users.to_string() })
                    .sorted_by(users as i64),
            ])
        };
        // One row per tag, like `docker images`, so removing a row drops a tag.
        if image.repo_tags.is_empty() {
            let digest_repo = image.repo_digests.first().map(|d| d.split('@').next().unwrap_or(d));
            rows.push(row(image.id.clone(), digest_repo.unwrap_or("<none>"), "<none>"));
        }
        for tag in &image.repo_tags {
            let (repository, name) = split_image_ref(tag);
            rows.push(row(tag.clone(), repository, name));
        }
    }
    rows
}

pub fn volume_rows(
    volumes: &[Volume],
    containers: &[Container],
    sizes: &HashMap<String, i64>,
    now: i64,
) -> Vec<Row> {
    volumes
        .iter()
        .map(|v| {
            let users: Vec<&str> = containers
                .iter()
                .filter(|c| c.mounts.iter().any(|m| m.kind == "volume" && m.name == v.name))
                .map(Container::name)
                .collect();
            let size = sizes.get(&v.name).copied();
            let created = parse_rfc3339(&v.created_at).unwrap_or(0);
            Row::new(v.name.clone(), vec![
                Cell::text(&v.name),
                Cell::dim(&v.driver),
                Cell::text(&size.map_or("—".to_string(), human_size)).sorted_by(size.unwrap_or(-1)),
                Cell::dim(&users.join(", ")),
                Cell::dim(&relative_time(now, created)).sorted_by(created),
            ])
        })
        .collect()
}

pub fn containers_summary(containers: &[Container]) -> String {
    let running = containers.iter().filter(|c| c.is_running()).count();
    format!("{} {}, {running} running", containers.len(), plural(containers.len(), "container"))
}

pub fn images_summary(images: &[Image]) -> String {
    let total: i64 = images.iter().map(|i| i.size).sum();
    format!("{} {}, {}", images.len(), plural(images.len(), "image"), human_size(total))
}

pub fn volumes_summary(volumes: &[Volume]) -> String {
    format!("{} {}", volumes.len(), plural(volumes.len(), "volume"))
}

/// Keyed by ID: Docker allows two networks with one name.
pub fn network_rows(networks: &[Network], containers: &[Container], now: i64) -> Vec<Row> {
    networks
        .iter()
        .map(|n| {
            let users = containers.iter().filter(|c| c.uses_network(n)).count();
            let created = parse_rfc3339(&n.created).unwrap_or(0);
            Row::new(n.id.clone(), vec![
                Cell::text(&n.name),
                Cell::dim(&n.driver),
                Cell::dim(&n.subnets()),
                Cell::dim(&if users == 0 { String::new() } else { users.to_string() }).sorted_by(users as i64),
                Cell::dim(n.project()),
                Cell::dim(&relative_time(now, created)).sorted_by(created),
            ])
        })
        .collect()
}

pub fn networks_summary(networks: &[Network]) -> String {
    format!("{} {}", networks.len(), plural(networks.len(), "network"))
}

/// Networks someone made that no container, running or stopped, is attached
/// to. Chosen here rather than by the engine's prune, so the confirmation can
/// name exactly what goes.
pub fn unused_networks<'a>(networks: &'a [Network], containers: &[Container]) -> Vec<&'a Network> {
    networks
        .iter()
        .filter(|n| !n.is_builtin() && !containers.iter().any(|c| c.uses_network(n)))
        .collect()
}

/// Title and details for removing the unused networks, naming each one.
pub fn network_prune_prompt(unused: &[&Network]) -> (String, String) {
    let items: Vec<(String, Option<i64>)> = unused.iter().map(|n| (n.name.clone(), None)).collect();
    (
        format!("Remove {} unused {}?", unused.len(), plural(unused.len(), "network")),
        format!("{}\n\nNo container, running or stopped, is attached to them.", name_list(&items)),
    )
}

pub fn plural(count: usize, noun: &str) -> String {
    if count == 1 { noun.to_string() } else { format!("{noun}s") }
}

/// Published TCP ports as menu title and URL, once each. Colima forwards them
/// to the Mac, so a port bound on all interfaces answers on localhost.
pub fn port_links(ports: &[Port]) -> Vec<(String, String)> {
    let mut links: Vec<(u16, String, String)> = ports
        .iter()
        .filter(|p| p.kind == "tcp")
        .filter_map(|p| {
            let public = p.public_port?;
            let host = match p.ip.as_str() {
                "" | "0.0.0.0" | "::" => "localhost".to_string(),
                ip if ip.contains(':') => format!("[{ip}]"),
                ip => ip.to_string(),
            };
            let scheme = if p.private_port == 443 { "https" } else { "http" };
            Some((public, format!("{host}:{public} → {}", p.private_port), format!("{scheme}://{host}:{public}")))
        })
        .collect();
    links.sort();
    links.dedup();
    links.into_iter().map(|(_, title, url)| (title, url)).collect()
}

/// Bind mounts as menu title and host path.
pub fn bind_mounts(mounts: &[Mount]) -> Vec<(String, String)> {
    mounts
        .iter()
        .filter(|m| m.kind == "bind" && !m.source.is_empty())
        .map(|m| (format!("{} → {}", m.source, m.destination), m.source.clone()))
        .collect()
}

/// The rows of the details sheet. A row with an empty label continues the one
/// above it.
pub fn container_details(d: &ContainerDetails, digests: &[String], now: i64) -> Vec<(&'static str, String)> {
    let mut lines = Vec::new();
    let mut add = |label: &'static str, values: Vec<String>| {
        for (i, value) in values.into_iter().filter(|v| !v.is_empty()).enumerate() {
            lines.push((if i == 0 { label } else { "" }, value));
        }
    };
    let status = match d.state.status.as_str() {
        "exited" => format!("exited ({})", d.state.exit_code),
        other => other.to_string(),
    };
    let command: Vec<&str> = std::iter::once(d.path.as_str()).chain(d.args.iter().map(String::as_str)).collect();
    let restart = match d.host_config.restart_policy.name.as_str() {
        "" | "no" => String::new(),
        name => name.to_string(),
    };
    let project = d.config.labels.get("com.docker.compose.project").cloned().unwrap_or_default();

    add("Name", vec![d.name.trim_start_matches('/').to_string()]);
    add("ID", vec![d.id.clone()]);
    add("Image", vec![d.config.image.clone()]);
    add("Image ID", vec![d.image.clone()]);
    add("Digest", digests.to_vec());
    add("Status", vec![status]);
    add("Created", vec![parse_rfc3339(&d.created).map_or(String::new(), |t| relative_time(now, t))]);
    add("Command", vec![command.join(" ").trim().to_string()]);
    add("Workdir", vec![d.config.working_dir.clone()]);
    add("Restart", vec![restart]);
    add("Project", vec![project]);
    add("Ports", port_lines(&d.network_settings.ports));
    add("Mounts", d.mounts.iter().map(mount_line).collect());
    add(
        "Networks",
        d.network_settings.networks.iter().map(|(name, e)| format!("{name} {}", e.ip_address).trim_end().to_string()).collect(),
    );
    lines
}

fn port_lines(ports: &BTreeMap<String, Option<Vec<PortBinding>>>) -> Vec<String> {
    let mut lines = Vec::new();
    for (inside, bindings) in ports {
        let mut outside: Vec<String> = bindings
            .iter()
            .flatten()
            .map(|b| match b.host_ip.as_str() {
                "" | "0.0.0.0" | "::" => b.host_port.clone(),
                ip => format!("{ip}:{}", b.host_port),
            })
            .collect();
        outside.sort();
        outside.dedup();
        if outside.is_empty() {
            lines.push(inside.clone());
        }
        lines.extend(outside.into_iter().map(|o| format!("{o} → {inside}")));
    }
    lines
}

fn mount_line(m: &Mount) -> String {
    let from = if m.kind == "volume" { &m.name } else { &m.source };
    let mode = if m.rw { "" } else { " (read-only)" };
    format!("{} {from} → {}{mode}", m.kind, m.destination)
}

/// Lays the rows out in two aligned columns for a monospaced view.
pub fn details_text(lines: &[(&str, String)]) -> String {
    let width = lines.iter().map(|(label, _)| label.chars().count()).max().unwrap_or(0) + 2;
    lines.iter().map(|(label, value)| format!("{label:<width$}{value}")).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn container(name: &str, state: &str, status: &str) -> Container {
        Container {
            id: format!("id-{name}"),
            names: vec![format!("/{name}")],
            state: state.into(),
            status: status.into(),
            image_id: "sha256:one".into(),
            ..Default::default()
        }
    }

    fn member(name: &str, project: &str, state: &str) -> Container {
        let mut c = container(name, state, "");
        if !project.is_empty() {
            c.labels.insert("com.docker.compose.project".into(), project.into());
        }
        c
    }

    #[test]
    fn containers_group_under_their_projects_then_the_rest() {
        let all = [member("web", "shop", "running"), member("solo", "", "running"), member("db", "shop", "exited")];
        let rows = group_by_project(container_rows(&all, 100), &all, &HashSet::new());
        let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, ["project:shop", "id-web", "id-db", "", "id-solo"]);
        assert_eq!(rows[0].cells[3].text, "1 of 2 running");
        assert_eq!(rows[0].cells[3].tone, Tone::Orange);
        assert_eq!(rows[1].cells[1].text.trim(), "web");
        assert!(rows[0].header && rows[3].header && !rows[1].header);
    }

    #[test]
    fn a_finished_one_shot_service_is_not_a_warning() {
        let mut init = member("init", "shop", "exited");
        init.status = "Exited (0) 2 minutes ago".into();
        let all = [member("web", "shop", "running"), init];
        let rows = group_by_project(container_rows(&all, 100), &all, &HashSet::new());
        assert_eq!(rows[0].cells[3].text, "1 of 2 running");
        assert_eq!(rows[0].cells[3].tone, Tone::Green);
    }

    #[test]
    fn without_projects_the_list_stays_flat() {
        let all = [member("a", "", "running"), member("b", "", "exited")];
        let rows = container_rows(&all, 100);
        assert_eq!(group_by_project(rows.clone(), &all, &HashSet::new()), rows);
    }

    #[test]
    fn a_collapsed_project_keeps_its_header_only() {
        let all = [member("web", "shop", "running"), member("db", "shop", "running")];
        let rows = group_by_project(container_rows(&all, 100), &all, &HashSet::from(["shop".to_string()]));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cells[0].text, "▸");
        assert_eq!(rows[0].cells[3].tone, Tone::Green);
    }

    #[test]
    fn a_filter_cannot_shrink_a_project_header() {
        let all = [member("web", "shop", "running"), member("db", "shop", "exited")];
        let rows = crate::ui::table::filter_rows(container_rows(&all, 100), "web");
        let rows = group_by_project(rows, &all, &HashSet::new());
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].cells[2].text, "2 containers");
    }

    #[test]
    fn project_keys_round_trip() {
        assert_eq!(project_of_key(&project_key("shop")), Some("shop"));
        assert_eq!(project_of_key("abc123"), None);
    }

    #[test]
    fn the_prune_result_does_not_promise_space_on_the_mac() {
        let (title, detail) = prune_result(405_000_000);
        assert_eq!(title, "405 MB freed inside the VM");
        assert!(detail.contains("keeps its current size"));
    }

    #[test]
    fn volume_prompts_warn_about_data() {
        let item = |name: &str, size| PruneItem { name: name.into(), size: Some(size), targets: Vec::new() };
        let preview = PrunePreview { items: vec![item("data", 1_000), item("cache", 0)], size: 1_000 };
        let (title, detail) = prune_prompt(Prune::Volumes, &preview).unwrap();
        assert_eq!(title, "Remove unused volumes?");
        assert!(detail.starts_with("2 volumes no container uses, named ones included, 1.0 KB:\n• data (1.0 KB)\n• cache\n"));
        assert!(detail.contains("cannot be recovered"));
    }

    #[test]
    fn prompts_name_the_first_few_and_count_the_rest() {
        let items: Vec<PruneItem> = (1..=11).map(|n| PruneItem { name: format!("img{n}"), ..PruneItem::default() }).collect();
        let (_, detail) = prune_prompt(Prune::Images, &PrunePreview { items, size: 5 }).unwrap();
        assert!(detail.contains("• img8\n• and 3 more"));
        assert!(!detail.contains("img9"));
    }

    #[test]
    fn a_nearly_full_vm_disk_is_called_out() {
        assert_eq!(vm_disk_warning(None), None);
        assert_eq!(vm_disk_warning(Some((0, 0))), None);
        assert_eq!(vm_disk_warning(Some((89, 100))), None);
        assert_eq!(vm_disk_warning(Some((90, 100))).as_deref(), Some("VM disk 90% full"));
    }

    #[test]
    fn nothing_to_prune_asks_nothing() {
        assert_eq!(prune_prompt(Prune::Volumes, &PrunePreview::default()), None);
        assert_eq!(prune_prompt(Prune::BuildCache, &PrunePreview::default()), None);
        let cache = PrunePreview { items: Vec::new(), size: 2_000 };
        assert_eq!(prune_prompt(Prune::BuildCache, &cache).unwrap().0, "Clear the build cache?");
    }

    #[test]
    fn a_batch_reports_every_failure() {
        let failure = |name: &str| (name.to_string(), format!("{name} is in use"));
        assert_eq!(batch_error(3, &[]), None);
        assert_eq!(batch_error(1, &[failure("a")]), Some("a is in use".to_string()));
        assert_eq!(
            batch_error(5, &[failure("a"), failure("c")]),
            Some("2 of 5 could not be done:\n• a: a is in use\n• c: c is in use".to_string())
        );
        let many: Vec<_> = (0..9).map(|n| failure(&format!("v{n}"))).collect();
        assert!(batch_error(9, &many).unwrap().ends_with("• v5: v5 is in use\n• and 3 more"));
    }

    #[test]
    fn stats_read_clearly() {
        let reading = StatsReading { cpu: Some(3.25), memory: 120_000_000, limit: 2 << 30 };
        assert_eq!(stats_text("web", &reading), "web: CPU 3.2%, RAM 114 MiB of 2 GiB");
        let first = StatsReading { cpu: None, memory: 0, limit: 0 };
        assert_eq!(stats_text("web", &first), "web: CPU …, RAM 0 B");
        assert_eq!(vm_usage_text(1 << 30, 2 << 30), "1.0 GiB of 2 GiB (50%)");
    }

    #[test]
    fn exit_codes_decide_the_status_color() {
        let rows = container_rows(
            &[
                container("ok", "exited", "Exited (0) 1 minute ago"),
                container("bad", "exited", "Exited (137) 1 minute ago"),
                container("up", "running", "Up 1 minute"),
            ],
            100,
        );
        assert_eq!(rows[0].cells[0].tone, Tone::Gray);
        assert_eq!(rows[1].cells[0].tone, Tone::Red);
        assert_eq!(rows[2].cells[0].tone, Tone::Green);
    }

    #[test]
    fn images_get_one_row_per_tag_and_dangling_ones_use_the_id() {
        let images = [
            Image { id: "sha256:one".into(), repo_tags: vec!["a:1".into(), "a:2".into()], size: 10, ..Default::default() },
            Image { id: "sha256:two".into(), size: 20, ..Default::default() },
        ];
        let rows = image_rows(&images, &[container("x", "running", "Up")], 100);
        let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, ["a:1", "a:2", "sha256:two"]);
        assert_eq!(rows[0].cells[5].text, "1");
        assert_eq!(rows[2].cells[0].text, "<none>");
    }

    #[test]
    fn volumes_list_the_containers_using_them() {
        let mut user = container("db", "running", "Up");
        user.mounts = vec![Mount { kind: "volume".into(), name: "data".into(), ..Default::default() }];
        let sizes = HashMap::from([("data".to_string(), 1_500_000)]);
        let volumes = [Volume { name: "data".into(), ..Default::default() }];
        let rows = volume_rows(&volumes, &[user], &sizes, 100);
        assert_eq!(rows[0].cells[2].text, "1.5 MB");
        assert_eq!(rows[0].cells[3].text, "db");
    }

    #[test]
    fn summaries_handle_singular() {
        assert_eq!(containers_summary(&[container("a", "running", "Up")]), "1 container, 1 running");
        assert_eq!(volumes_summary(&[]), "0 volumes");
        assert_eq!(networks_summary(&[Network::default()]), "1 network");
    }

    fn network(name: &str) -> Network {
        Network { name: name.into(), id: format!("id-{name}"), ..Default::default() }
    }

    fn attach(c: &mut Container, n: &Network) {
        let endpoint = Endpoint { network_id: n.id.clone(), ..Default::default() };
        c.network_settings.networks.insert(n.name.clone(), endpoint);
    }

    #[test]
    fn networks_count_attached_containers_running_or_not() {
        let networks = [network("bridge"), network("shop_default"), network("spare")];
        let mut web = container("web", "running", "Up");
        let mut db = container("db", "exited", "Exited (0)");
        attach(&mut web, &networks[1]);
        attach(&mut db, &networks[1]);
        let rows = network_rows(&networks, &[web.clone(), db.clone()], 100);
        assert_eq!(rows[1].key, "id-shop_default");
        assert_eq!(rows[1].cells[3].text, "2");
        assert_eq!(rows[2].cells[3].text, "");

        let unused: Vec<&str> = unused_networks(&networks, &[web, db]).iter().map(|n| n.name.as_str()).collect();
        assert_eq!(unused, ["spare"]);
        let (title, detail) = network_prune_prompt(&[&networks[2]]);
        assert_eq!(title, "Remove 1 unused network?");
        assert!(detail.starts_with("• spare\n"));
    }

    #[test]
    fn ports_open_once_each_on_localhost() {
        let port = |ip: &str, private, public, kind: &str| Port { ip: ip.into(), private_port: private, public_port: public, kind: kind.into() };
        let ports = [
            port("0.0.0.0", 80, Some(18080), "tcp"),
            port("::", 80, Some(18080), "tcp"),
            port("127.0.0.1", 443, Some(8443), "tcp"),
            port("", 5432, None, "tcp"),
            port("0.0.0.0", 53, Some(5353), "udp"),
        ];
        assert_eq!(
            port_links(&ports),
            [
                ("127.0.0.1:8443 → 443".to_string(), "https://127.0.0.1:8443".to_string()),
                ("localhost:18080 → 80".to_string(), "http://localhost:18080".to_string()),
            ]
        );
    }

    #[test]
    fn only_bind_mounts_open_in_finder() {
        let mounts = [
            Mount { kind: "bind".into(), source: "/Users/a/site".into(), destination: "/www".into(), ..Default::default() },
            Mount { kind: "volume".into(), name: "data".into(), destination: "/data".into(), ..Default::default() },
        ];
        assert_eq!(bind_mounts(&mounts), [("/Users/a/site → /www".to_string(), "/Users/a/site".to_string())]);
    }

    #[test]
    fn details_read_the_inspect_json() {
        let json = r#"{"Id":"abc","Name":"/web","Created":"1970-01-01T01:00:00Z","Path":"nginx","Args":["-g","daemon off;"],
            "Image":"sha256:img","State":{"Status":"exited","ExitCode":137},
            "Config":{"Image":"nginx:alpine","WorkingDir":"","Labels":{"com.docker.compose.project":"demo"}},
            "HostConfig":{"RestartPolicy":{"Name":"no"}},
            "Mounts":[{"Type":"bind","Source":"/Users/a/site","Destination":"/www","RW":false}],
            "NetworkSettings":{"Ports":{"80/tcp":[{"HostIp":"0.0.0.0","HostPort":"18080"},{"HostIp":"::","HostPort":"18080"}],"443/tcp":null},
                               "Networks":{"bridge":{"IPAddress":"172.17.0.2"}}}}"#;
        let details: ContainerDetails = serde_json::from_str(json).unwrap();
        let lines = container_details(&details, &["nginx@sha256:d".to_string()], 3_600 + 7_200);
        let find = |label: &str| lines.iter().position(|(l, _)| *l == label).map(|i| lines[i].1.as_str());
        assert_eq!(find("Name"), Some("web"));
        assert_eq!(find("Digest"), Some("nginx@sha256:d"));
        assert_eq!(find("Status"), Some("exited (137)"));
        assert_eq!(find("Created"), Some("2 hours ago"));
        assert_eq!(find("Command"), Some("nginx -g daemon off;"));
        assert_eq!(find("Restart"), None);
        assert_eq!(find("Project"), Some("demo"));
        assert_eq!(find("Mounts"), Some("bind /Users/a/site → /www (read-only)"));
        assert_eq!(find("Networks"), Some("bridge 172.17.0.2"));
        let ports = lines.iter().position(|(l, _)| *l == "Ports").unwrap();
        assert_eq!(lines[ports].1, "443/tcp");
        assert_eq!(lines[ports + 1], ("", "18080 → 80/tcp".to_string()));
        assert!(details_text(&lines).starts_with("Name      web\nID        abc"));
    }
}
