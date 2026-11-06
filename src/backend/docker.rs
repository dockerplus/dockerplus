//! Docker Engine API client.
//!
//! It is bound to one socket path, Colima's. It never reads `docker context` and
//! never falls back to another daemon, so Docker Desktop and OrbStack containers
//! on the same machine are out of reach by construction.

use super::colima::{find_tool, search_path};
use super::http::{self, Cancel, encode};
use super::model::*;
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

pub type DResult<T> = Result<T, String>;

const QUICK: Duration = Duration::from_secs(15);
/// Stopping waits for the container's own grace period, so allow well beyond it.
const SLOW: Duration = Duration::from_secs(120);
/// The event stream is silent while nothing changes. After this long without a
/// word, the caller's check says whether the engine is still there.
const EVENTS_IDLE: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq)]
pub struct Docker {
    sock: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ContainerAction {
    Start,
    Stop,
    Restart,
    Pause,
    Unpause,
}

/// Requests made for the containers, images, volumes and networks lists, for measuring.
#[cfg(feature = "devtools")]
pub static FETCHES: [std::sync::atomic::AtomicUsize; 4] = [const { std::sync::atomic::AtomicUsize::new(0) }; 4];

/// Which lists a refresh should fetch.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Scope {
    pub containers: bool,
    pub images: bool,
    pub volumes: bool,
    pub networks: bool,
}

impl Scope {
    pub const ALL: Self = Self { containers: true, images: true, volumes: true, networks: true };
    pub const CONTAINERS: Self = Self { containers: true, images: false, volumes: false, networks: false };
    pub const IMAGES: Self = Self { containers: false, images: true, volumes: false, networks: false };
    pub const VOLUMES: Self = Self { containers: false, images: false, volumes: true, networks: false };
    pub const NETWORKS: Self = Self { containers: false, images: false, volumes: false, networks: true };

    pub fn merge(&mut self, other: Self) {
        self.containers |= other.containers;
        self.images |= other.images;
        self.volumes |= other.volumes;
        self.networks |= other.networks;
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The list an engine event touches. Image and volume "used by" columns come
    /// from the containers list, so container events need nothing else. Anything
    /// unreadable refreshes everything, since a stale list is worse than a fetch.
    /// Connecting a network changes a container, which is where the network
    /// page counts its containers.
    fn of_event(line: &str) -> Self {
        #[derive(serde::Deserialize)]
        struct Event {
            #[serde(rename = "Type")]
            kind: String,
            #[serde(rename = "Action", default)]
            action: String,
        }
        let event = serde_json::from_str::<Event>(line);
        match event.as_ref().map(|event| (event.kind.as_str(), event.action.as_str())) {
            Ok(("container", _)) => Self::CONTAINERS,
            Ok(("image", _)) => Self::IMAGES,
            Ok(("volume", _)) => Self::VOLUMES,
            Ok(("network", "connect" | "disconnect")) => Self::CONTAINERS,
            Ok(("network", _)) => Self::NETWORKS,
            _ => Self::ALL,
        }
    }
}

/// The lists a refresh fetched; the rest are left as they were.
#[derive(Default)]
pub struct Snapshot {
    pub containers: Option<Vec<Container>>,
    pub images: Option<Vec<Image>>,
    pub volumes: Option<Vec<Volume>>,
    pub networks: Option<Vec<Network>>,
    /// The lists that could not be fetched.
    pub failed: Scope,
}

impl Snapshot {
    fn keep<T>(&mut self, list: Scope, result: DResult<T>) -> Option<T> {
        if result.is_err() {
            self.failed.merge(list);
        }
        result.ok()
    }
}

impl Docker {
    pub fn new(sock: impl Into<PathBuf>) -> Self {
        Self { sock: sock.into() }
    }

    pub fn socket(&self) -> &Path {
        &self.sock
    }

    fn call(&self, method: &str, path: &str, timeout: Duration) -> DResult<Vec<u8>> {
        let res = http::request(&self.sock, method, path, None, timeout)
            .map_err(|e| format!("Cannot reach the Docker engine: {e}"))?;
        if res.status >= 400 {
            return Err(api_error(res.status, &res.body));
        }
        Ok(res.body)
    }

    fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> DResult<T> {
        let body = self.call("GET", path, QUICK)?;
        serde_json::from_slice(&body).map_err(|e| format!("Unexpected reply from {path}: {e}"))
    }

    pub fn ping(&self) -> bool {
        matches!(http::request(&self.sock, "GET", "/_ping", None, Duration::from_secs(2)), Ok(r) if r.status == 200)
    }

    pub fn version(&self) -> DResult<EngineVersion> {
        self.get_json("/version")
    }

