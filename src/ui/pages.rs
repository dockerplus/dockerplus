//! Turns Docker objects into table rows. Pure data, so it needs no window.

use super::table::{Cell, Column, Row, Tone};
use crate::backend::model::*;
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Page {
    #[default]
    Colima,
    Containers,
    Images,
    Volumes,
}

impl Page {
    pub fn title(self) -> &'static str {
        match self {
            Self::Colima => "Colima",
            Self::Containers => "Containers",
            Self::Images => "Images",
            Self::Volumes => "Volumes",
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
    Column::new("Project", 100.0, 50.0),
    Column::new("Status", 150.0, 70.0),
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

pub fn container_rows(containers: &[Container], now: i64) -> Vec<Row> {
    containers
        .iter()
        .map(|c| Row {
            key: c.id.clone(),
            cells: vec![
                status_dot(c),
                Cell::text(c.name()),
                Cell::text(&c.image),
                Cell::dim(c.project()),
                Cell::text(&c.status),
                Cell::dim(&ports_text(&c.ports)),
                Cell::dim(&relative_time(now, c.created)).sorted_by(c.created),
            ],
        })
        .collect()
}

pub fn image_rows(images: &[Image], containers: &[Container], now: i64) -> Vec<Row> {
    let mut rows = Vec::new();
    for image in images {
        let users = containers.iter().filter(|c| c.image_id == image.id).count();
        let row = |key: String, repository: &str, tag: &str| Row {
            key,
            cells: vec![
                Cell::text(repository),
                Cell::text(tag),
                Cell::dim(short_id(&image.id)),
                Cell::text(&human_size(image.size)).sorted_by(image.size),
                Cell::dim(&relative_time(now, image.created)).sorted_by(image.created),
                Cell::dim(&if users == 0 { String::new() } else { users.to_string() })
                    .sorted_by(users as i64),
            ],
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
            Row {
                key: v.name.clone(),
                cells: vec![
                    Cell::text(&v.name),
                    Cell::dim(&v.driver),
                    Cell::text(&size.map_or("—".to_string(), human_size)).sorted_by(size.unwrap_or(-1)),
                    Cell::dim(&users.join(", ")),
                    Cell::dim(&relative_time(now, created)).sorted_by(created),
                ],
            }
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

pub fn plural(count: usize, noun: &str) -> String {
    if count == 1 { noun.to_string() } else { format!("{noun}s") }
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
        user.mounts = vec![Mount { kind: "volume".into(), name: "data".into() }];
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
    }
}
