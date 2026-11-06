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

impl Colima {
    pub fn locate(profile: &str) -> Option<Self> {
        let path = search_path();
        env::split_paths(&path)
            .map(|dir| dir.join("colima"))
            .find(|candidate| is_executable(candidate))
            .map(|bin| Self { bin, profile: profile.to_string() })
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

    pub fn start(&self, mut on_line: impl FnMut(String)) -> Result<(), String> {
        if let Some(note) = prepare_guest_user()? {
            on_line(note);
        }
        self.run(&start_args(&self.profile), on_line)
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

fn start_args(profile: &str) -> [&str; 4] {
    ["start", "--activate=false", "--profile", profile]
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
fn search_path() -> OsString {
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
        assert!(start_args("default").contains(&"--activate=false"));
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
