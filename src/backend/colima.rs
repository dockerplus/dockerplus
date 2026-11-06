//! Wraps the `colima` command line.
//!
//! Two things here exist because of how Colima behaves, not by taste:
//!
//! * `colima start` switches the machine's active `docker context` unless told
//!   not to. Other tools rely on the current context, so every start passes
//!   `--activate=false`.
//! * An app launched from Finder inherits launchd's bare PATH, where neither
//!   `colima` nor the `limactl` and `docker` it shells out to can be found.
//! * Lima names the VM's user after the macOS account. When that name is already
//!   a system account in Ubuntu (`man`, say), SSH lands in its `nologin` shell
//!   and the first start hangs for ten minutes before failing with no clue why.

use super::model::{ColimaProfile, ColimaStatus};
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

const EXTRA_PATHS: [&str; 3] = ["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"];

/// Accounts that exist in the Ubuntu image before Lima adds its own user.
const RESERVED_GUEST_USERS: [&str; 18] = [
    "root", "daemon", "bin", "sys", "sync", "games", "man", "lp", "mail", "news", "uucp", "proxy",
    "www-data", "backup", "list", "irc", "gnats", "nobody",
];
const GUEST_USER_OVERRIDE: &str = "# Written by DockerPlus: the macOS account name is a system account in Ubuntu.\nuser:\n  name: colima\n";

#[derive(Clone, Debug)]
pub struct Colima {
    bin: PathBuf,
    profile: String,
}

/// What we know about the Colima instance.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ColimaInfo {
    pub installed: bool,
    pub version: String,
    pub running: bool,
    /// False until `colima start` has created a VM.
    pub provisioned: bool,
    pub status: Option<ColimaStatus>,
    pub profile: Option<ColimaProfile>,
}

/// VM size in whole CPUs and GiB, as `colima start` takes it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Resources {
    pub cpus: u32,
    pub memory: u32,
    pub disk: u32,
}

/// Offered disk sizes. Colima can grow a disk but never shrink it.
const DISK_STEPS: [u32; 12] = [20, 40, 60, 80, 100, 150, 200, 300, 400, 500, 750, 1000];
/// RAM kept back for macOS. A VM that fills the rest of the machine stalls it.
const HOST_RESERVE_GIB: u32 = 4;

/// What a start should change. `None` leaves Colima's saved value alone, so a
/// value that was only displayed, and rounded on the way, is never written back.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ResourceChange {
    pub cpus: Option<u32>,
    pub memory: Option<u32>,
    pub disk: Option<u32>,
}