    /// A list that fails costs only itself: the others still arrive.
    pub fn snapshot(&self, scope: Scope) -> Snapshot {
        #[cfg(feature = "devtools")]
        for (count, fetched) in FETCHES.iter().zip([scope.containers, scope.images, scope.volumes, scope.networks]) {
            count.fetch_add(fetched as usize, std::sync::atomic::Ordering::Relaxed);
        }
        let mut snapshot = Snapshot::default();
        if scope.containers {
            snapshot.containers = snapshot.keep(Scope::CONTAINERS, self.get_json("/containers/json?all=1"));
        }
        if scope.images {
            snapshot.images = snapshot.keep(Scope::IMAGES, self.get_json("/images/json"));
        }
        if scope.volumes {
            let volumes = self.get_json::<VolumeList>("/volumes").map(|list| list.volumes);
            snapshot.volumes = snapshot.keep(Scope::VOLUMES, volumes);
        }
        if scope.networks {
            snapshot.networks = snapshot.keep(Scope::NETWORKS, self.get_json("/networks"));
        }
        snapshot
    }

    /// What is using the disk, and how much of it could go. The engine walks the
    /// disk for this, so it is only asked while the Storage page is open.
    pub fn disk_usage(&self) -> DResult<Vec<StorageLine>> {
        Ok(self.get_json::<DiskUsage>("/system/df")?.lines())
    }

    /// Removes what a confirmation listed and nothing else, one by one, and
    /// returns what the engine refused and why. The engine's own prune would
    /// choose afresh, and take whatever stopped or came free while the dialog
    /// was open. This way, anything put back to use meanwhile is simply refused.
    pub fn remove_listed(&self, kind: Prune, items: &[PruneItem]) -> Vec<(String, String)> {
        let remove = |item: &PruneItem| item.targets.iter().find_map(|target| self.remove_target(kind, target).err());
        // An image can be refused until the ones built on it have gone, so
        // whatever failed gets one more try.
        let refused: Vec<&PruneItem> = items.iter().filter(|item| remove(item).is_some()).collect();
        refused.into_iter().filter_map(|item| remove(item).map(|error| (item.name.clone(), error))).collect()
    }

    fn remove_target(&self, kind: Prune, target: &str) -> DResult<()> {
        let collection = match kind {
            Prune::Containers => "containers",
            Prune::Images => "images",
            Prune::Volumes => "volumes",
            Prune::BuildCache => return Ok(()),
        };
        let path = format!("/{collection}/{}", encode(target));
        let res = http::request(&self.sock, "DELETE", &path, None, SLOW)
            .map_err(|e| format!("Cannot reach the Docker engine: {e}"))?;
        // Already gone is what was asked for.
        if res.status >= 400 && res.status != 404 {
            return Err(api_error(res.status, &res.body));
        }
        Ok(())
    }

    /// Empties the build cache and returns the bytes the engine says it freed.
    /// This is the one prune left to the engine: its entries have no names to
    /// list, and they can always be built again.
    pub fn clear_build_cache(&self) -> DResult<i64> {
        #[derive(serde::Deserialize, Default)]
        #[serde(rename_all = "PascalCase", default)]
        struct Report {
            space_reclaimed: i64,
        }
        let path = "/build/prune?all=1";
        let body = self.call("POST", path, SLOW)?;
        let report: Report = serde_json::from_slice(&body).map_err(|e| format!("Unexpected reply from {path}: {e}"))?;
        Ok(report.space_reclaimed)
    }

    /// What pruning `kind` would remove right now, for the confirmation.
    pub fn prune_preview(&self, kind: Prune) -> DResult<PrunePreview> {
        let only = match kind {
            Prune::Containers => "container",
            Prune::Images => "image",
            Prune::Volumes => "volume",
            Prune::BuildCache => "build-cache",
        };
        Ok(self.get_json::<DiskUsage>(&format!("/system/df?type={only}"))?.preview(kind))
    }

    /// One reading, returned at once. Without `one-shot` the engine spends two
    /// seconds taking its own second sample.
    pub fn stats(&self, id: &str) -> DResult<ContainerStats> {
        self.get_json(&format!("/containers/{}/stats?stream=0&one-shot=1", encode(id)))
    }

    /// Volume sizes need the engine to walk the disk, so they are fetched apart
    /// from the listing and only when the Volumes page is on screen.
    pub fn volume_sizes(&self) -> DResult<HashMap<String, i64>> {
        let usage: DiskUsage = self.get_json("/system/df?type=volume")?;
        Ok(usage
            .volumes
            .into_iter()
            .filter_map(|v| Some((v.name, v.usage_data?.size)))
            .collect())
    }

    pub fn container_action(&self, id: &str, action: ContainerAction) -> DResult<()> {
        let verb = match action {
            ContainerAction::Start => "start",
            ContainerAction::Stop => "stop",
            ContainerAction::Restart => "restart",
            ContainerAction::Pause => "pause",
            ContainerAction::Unpause => "unpause",
        };
        self.call("POST", &format!("/containers/{}/{verb}", encode(id)), SLOW).map(drop)
    }

