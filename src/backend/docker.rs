//! Docker Engine API client.
//!
//! It is bound to one socket path, Colima's. It never reads `docker context` and
//! never falls back to another daemon, so Docker Desktop and OrbStack containers
//! on the same machine are out of reach by construction.

use super::http::{self, Cancel, encode};
use super::model::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub type DResult<T> = Result<T, String>;

const QUICK: Duration = Duration::from_secs(15);
/// Stopping waits for the container's own grace period, so allow well beyond it.
const SLOW: Duration = Duration::from_secs(120);

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

#[derive(Default)]
pub struct Snapshot {
    pub containers: Vec<Container>,
    pub images: Vec<Image>,
    pub volumes: Vec<Volume>,
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

    pub fn snapshot(&self) -> DResult<Snapshot> {
        Ok(Snapshot {
            containers: self.get_json("/containers/json?all=1")?,
            images: self.get_json("/images/json")?,
            volumes: self.get_json::<VolumeList>("/volumes")?.volumes,
        })
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

    /// Blocks until the event stream ends, calling `on_event` for each change
    /// that could alter what the app shows.
    pub fn watch_events(&self, cancel: &Cancel, mut on_event: impl FnMut()) -> DResult<()> {
        // Health checks fire exec_* events constantly; they change nothing here.
        let filters = serde_json::json!({
            "type": ["container", "image", "volume"],
            "event": ["create", "destroy", "start", "stop", "die", "pause", "unpause",
                      "rename", "pull", "delete", "tag", "untag", "load", "health_status"],
        });
        let path = format!("/events?filters={}", encode(&filters.to_string()));
        http::stream(&self.sock, "GET", &path, cancel, |_, _| on_event())
            .map(drop)
            .map_err(|e| e.to_string())
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

fn shell_quote(text: &str) -> String {
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