impl ResourceChange {
    /// The difference from `current`. With no VM yet everything is new. The
    /// disk only ever grows, whatever was asked for.
    pub fn between(current: Option<Resources>, wanted: Resources) -> Self {
        let Some(current) = current else {
            return Self { cpus: Some(wanted.cpus), memory: Some(wanted.memory), disk: Some(wanted.disk) };
        };
        Self {
            cpus: (wanted.cpus != current.cpus).then_some(wanted.cpus),
            memory: (wanted.memory != current.memory).then_some(wanted.memory),
            disk: (wanted.disk > current.disk).then_some(wanted.disk),
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

impl Resources {
    /// Colima's own defaults, used before a VM exists.
    pub const DEFAULT: Self = Self { cpus: 2, memory: 2, disk: 100 };

    pub fn current(info: &ColimaInfo) -> Option<Self> {
        let (cpus, memory, disk) = match (&info.status, &info.profile) {
            (Some(s), _) => (s.cpu, s.memory, s.disk),
            (None, Some(p)) => (p.cpus, p.memory, p.disk),
            _ => return None,
        };
        let gib = |bytes: i64| (bytes as f64 / (1u64 << 30) as f64).round() as u32;
        (cpus > 0).then(|| Self { cpus: cpus as u32, memory: gib(memory), disk: gib(disk) })
    }

    /// Memory sizes to offer, keeping the current one even if it is over the limit.
    pub fn memory_choices(host_gib: u32, current: u32) -> Vec<u32> {
        (1..=host_gib.saturating_sub(HOST_RESERVE_GIB).max(current).max(1)).collect()
    }

    pub fn disk_choices(current: Option<u32>) -> Vec<u32> {
        let floor = current.unwrap_or(0);
        let mut choices: Vec<u32> = DISK_STEPS.into_iter().filter(|d| *d >= floor).chain(current).collect();
        choices.sort_unstable();
        choices.dedup();
        choices
    }
}

impl Colima {
    pub fn locate(profile: &str) -> Option<Self> {
        find_tool("colima").map(|bin| Self { bin, profile: profile.to_string() })
    }

    pub fn default_socket(profile: &str) -> PathBuf {
        colima_home().join(profile).join("docker.sock")
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args).env("PATH", search_path()).stdin(Stdio::null());
        cmd
    }

    pub fn info(&self) -> ColimaInfo {
        let version = self
            .command(&["version"])
            .output()
            .ok()
            .and_then(|out| parse_version(&String::from_utf8_lossy(&out.stdout)))
            .unwrap_or_default();

        let status = self
            .command(&["status", "--json", "--profile", &self.profile])
            .output()
            .ok()
            .filter(|out| out.status.success())
            .and_then(|out| serde_json::from_slice::<ColimaStatus>(&out.stdout).ok());

        let profile = self
            .command(&["list", "--json"])
            .output()
            .ok()
            .and_then(|out| parse_profile(&String::from_utf8_lossy(&out.stdout), &self.profile));

        ColimaInfo {
            installed: true,
            version,
            running: is_running(status.as_ref(), profile.as_ref()),
            provisioned: profile.is_some() || status.is_some(),
            status,
            profile,
        }
    }

    /// Starts the VM with `change` applied. Colima saves the new size in its
    /// config, so later plain starts keep it.
    pub fn start(&self, change: ResourceChange, mut on_line: impl FnMut(String)) -> Result<(), String> {
        if let Some(note) = prepare_guest_user()? {
            on_line(note);
        }
        let args = start_args(&self.profile, change);
        self.run(&args.iter().map(String::as_str).collect::<Vec<_>>(), on_line)
    }

    /// Memory in use inside the VM and its total, in bytes.
    pub fn vm_memory(&self) -> Option<(u64, u64)> {
        let output = self.command(&["ssh", "--profile", &self.profile, "--", "free", "-b"]).output().ok()?;
        parse_free(&String::from_utf8_lossy(&output.stdout))
    }

    pub fn stop(&self, on_line: impl FnMut(String)) -> Result<(), String> {
        self.run(&["stop", "--profile", self.profile.as_str()], on_line)
    }

    /// Runs colima to completion, forwarding its output line by line. Colima
    /// logs to stderr and tools it runs may use stdout, so both are merged.
    fn run(&self, args: &[&str], mut on_line: impl FnMut(String)) -> Result<(), String> {
        let mut child = self
            .command(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Could not run {}: {e}", self.bin.display()))?;

        let (tx, rx) = mpsc::channel();
        forward_lines(child.stdout.take().unwrap(), tx.clone());
        forward_lines(child.stderr.take().unwrap(), tx);

        let mut last_error = None;
        for line in rx {
            let cleaned = clean_log_line(&line);
            if is_failure_line(&line) {
                last_error = Some(cleaned.clone());
            }
            on_line(cleaned);
        }
        let status = child.wait().map_err(|e| e.to_string())?;
        if status.success() {
            Ok(())
        } else {
            Err(last_error.unwrap_or_else(|| format!("colima exited with {status}")))
        }
    }
}

fn start_args(profile: &str, change: ResourceChange) -> Vec<String> {
    let mut args: Vec<String> = ["start", "--activate=false", "--profile", profile].map(String::from).into();
    for (flag, value) in [("--cpus", change.cpus), ("--memory", change.memory), ("--disk", change.disk)] {
        if let Some(value) = value {
            args.push(flag.to_string());
            args.push(value.to_string());
        }
    }
    args
}

/// Bytes the VM's data disk really takes on the Mac. The file is sparse, so its
/// length says nothing; the blocks it occupies do.
pub fn disk_on_mac() -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    let disk = colima_home().join("_lima/_disks/colima/datadisk");
    fs::metadata(disk).ok().map(|meta| meta.blocks() * 512)
}

/// Reads `free -b`: "Mem: total used free shared buff/cache available". Used here
/// means what is not available, since Linux fills idle memory with cache.
fn parse_free(text: &str) -> Option<(u64, u64)> {
    let line = text.lines().find(|line| line.starts_with("Mem:"))?;
    let fields: Vec<u64> = line.split_whitespace().skip(1).filter_map(|field| field.parse().ok()).collect();
    let (total, available) = (*fields.first()?, *fields.get(5)?);
    Some((total.saturating_sub(available), total))
}

fn colima_home() -> PathBuf {
    env::var_os("COLIMA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".colima")))
        .unwrap_or_default()
}

/// A half-started VM has no status but is still listed as running, and it needs
/// Stop to stay available.
fn is_running(status: Option<&ColimaStatus>, profile: Option<&ColimaProfile>) -> bool {
    status.is_some() || profile.is_some_and(|p| p.status == "Running")
}

fn needs_guest_user_override(host_user: &str) -> bool {
    RESERVED_GUEST_USERS.contains(&host_user)
}

/// Gives the VM a usable account when the macOS name cannot be one. Leaves any
/// existing override alone, since that file is the user's to own.
fn prepare_guest_user() -> Result<Option<String>, String> {
    let user = env::var("USER").unwrap_or_default();
    if !needs_guest_user_override(&user) {
        return Ok(None);
    }
    let path = colima_home().join("_lima/_config/override.yaml");
    if path.exists() {
        return Ok(None);
    }
    let write = |path: &Path| -> std::io::Result<()> {
        fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
        fs::write(path, GUEST_USER_OVERRIDE)
    };
    write(&path).map_err(|e| format!("Could not write {}: {e}", path.display()))?;
    Ok(Some(format!(
        "“{user}” is a system account inside Ubuntu, so the VM's user will be “colima” ({}).",
        path.display()
    )))
}

fn forward_lines(source: impl Read + Send + 'static, tx: mpsc::Sender<String>) {
    thread::spawn(move || {
        for line in BufReader::new(source).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
}

/// The directories `colima` and its helpers are looked up in.
pub fn search_path() -> OsString {
    let mut dirs: Vec<PathBuf> = EXTRA_PATHS.iter().map(PathBuf::from).collect();
    if let Some(home) = env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".local/bin"));
    }
    if let Some(current) = env::var_os("PATH") {
        for dir in env::split_paths(&current) {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
    }
    env::join_paths(dirs).unwrap_or_default()
}

/// Finds a command line tool where a login shell would, even when the app was
/// opened from Finder.
pub fn find_tool(name: &str) -> Option<PathBuf> {
    env::split_paths(&search_path()).map(|dir| dir.join(name)).find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

fn parse_version(text: &str) -> Option<String> {
    let line = text.lines().find(|l| l.starts_with("colima version"))?;
    line.rsplit(' ').next().map(str::to_string)
}

fn parse_profile(text: &str, name: &str) -> Option<ColimaProfile> {
    text.lines()
        .filter_map(|line| serde_json::from_str::<ColimaProfile>(line).ok())
        .find(|p| p.name == name)
}

fn is_failure_line(line: &str) -> bool {
    line.contains("level=fatal") || line.contains("level=error")
}

/// Reduces logfmt output (`time=… level=info msg="…"`) to a readable line. The
/// value is only quoted when it needs to be: a bare `msg=done` is valid too.
fn clean_log_line(line: &str) -> String {
    let Some(start) = line.find("msg=") else { return line.to_string() };
    let rest = &line[start + 4..];
    let message = match rest.strip_prefix('"') {
        Some(quoted) => {
            let mut message = String::new();
            let mut chars = quoted.chars();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => message.extend(chars.next()),
                    '"' => break,
                    _ => message.push(c),
                }
            }
            message
        }
        None => rest.split_whitespace().next().unwrap_or("").to_string(),
    };
    let level = line
        .split_whitespace()
        .find_map(|field| field.strip_prefix("level="))
        .unwrap_or("info");
    match level {
        "info" => message,
        other => format!("{}: {message}", other.to_uppercase()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_start_refuses_to_change_the_docker_context() {
        let plain = ["start", "--activate=false", "--profile", "default"];
        assert_eq!(start_args("default", ResourceChange::default()), plain);
        let everything = ResourceChange { cpus: Some(4), memory: Some(8), disk: Some(120) };
        assert_eq!(
            start_args("default", everything),
            ["start", "--activate=false", "--profile", "default", "--cpus", "4", "--memory", "8", "--disk", "120"]
        );
    }

    #[test]
    fn only_what_changed_reaches_colima() {
        let current = Resources { cpus: 2, memory: 3, disk: 100 };
        let change = ResourceChange::between(Some(current), Resources { cpus: 4, ..current });
        assert_eq!(start_args("default", change)[4..], ["--cpus", "4"]);
        assert!(ResourceChange::between(Some(current), current).is_empty());
    }

    #[test]
    fn a_disk_is_never_asked_to_shrink() {
        let current = Resources { cpus: 2, memory: 2, disk: 100 };
        assert_eq!(ResourceChange::between(Some(current), Resources { disk: 40, ..current }).disk, None);
        assert_eq!(ResourceChange::between(Some(current), Resources { disk: 150, ..current }).disk, Some(150));
    }

    #[test]
    fn a_first_start_sends_every_value() {
        let wanted = Resources { cpus: 4, memory: 8, disk: 60 };
        let expected = ResourceChange { cpus: Some(4), memory: Some(8), disk: Some(60) };
        assert_eq!(ResourceChange::between(None, wanted), expected);
    }

    #[test]
    fn memory_choices_leave_room_for_macos() {
        assert_eq!(Resources::memory_choices(32, 2).last(), Some(&28));
        assert_eq!(Resources::memory_choices(32, 30).last(), Some(&30));
        assert_eq!(Resources::memory_choices(4, 2).last(), Some(&2));
        assert_eq!(Resources::memory_choices(2, 0), [1]);
    }

    #[test]
    fn disks_are_never_offered_smaller_than_they_are() {
        assert_eq!(Resources::disk_choices(Some(100)), [100, 150, 200, 300, 400, 500, 750, 1000]);
        assert_eq!(Resources::disk_choices(Some(130))[0], 130);
        assert_eq!(Resources::disk_choices(None)[0], 20);
    }

    #[test]
    fn current_resources_come_in_gib() {
        let status = ColimaStatus { cpu: 2, memory: 2 << 30, disk: 100 << 30, ..Default::default() };
        let info = ColimaInfo { status: Some(status), ..Default::default() };
        assert_eq!(Resources::current(&info), Some(Resources { cpus: 2, memory: 2, disk: 100 }));
        assert_eq!(Resources::current(&ColimaInfo::default()), None);
    }

    #[test]
    fn system_account_names_get_a_different_guest_user() {
        assert!(needs_guest_user_override("man"));
        assert!(!needs_guest_user_override("vudd"));
    }

    #[test]
    fn a_half_started_vm_counts_as_running() {
        let listed = |status: &str| ColimaProfile { status: status.into(), ..Default::default() };
        assert!(is_running(None, Some(&listed("Running"))));
        assert!(!is_running(None, Some(&listed("Stopped"))));
        assert!(is_running(Some(&ColimaStatus::default()), None));
        assert!(!is_running(None, None));
    }

    #[test]
    fn memory_in_use_leaves_out_cache() {
        let text = "               total        used        free      shared  buff/cache   available\n\
                    Mem:      2054840320   265801728  1275277312      806912   587444224  1789038592\n\
                    Swap:              0           0           0\n";
        assert_eq!(parse_free(text), Some((265801728, 2054840320)));
        assert_eq!(parse_free("nothing useful"), None);
    }

    #[test]
    fn log_lines_are_cleaned() {
        let info = r#"time="2026-11-06T10:25:40+07:00" level=info msg="starting colima""#;
        assert_eq!(clean_log_line(info), "starting colima");
        let fatal = r#"time="2026-11-06T10:25:40+07:00" level=fatal msg="colima is not running""#;
        assert_eq!(clean_log_line(fatal), "FATAL: colima is not running");
        let quoted = r#"level=warning msg="say \"hi\"""#;
        assert_eq!(clean_log_line(quoted), "WARNING: say \"hi\"");
        assert_eq!(clean_log_line("plain text"), "plain text");
        assert_eq!(clean_log_line(r#"time="now" level=info msg=done"#), "done");
    }

    #[test]
    fn version_line() {
        assert_eq!(parse_version("colima version 0.10.3\ngit commit: 00f6"), Some("0.10.3".into()));
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn profile_listing_is_one_json_object_per_line() {
        let text = "{\"name\":\"other\",\"status\":\"Stopped\"}\n{\"name\":\"default\",\"status\":\"Running\",\"cpus\":2}\n";
        let profile = parse_profile(text, "default").unwrap();
        assert_eq!(profile.status, "Running");
        assert_eq!(profile.cpus, 2);
        assert!(parse_profile(text, "missing").is_none());
    }

    #[test]
    fn search_path_reaches_homebrew_even_from_launchd() {
        let path = search_path();
        assert!(env::split_paths(&path).any(|d| d == Path::new("/opt/homebrew/bin")));
    }
}