    pub fn inspect(&self, id: &str) -> DResult<ContainerDetails> {
        self.get_json(&format!("/containers/{}/json", encode(id)))
    }

    pub fn remove_container(&self, id: &str, force: bool) -> DResult<()> {
        let path = format!("/containers/{}?force={}", encode(id), force as u8);
        self.call("DELETE", &path, SLOW).map(drop)
    }

    /// Removing `repo:tag` drops that tag; the image goes with its last tag.
    pub fn remove_image(&self, reference: &str) -> DResult<()> {
        self.call("DELETE", &format!("/images/{}", encode(reference)), SLOW).map(drop)
    }

    pub fn remove_volume(&self, name: &str) -> DResult<()> {
        self.call("DELETE", &format!("/volumes/{}", encode(name)), SLOW).map(drop)
    }

    /// The engine refuses while a running container is attached.
    pub fn remove_network(&self, id: &str) -> DResult<()> {
        self.call("DELETE", &format!("/networks/{}", encode(id)), SLOW).map(drop)
    }

    /// Streams a container's logs. Follows until `cancel` fires or it exits.
    pub fn logs(
        &self,
        id: &str,
        tail: u32,
        follow: bool,
        cancel: &Cancel,
        mut on_text: impl FnMut(&[u8]),
    ) -> DResult<()> {
        #[derive(serde::Deserialize, Default)]
        #[serde(rename_all = "PascalCase", default)]
        struct Inspect {
            config: InspectConfig,
        }
        #[derive(serde::Deserialize, Default)]
        #[serde(rename_all = "PascalCase", default)]
        struct InspectConfig {
            tty: bool,
        }

        let tty = self.get_json::<Inspect>(&format!("/containers/{}/json", encode(id)))?.config.tty;
        let path = format!(
            "/containers/{}/logs?stdout=1&stderr=1&tail={tail}&follow={}",
            encode(id),
            follow as u8
        );
        let mut demux = Demux::new(tty);
        let mut failure = Vec::new();
        let status = http::stream(&self.sock, "GET", &path, cancel, |status, chunk| {
            if status >= 400 {
                failure.extend_from_slice(chunk); // error bodies are not framed
            } else {
                demux.feed(chunk, &mut on_text);
            }
        })
        .map_err(|e| format!("Log stream failed: {e}"))?;
        if status >= 400 {
            return Err(api_error(status, &failure));
        }
        Ok(())
    }

    /// Pulls an image, reporting human-readable progress lines. Docker reports
    /// pull failures inside a 200 response, so the stream itself is checked too.
    pub fn pull(&self, reference: &str, mut on_progress: impl FnMut(String)) -> DResult<()> {
        let (repo, tag) = split_image_ref(reference);
        let digest = reference.contains('@');
        // An empty tag would pull every tag of the repository.
        let tag = if tag.is_empty() && !digest { "latest" } else { tag };
        let mut path = format!("/images/create?fromImage={}", encode(if digest { reference } else { repo }));
        if !digest {
            path.push_str(&format!("&tag={}", encode(tag)));
        }

        let cancel = Cancel::new();
        let mut lines = LineSplitter::default();
        let mut progress = PullProgress::default();
        let mut failure = Vec::new();
        let mut error = None;
        let status = http::stream(&self.sock, "POST", &path, &cancel, |status, chunk| {
            if status >= 400 {
                failure.extend_from_slice(chunk);
                return;
            }
            lines.feed(chunk, |line| match progress.update(line) {
                PullEvent::Message(text) => on_progress(text),
                PullEvent::Failed(text) => error = Some(text),
                PullEvent::Nothing => {}
            });
        })
        .map_err(|e| format!("Pull failed: {e}"))?;
        if status >= 400 {
            return Err(api_error(status, &failure));
        }
        error.map_or(Ok(()), Err)
    }

    /// Blocks until the event stream ends, reporting which list each change
    /// could alter. An error reply ends it with that error rather than being
    /// read as events. After a silent spell, `alive` decides whether to wait on.
    pub fn watch_events(
        &self,
        cancel: &Cancel,
        alive: impl FnMut() -> bool,
        mut on_event: impl FnMut(Scope),
    ) -> DResult<()> {
        // Health checks fire exec_* events constantly; they change nothing here.
        let filters = serde_json::json!({
            "type": ["container", "image", "volume", "network"],
            "event": ["create", "destroy", "start", "stop", "die", "pause", "unpause",
                      "rename", "pull", "delete", "tag", "untag", "load", "health_status",
                      "connect", "disconnect", "remove"],
        });
        let path = format!("/events?filters={}", encode(&filters.to_string()));
        let mut lines = LineSplitter::default();
        let mut failure = Vec::new();
        let status = http::stream_watched(&self.sock, "GET", &path, cancel, EVENTS_IDLE, alive, |status, chunk| {
            if status >= 400 {
                failure.extend_from_slice(chunk); // error bodies are not framed
            } else {
                lines.feed(chunk, |line| on_event(Scope::of_event(line)));
            }
        })
        .map_err(|e| e.to_string())?;
        if status >= 400 {
            return Err(api_error(status, &failure));
        }
        Ok(())
    }
}

fn api_error(status: u16, body: &[u8]) -> String {
    #[derive(serde::Deserialize)]
    struct ApiError {
        message: String,
    }
    match serde_json::from_slice::<ApiError>(body) {
        Ok(err) => err.message,
        Err(_) => format!("Docker returned HTTP {status}: {}", String::from_utf8_lossy(body).trim()),
    }
}

/// Runs `docker compose` on a project by name, so no compose file is needed. The
/// CLI does the work that makes a project more than its containers: `depends_on`
/// order and waiting for health checks. `DOCKER_HOST` binds it to Colima's socket
/// whatever the active context is.
pub fn compose(socket: &Path, project: &str, args: &[&str]) -> DResult<()> {
    let mut all: Vec<OsString> = vec!["-p".into(), project.into()];
    all.extend(args.iter().map(OsString::from));
    run_compose(socket, &all)
}

/// Starts the project in a folder, or in one Compose file, as `docker compose
/// up -d` would from a terminal there.
pub fn compose_up(socket: &Path, path: &Path) -> DResult<()> {
    run_compose(socket, &compose_up_args(path, path.is_dir())?)
}

/// The folder is the project directory, so `.env` and relative paths resolve
/// as they do in a terminal; a chosen file is its folder plus `-f`.
pub fn compose_up_args(path: &Path, is_dir: bool) -> DResult<Vec<OsString>> {
    let mut args: Vec<OsString> = vec!["--project-directory".into()];
    if is_dir {
        args.push(path.into());
    } else {
        let yaml = path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("yml") || ext.eq_ignore_ascii_case("yaml"));
        let Some(folder) = path.parent().filter(|_| yaml) else {
            return Err(format!("{} is not a Compose file. Choose a folder, or a .yml or .yaml file.", path.display()));
        };
        args.extend([folder.into(), "-f".into(), path.into()]);
    }
    args.extend(["up".into(), "-d".into()]);
    Ok(args)
}

/// Colima shares only the home folder with its VM; bind mounts from anywhere
/// else come up empty inside the containers.
pub fn shared_with_vm(path: &Path, home: &Path) -> bool {
    path.starts_with(home)
}

/// What the Run dialog asks for. Ports, env and volumes are lists separated by
/// spaces, so a value cannot hold one. That covers nearly all of them, and
/// anything else belongs on the command line.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunForm {
    pub image: String,
    pub name: String,
    pub ports: String,
    pub env: String,
    pub volumes: String,
}

/// The arguments for `docker`, or what is wrong with the form. A leading `-`
/// would be read as an option, and `KEY` alone would pass on the app's own value.
pub fn run_args(form: &RunForm, home: Option<&Path>) -> DResult<Vec<String>> {
    let (image, name) = (form.image.trim(), form.name.trim());
    if image.is_empty() || image.starts_with('-') {
        return Err("Enter an image to run, such as nginx:alpine.".to_string());
    }
    if name.starts_with('-') {
        return Err(format!("{name} is not a container name."));
    }
    let mut args: Vec<String> = ["run", "-d"].map(String::from).into();
    if !name.is_empty() {
        args.extend(["--name".to_string(), name.to_string()]);
    }
    for port in form.ports.split_whitespace() {
        args.extend(["-p".to_string(), port.to_string()]);
    }
    for variable in form.env.split_whitespace() {
        if !variable.contains('=') {
            return Err(format!("{variable} is not KEY=value."));
        }
        args.extend(["-e".to_string(), variable.to_string()]);
    }
    for volume in form.volumes.split_whitespace() {
        args.extend(["-v".to_string(), expand_home(volume, home)]);
    }
    args.push(image.to_string());
    Ok(args)
}

fn expand_home(volume: &str, home: Option<&Path>) -> String {
    match (volume.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => home.join(rest).display().to_string(),
        _ => volume.to_string(),
    }
}

/// The host folders in a form that the VM cannot see.
pub fn unshared_mounts(form: &RunForm, home: &Path) -> Vec<String> {
    form.volumes
        .split_whitespace()
        .map(|volume| expand_home(volume, Some(home)))
        .filter_map(|volume| volume.split(':').next().map(String::from))
        .filter(|source| source.starts_with('/') && !shared_with_vm(Path::new(source), home))
        .collect()
}

pub fn run_image(socket: &Path, args: &[String]) -> DResult<()> {
    let Some(docker) = find_tool("docker") else {
        return Err("Running an image needs the Docker command line. Install it with: brew install docker".to_string());
    };
    run_cli(Command::new(docker), socket, args, "docker run failed")
}

fn run_compose(socket: &Path, args: &[OsString]) -> DResult<()> {
    let Some(command) = compose_command(find_tool("docker"), find_tool("docker-compose")) else {
        return Err("Project actions need Docker Compose. Install it with: brew install docker docker-compose".to_string());
    };
    run_cli(command, socket, args, "docker compose failed")
}

/// Runs a Docker tool against this engine. Its last line of errors is the reason.
fn run_cli(mut command: Command, socket: &Path, args: &[impl AsRef<std::ffi::OsStr>], fallback: &str) -> DResult<()> {
    let output = command
        .args(args)
        .env("DOCKER_HOST", format!("unix://{}", socket.display()))
        .env("PATH", search_path())
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("Could not run {}: {e}", command.get_program().to_string_lossy()))?;
    if output.status.success() {
        return Ok(());
    }
    Err(failure_reason(&String::from_utf8_lossy(&output.stderr)).unwrap_or(fallback).to_string())
}

/// The last line that says something. `docker run` ends on a pointer to its help.
fn failure_reason(text: &str) -> Option<&str> {
    let line = text.lines().map(str::trim).rfind(|line| !line.is_empty() && !line.starts_with("Run 'docker"))?;
    Some(line.strip_prefix("docker: ").unwrap_or(line))
}

/// The `docker compose` plugin, or else the plain `docker-compose` binary: Docker
/// only finds Homebrew's plugin once its config says where to look.
fn compose_command(docker: Option<PathBuf>, standalone: Option<PathBuf>) -> Option<Command> {
    let has_plugin = |docker: &PathBuf| {
        let check = Command::new(docker).args(["compose", "version"]).env("PATH", search_path()).output();
        check.is_ok_and(|output| output.status.success())
    };
    match docker.filter(has_plugin) {
        Some(docker) => {
            let mut command = Command::new(docker);
            command.arg("compose");
            Some(command)
        }
        None => standalone.map(Command::new),
    }
}

/// A script for Terminal that opens a shell in a running container. It names
/// the socket itself, so the CLI cannot reach another daemon through a context.
pub fn exec_script(cli: &Path, socket: &Path, id: &str) -> String {
    let host = format!("unix://{}", socket.display());
    let shell = "command -v bash >/dev/null 2>&1 && exec bash || exec sh";
    format!(
        "#!/bin/sh\nrm -f \"$0\"\nexport DOCKER_HOST={}\nexec {} exec -it {} sh -c {}\n",
        shell_quote(&host),
        shell_quote(&cli.to_string_lossy()),
        shell_quote(id),
        shell_quote(shell),
    )
}

pub(crate) fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// Splits Docker's multiplexed log stream. Without a TTY every frame carries an
/// 8-byte header: stream type, three zero bytes, big-endian payload length.
pub struct Demux {
    raw: bool,
    header: [u8; 8],
    have: usize,
    remaining: usize,
}

impl Demux {
    pub fn new(tty: bool) -> Self {
        Self { raw: tty, header: [0; 8], have: 0, remaining: 0 }
    }

    pub fn feed(&mut self, mut input: &[u8], out: &mut impl FnMut(&[u8])) {
        if self.raw {
            out(input);
            return;
        }
        while !input.is_empty() {
            if self.remaining > 0 {
                let take = self.remaining.min(input.len());
                out(&input[..take]);
                input = &input[take..];
                self.remaining -= take;
                continue;
            }
            let take = (8 - self.have).min(input.len());
            self.header[self.have..self.have + take].copy_from_slice(&input[..take]);
            self.have += take;
            input = &input[take..];
            if self.have == 8 {
                let h = self.header;
                self.remaining = u32::from_be_bytes([h[4], h[5], h[6], h[7]]) as usize;
                self.have = 0;
            }
        }
    }
}

#[derive(Default)]
struct LineSplitter {
    partial: Vec<u8>,
}

impl LineSplitter {
    fn feed(&mut self, chunk: &[u8], mut on_line: impl FnMut(&str)) {
        self.partial.extend_from_slice(chunk);
        while let Some(end) = self.partial.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=end).collect();
            on_line(String::from_utf8_lossy(&line).trim());
        }
    }
}

#[derive(PartialEq, Debug)]
enum PullEvent {
    Message(String),
    Failed(String),
    Nothing,
}

/// Turns per-layer pull JSON into one overall percentage.
#[derive(Default)]
struct PullProgress {
    layers: HashMap<String, (u64, u64)>,
    last_percent: Option<u64>,
}

impl PullProgress {
    fn update(&mut self, line: &str) -> PullEvent {
        #[derive(serde::Deserialize, Default)]
        #[serde(default, rename_all = "camelCase")]
        struct Line {
            status: String,
            id: String,
            error: Option<String>,
            progress_detail: Detail,
        }
        #[derive(serde::Deserialize, Default)]
        #[serde(default)]
        struct Detail {
            current: u64,
            total: u64,
        }
        let Ok(parsed) = serde_json::from_str::<Line>(line) else { return PullEvent::Nothing };
        if let Some(error) = parsed.error {
            return PullEvent::Failed(error);
        }
        if parsed.status == "Downloading" && parsed.progress_detail.total > 0 {
            let detail = parsed.progress_detail;
            self.layers.insert(parsed.id, (detail.current, detail.total));
            let (done, total) = self.layers.values().fold((0, 0), |(d, t), (c, s)| (d + c, t + s));
            let percent = done * 100 / total;
            if self.last_percent == Some(percent) {
                return PullEvent::Nothing;
            }
            self.last_percent = Some(percent);
            return PullEvent::Message(format!("Downloading {percent}%"));
        }
        if parsed.status.starts_with("Status:") || parsed.status.starts_with("Digest:") {
            return PullEvent::Message(parsed.status);
        }
        PullEvent::Nothing
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dp-{name}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn script(path: &Path, exit: u8) -> PathBuf {
        fs::write(path, format!("#!/bin/sh\nexit {exit}\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        path.to_path_buf()
    }

    /// An engine that answers each request with what `reply` returns for its path,
    /// as status line and body, then closes the connection.
    fn fake_engine(name: &str, reply: fn(&str) -> (&'static str, &'static str)) -> (PathBuf, PathBuf) {
        let dir = scratch_dir(name);
        let sock = dir.join("d.sock");
        let _ = fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut request = String::new();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                reader.read_line(&mut request).unwrap();
                while reader.read_line(&mut String::new()).is_ok_and(|n| n > 2) {}
                let (status, body) = reply(request.split(' ').nth(1).unwrap_or(""));
                let head = format!("HTTP/1.1 {status}\r\nConnection: close\r\n\r\n");
                let _ = stream.write_all(head.as_bytes()).and_then(|()| stream.write_all(body.as_bytes()));
            }
        });
        (dir, sock)
    }

    #[test]
    fn an_event_stream_error_is_an_error_not_events() {
        let (dir, sock) = fake_engine("events-error", |_| ("500 Oops", r#"{"message":"events are off"}"#));
        let mut events = 0;
        let result = Docker::new(&sock).watch_events(&Cancel::new(), || true, |_| events += 1);
        assert_eq!(result, Err("events are off".to_string()));
        assert_eq!(events, 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn events_report_the_lists_they_touch() {
        let (dir, sock) = fake_engine("events", |_| {
            ("200 OK", "{\"Type\":\"container\",\"Action\":\"start\"}\n{\"Type\":\"image\",\"Action\":\"pull\"}\n")
        });
        let mut scopes = Vec::new();
        let result = Docker::new(&sock).watch_events(&Cancel::new(), || true, |scope| scopes.push(scope));
        assert_eq!(result, Ok(()));
        assert_eq!(scopes, [Scope::CONTAINERS, Scope::IMAGES]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_prune_preview_asks_only_for_its_kind() {
        let (dir, sock) = fake_engine("preview", |path| match path {
            "/system/df?type=volume" => {
                ("200 OK", r#"{"Volumes":[{"Name":"data","UsageData":{"RefCount":0,"Size":500}}],"VolumeUsage":{}}"#)
            }
            _ => ("404 Not Found", r#"{"message":"wrong path"}"#),
        });
        let preview = Docker::new(&sock).prune_preview(Prune::Volumes).unwrap();
        assert_eq!(preview.items[0].name, "data");
        assert_eq!(preview.items[0].size, Some(500));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_run_form_becomes_docker_run_arguments() {
        let form = RunForm {
            image: " nginx:alpine ".into(),
            name: "web".into(),
            ports: "8080:80  8443:443".into(),
            env: "MODE=dev DSN=a=b".into(),
            volumes: "data:/data ~/site:/srv".into(),
        };
        let args = run_args(&form, Some(Path::new("/Users/a"))).unwrap();
        assert_eq!(args.join(" "), "run -d --name web -p 8080:80 -p 8443:443 -e MODE=dev -e DSN=a=b \
                                    -v data:/data -v /Users/a/site:/srv nginx:alpine");
        let bare = RunForm { image: "alpine".into(), ..Default::default() };
        assert_eq!(run_args(&bare, None).unwrap(), ["run", "-d", "alpine"]);
    }

    #[test]
    fn a_run_form_that_cannot_work_says_why() {
        let form = |image: &str, name: &str, env: &str| RunForm {
            image: image.into(),
            name: name.into(),
            env: env.into(),
            ..Default::default()
        };
        assert!(run_args(&form("  ", "", ""), None).unwrap_err().contains("Enter an image"));
        assert!(run_args(&form("--privileged", "", ""), None).is_err());
        assert!(run_args(&form("alpine", "-x", ""), None).is_err());
        assert_eq!(run_args(&form("alpine", "", "SECRET"), None).unwrap_err(), "SECRET is not KEY=value.");
    }

    #[test]
    fn a_failure_gives_the_reason_not_the_help_pointer() {
        let text = "docker: Error response from daemon: name is in use\n\nRun 'docker run --help' for more information\n";
        assert_eq!(failure_reason(text), Some("Error response from daemon: name is in use"));
        assert_eq!(failure_reason("no such service: web\n"), Some("no such service: web"));
        assert_eq!(failure_reason("\n"), None);
    }

    #[test]
    fn only_host_folders_outside_home_are_flagged() {
        let form = RunForm { volumes: "data:/d ~/ok:/o /Users/a/fine:/f /opt/x:/x /tmp/y:/y:ro".into(), ..Default::default() };
        assert_eq!(unshared_mounts(&form, Path::new("/Users/a")), ["/opt/x", "/tmp/y"]);
    }

    #[test]
    fn only_what_was_listed_is_removed() {
        static ASKED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let (dir, sock) = fake_engine("listed", |path| {
            ASKED.lock().unwrap().push(path.to_string());
            match path {
                "/volumes/gone" => ("404 Not Found", r#"{"message":"no such volume"}"#),
                "/volumes/busy" => ("409 Conflict", r#"{"message":"volume is in use"}"#),
                _ => ("204 No Content", ""),
            }
        });
        let item = |name: &str| PruneItem { name: name.into(), size: None, targets: vec![name.into()] };
        let items = [item("data"), item("gone"), item("busy")];

        let refused = Docker::new(&sock).remove_listed(Prune::Volumes, &items);

        assert_eq!(refused, [("busy".to_string(), "volume is in use".to_string())]);
        let asked = ASKED.lock().unwrap();
        assert_eq!(*asked, ["/volumes/data", "/volumes/gone", "/volumes/busy", "/volumes/busy"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_image_can_wait_for_the_ones_built_on_it() {
        static PARENT_TRIES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let (dir, sock) = fake_engine("listed-images", |path| match path {
            "/images/parent" if PARENT_TRIES.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 => {
                ("409 Conflict", r#"{"message":"image has dependent child images"}"#)
            }
            _ => ("200 OK", "[]"),
        });
        let item = |name: &str| PruneItem { name: name.into(), size: None, targets: vec![name.into()] };

        let refused = Docker::new(&sock).remove_listed(Prune::Images, &[item("parent"), item("child")]);

        assert!(refused.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_list_does_not_cost_the_others() {
        let dir = scratch_dir("snapshot");
        let sock = dir.join("d.sock");
        let _ = fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut request = String::new();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                reader.read_line(&mut request).unwrap();
                while reader.read_line(&mut String::new()).is_ok_and(|n| n > 2) {}
                let (status, body) = match request.split(' ').nth(1).unwrap_or("") {
                    "/images/json" => ("500 Oops", r#"{"message":"store is busy"}"#),
                    "/volumes" => ("200 OK", r#"{"Volumes":[]}"#),
                    _ => ("200 OK", "[]"),
                };
                let head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = stream.write_all(head.as_bytes()).and_then(|()| stream.write_all(body.as_bytes()));
            }
        });

        let snapshot = Docker::new(&sock).snapshot(Scope::ALL);
        assert!(snapshot.containers.is_some());
        assert!(snapshot.images.is_none());
        assert!(snapshot.volumes.is_some());
        assert_eq!(snapshot.failed, Scope::IMAGES);
        assert!(Docker::new(&sock).snapshot(Scope::CONTAINERS).failed.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn compose_falls_back_to_the_plain_binary() {
        let dir = scratch_dir("compose");
        let working = script(&dir.join("docker-with-plugin"), 0);
        let without = script(&dir.join("docker-without-plugin"), 1);
        let standalone = dir.join("docker-compose");

        let plugin = compose_command(Some(working.clone()), Some(standalone.clone())).unwrap();
        assert_eq!(plugin.get_program(), working);
        assert_eq!(plugin.get_args().collect::<Vec<_>>(), ["compose"]);

        let plain = compose_command(Some(without.clone()), Some(standalone.clone())).unwrap();
        assert_eq!(plain.get_program(), standalone);
        assert_eq!(plain.get_args().count(), 0);

        assert_eq!(compose_command(None, Some(standalone.clone())).unwrap().get_program(), standalone);
        assert!(compose_command(Some(without), None).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    fn frame(stream: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![stream, 0, 0, 0];
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn demux_handles_frames_split_anywhere() {
        let mut wire = frame(1, b"out line\n");
        wire.extend(frame(2, b"err line\n"));
        for piece in 1..wire.len() {
            let mut demux = Demux::new(false);
            let mut out = Vec::new();
            for part in wire.chunks(piece) {
                demux.feed(part, &mut |c| out.extend_from_slice(c));
            }
            assert_eq!(out, b"out line\nerr line\n", "piece size {piece}");
        }
    }

    #[test]
    fn events_refresh_only_their_own_list() {
        assert_eq!(Scope::of_event(r#"{"Type":"container","Action":"start"}"#), Scope::CONTAINERS);
        assert_eq!(Scope::of_event(r#"{"Type":"image","Action":"pull"}"#), Scope::IMAGES);
        assert_eq!(Scope::of_event(r#"{"Type":"volume","Action":"create"}"#), Scope::VOLUMES);
        assert_eq!(Scope::of_event(r#"{"Type":"network","Action":"destroy"}"#), Scope::NETWORKS);
        assert_eq!(Scope::of_event(r#"{"Type":"network","Action":"connect"}"#), Scope::CONTAINERS);
    }

    #[test]
    fn anything_unreadable_refreshes_everything() {
        assert_eq!(Scope::of_event(r#"{"Type":"daemon"}"#), Scope::ALL);
        assert_eq!(Scope::of_event("not json"), Scope::ALL);
    }

    #[test]
    fn scopes_add_up() {
        let mut scope = Scope::default();
        assert!(scope.is_empty());
        scope.merge(Scope::IMAGES);
        scope.merge(Scope::VOLUMES);
        assert_eq!(scope, Scope { containers: false, images: true, volumes: true, networks: false });
    }

    #[test]
    fn compose_up_runs_in_the_chosen_folder() {
        let args = |path: &str, is_dir| compose_up_args(Path::new(path), is_dir).map(|a| a.join(std::ffi::OsStr::new(" ")));
        assert_eq!(args("/Users/a/shop", true).unwrap(), "--project-directory /Users/a/shop up -d");
        assert_eq!(
            args("/Users/a/shop/compose.prod.YAML", false).unwrap(),
            "--project-directory /Users/a/shop -f /Users/a/shop/compose.prod.YAML up -d"
        );
        assert!(args("/Users/a/shop/Dockerfile", false).unwrap_err().contains("not a Compose file"));
    }

    #[test]
    fn only_the_home_folder_reaches_the_vm() {
        let home = Path::new("/Users/a");
        assert!(shared_with_vm(Path::new("/Users/a/shop"), home));
        assert!(!shared_with_vm(Path::new("/Users/ab/shop"), home));
        assert!(!shared_with_vm(Path::new("/Volumes/Work/shop"), home));
    }

    #[test]
    fn tty_logs_pass_through() {
        let mut demux = Demux::new(true);
        let mut out = Vec::new();
        demux.feed(b"\x01raw", &mut |c| out.extend_from_slice(c));
        assert_eq!(out, b"\x01raw");
    }

    #[test]
    fn line_splitter_joins_across_chunks() {
        let mut splitter = LineSplitter::default();
        let mut lines = Vec::new();
        splitter.feed(b"{\"a\":", |l| lines.push(l.to_string()));
        splitter.feed(b"1}\n{\"b\":2}\n", |l| lines.push(l.to_string()));
        assert_eq!(lines, ["{\"a\":1}", "{\"b\":2}"]);
    }

    #[test]
    fn pull_reports_percent_and_errors() {
        let mut p = PullProgress::default();
        let line = r#"{"status":"Downloading","id":"a","progressDetail":{"current":50,"total":100}}"#;
        assert_eq!(p.update(line), PullEvent::Message("Downloading 50%".into()));
        assert_eq!(p.update(line), PullEvent::Nothing);
        assert_eq!(p.update(r#"{"status":"Waiting","id":"b"}"#), PullEvent::Nothing);
        assert_eq!(p.update(r#"{"error":"manifest unknown"}"#), PullEvent::Failed("manifest unknown".into()));
    }

    #[test]
    fn api_errors_prefer_the_engine_message() {
        assert_eq!(api_error(409, br#"{"message":"volume is in use"}"#), "volume is in use");
        assert_eq!(api_error(500, b"boom"), "Docker returned HTTP 500: boom");
    }

    #[test]
    fn terminal_script_is_bound_to_the_colima_socket() {
        let script = exec_script(Path::new("/usr/local/bin/docker"), Path::new("/Users/a b/.colima/docker.sock"), "abc");
        assert!(script.contains("export DOCKER_HOST='unix:///Users/a b/.colima/docker.sock'\n"));
        assert!(script.contains("exec '/usr/local/bin/docker' exec -it 'abc' sh -c '"));
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }
}
