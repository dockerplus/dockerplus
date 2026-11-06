//! "Check for Updates…": the app is built from a git checkout, so an update is
//! new commits upstream of the folder `scripts/install.sh` ran from. Nothing
//! here runs unless the user asks.

use super::colima::{find_tool, search_path};
use super::docker::shell_quote;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Written by `scripts/install.sh` into the app's defaults.
pub const SOURCE_KEY: &str = "DPSourceDirectory";
const FETCH_LIMIT: Duration = Duration::from_secs(30);
const LOCAL_LIMIT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(50);
/// Commit titles listed in the dialog; the rest are only counted.
const TITLES_SHOWN: usize = 8;
const HOW_TO_UPDATE: &str = "To update, run this in the DockerPlus source folder:\n\ngit pull\nscripts/install.sh";

#[derive(Debug, PartialEq)]
pub enum Check {
    /// Not installed by `scripts/install.sh`, so the source folder is unknown.
    NoSource,
    NotCheckout(PathBuf),
    Detached(PathBuf),
    NoUpstream { dir: PathBuf, branch: String },
    Failed { dir: PathBuf, error: String },
    Done(Status),
}

#[derive(Debug, PartialEq)]
pub struct Status {
    pub dir: PathBuf,
    pub upstream: String,
    pub head: String,
    pub ahead: usize,
    pub behind: usize,
    /// Titles of the newest upstream commits, newest first.
    pub titles: Vec<String>,
    /// Tracked files have uncommitted changes. Only looked at when behind.
    pub dirty: bool,
}

#[derive(Debug, PartialEq)]
pub struct Notice {
    pub title: String,
    pub detail: String,
    pub warning: bool,
    /// The folder to pull and reinstall from, when that is safe to offer.
    pub update: Option<PathBuf>,
}

/// Git as a login shell would find it; `/usr/bin/git` is always there on macOS.
pub fn git_tool() -> PathBuf {
    find_tool("git").unwrap_or_else(|| PathBuf::from("/usr/bin/git"))
}

/// Fetches and compares with upstream. Slow: call it off the main thread.
pub fn check(source: Option<&Path>) -> Check {
    let Some(dir) = source else { return Check::NoSource };
    let git = git_tool();
    let run = |args: &[&str], limit| run_git(&git, dir, args, limit);
    let dir = dir.to_path_buf();
    if !dir.join("scripts/install.sh").is_file() || run(&["rev-parse", "--is-inside-work-tree"], LOCAL_LIMIT).is_err() {
        return Check::NotCheckout(dir);
    }
    let Ok(branch) = run(&["symbolic-ref", "--quiet", "--short", "HEAD"], LOCAL_LIMIT) else {
        return Check::Detached(dir);
    };
    let Ok(upstream) = run(&["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"], LOCAL_LIMIT) else {
        return Check::NoUpstream { dir, branch: branch.trim().to_string() };
    };
    let status = || -> Result<Status, String> {
        run(&["fetch", "--quiet"], FETCH_LIMIT)?;
        let counts = run(&["rev-list", "--left-right", "--count", "HEAD...@{u}"], LOCAL_LIMIT)?;
        let (ahead, behind) = parse_counts(&counts).ok_or_else(|| format!("unexpected output: {counts}"))?;
        let head = run(&["rev-parse", "--short", "HEAD"], LOCAL_LIMIT)?.trim().to_string();
        let (mut titles, mut dirty) = (Vec::new(), false);
        if behind > 0 {
            let shown = format!("-n{TITLES_SHOWN}");
            titles = run(&["log", "--format=%s", &shown, "HEAD..@{u}"], LOCAL_LIMIT)?.lines().map(String::from).collect();
            dirty = !run(&["status", "--porcelain", "--untracked-files=no"], LOCAL_LIMIT)?.trim().is_empty();
        }
        let upstream = upstream.trim().to_string();
        Ok(Status { dir: dir.clone(), upstream, head, ahead, behind, titles, dirty })
    };
    match status() {
        Ok(status) => Check::Done(status),
        Err(error) => Check::Failed { dir, error },
    }
}

/// Runs git with no terminal to prompt on, giving up after `limit`.
fn run_git(git: &Path, dir: &Path, args: &[&str], limit: Duration) -> Result<String, String> {
    let mut child = Command::new(git)
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("PATH", search_path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run git: {e}"))?;
    // Drained on their own threads so a chatty command cannot fill a pipe and stall.
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let end = Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if Instant::now() >= end {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("git {} gave no answer in {} seconds.", args[0], limit.as_secs()));
        }
        thread::sleep(POLL);
    };
    let out = out.join().unwrap_or_default();
    let err = err.join().unwrap_or_default();
    if status.success() { Ok(out) } else { Err(err.trim().to_string()) }
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

/// `git rev-list --left-right --count HEAD...@{u}` prints "ahead<TAB>behind".
fn parse_counts(text: &str) -> Option<(usize, usize)> {
    let mut parts = text.split_whitespace();
    let ahead = parts.next()?.parse().ok()?;
    let behind = parts.next()?.parse().ok()?;
    Some((ahead, behind))
}

fn commits(n: usize) -> String {
    format!("{n} {}", if n == 1 { "commit" } else { "commits" })
}

pub fn notice(check: &Check) -> Notice {
    let plain = |title: &str, detail: String, warning| Notice { title: title.into(), detail, warning, update: None };
    match check {
        Check::NoSource => plain(
            "DockerPlus does not know where its source is",
            format!("It was not installed with scripts/install.sh. {HOW_TO_UPDATE}"),
            false,
        ),
        Check::NotCheckout(dir) => plain(
            "The source folder is gone",
            format!("{} is no longer a DockerPlus git checkout. {HOW_TO_UPDATE}", dir.display()),
            true,
        ),
        Check::Detached(dir) => plain(
            "The source folder is not on a branch",
            format!("{} has a detached HEAD, so there is no branch to update. Check out a branch, then run git pull and scripts/install.sh.", dir.display()),
            true,
        ),
        Check::NoUpstream { dir, branch } => plain(
            "The branch has no upstream",
            format!("Branch {branch} in {} does not track a remote branch, so there is nothing to compare with. Set one with git branch --set-upstream-to, or update by hand.", dir.display()),
            true,
        ),
        Check::Failed { dir, error } => plain(
            "Could not check for updates",
            format!("Git failed in {}:\n\n{error}", dir.display()),
            true,
        ),
        Check::Done(status) => status_notice(status),
    }
}

fn status_notice(s: &Status) -> Notice {
    let dir = s.dir.display();
    let plain = |title: &str, detail: String, warning| Notice { title: title.into(), detail, warning, update: None };
    if s.behind == 0 {
        // Only the folder is known: the app does not record the commit it was built from.
        let detail = match s.ahead {
            0 => format!("The source folder is at {}, the same as {}.", s.head, s.upstream),
            n => format!("The source folder is at {}, {} ahead of {}.", s.head, commits(n), s.upstream),
        };
        return plain("No updates found", detail, false);
    }
    if s.ahead > 0 {
        let detail = format!(
            "{dir} has {} of its own and {} has {} new, so a fast-forward pull is not possible. Merge or rebase them, then run scripts/install.sh.",
            commits(s.ahead), s.upstream, commits(s.behind),
        );
        return plain("The source folder has diverged", detail, true);
    }
    if s.dirty {
        let detail = format!(
            "{} {} on {}, but {dir} has changes that are not committed. Commit or stash them, then run git pull and scripts/install.sh.",
            commits(s.behind), if s.behind == 1 { "is new" } else { "are new" }, s.upstream,
        );
        return plain("The source folder has uncommitted changes", detail, true);
    }
    let mut detail = format!("{} on {}:\n", commits(s.behind), s.upstream);
    for title in &s.titles {
        detail.push_str(&format!("\n• {title}"));
    }
    if s.behind > s.titles.len() {
        detail.push_str(&format!("\n…and {} more", s.behind - s.titles.len()));
    }
    detail.push_str("\n\nUpdate pulls them and reinstalls in Terminal. DockerPlus quits while it installs; Colima keeps running.");
    Notice { title: "A new version of DockerPlus is available".into(), detail, warning: false, update: Some(s.dir.clone()) }
}

/// A Terminal .command that pulls and reinstalls. It deletes itself first, like
/// the docker exec scripts, so temp files do not pile up.
pub fn update_script(git: &Path, dir: &Path) -> String {
    format!(
        "#!/bin/sh\nrm -f \"$0\"\ncd {} || exit 1\n{} pull --ff-only && exec scripts/install.sh\n",
        shell_quote(&dir.to_string_lossy()),
        shell_quote(&git.to_string_lossy()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn status(ahead: usize, behind: usize, dirty: bool) -> Status {
        let titles = (0..behind.min(TITLES_SHOWN)).map(|i| format!("change {i}")).collect();
        Status { dir: "/src/dp".into(), upstream: "origin/main".into(), head: "abc1234".into(), ahead, behind, titles, dirty }
    }

    #[test]
    fn parses_left_right_counts() {
        assert_eq!(parse_counts("2\t5\n"), Some((2, 5)));
        assert_eq!(parse_counts("0\t0"), Some((0, 0)));
        assert_eq!(parse_counts("x"), None);
    }

    #[test]
    fn only_a_clean_fast_forward_offers_the_update() {
        let fresh = notice(&Check::Done(status(0, 3, false)));
        assert_eq!(fresh.update, Some(PathBuf::from("/src/dp")));
        assert!(fresh.detail.contains("3 commits on origin/main"));
        assert!(fresh.detail.contains("• change 2"));
        for case in [status(0, 0, false), status(2, 0, false), status(1, 3, false), status(0, 3, true)] {
            assert_eq!(notice(&Check::Done(case)).update, None);
        }
        let failures = [
            Check::NoSource,
            Check::NotCheckout("/x".into()),
            Check::Detached("/x".into()),
            Check::NoUpstream { dir: "/x".into(), branch: "dev".into() },
            Check::Failed { dir: "/x".into(), error: "offline".into() },
        ];
        for case in failures {
            assert_eq!(notice(&case).update, None);
        }
    }

    #[test]
    fn notices_name_the_situation() {
        assert_eq!(notice(&Check::Done(status(0, 0, false))).title, "No updates found");
        assert!(notice(&Check::Done(status(0, 0, false))).detail.contains("abc1234"));
        assert!(notice(&Check::Done(status(2, 0, false))).detail.contains("2 commits ahead of origin/main"));
        assert_eq!(notice(&Check::Done(status(1, 3, false))).title, "The source folder has diverged");
        assert!(notice(&Check::Done(status(0, 1, true))).detail.contains("1 commit is new"));
        assert!(notice(&Check::NoSource).detail.contains("scripts/install.sh"));
        assert!(notice(&Check::NoUpstream { dir: "/x".into(), branch: "dev".into() }).detail.contains("Branch dev"));
    }

    #[test]
    fn lists_a_few_titles_and_counts_the_rest() {
        let detail = notice(&Check::Done(status(0, TITLES_SHOWN + 4, false))).detail;
        assert_eq!(detail.matches('•').count(), TITLES_SHOWN);
        assert!(detail.contains("…and 4 more"));
    }

    #[test]
    fn script_quotes_the_folder_and_removes_itself() {
        let script = update_script(Path::new("/usr/bin/git"), Path::new("/Users/a/it's here"));
        assert!(script.starts_with("#!/bin/sh\nrm -f \"$0\"\n"));
        assert!(script.contains(r"cd '/Users/a/it'\''s here' || exit 1"));
        assert!(script.contains("'/usr/bin/git' pull --ff-only && exec scripts/install.sh"));
    }

    /// A scratch folder under the system temp dir, removed when dropped.
    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com", "-c", "commit.gpgsign=false"])
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok, "git {args:?} failed in {}", dir.display());
    }

    fn commit(dir: &Path, title: &str) {
        fs::write(dir.join("file.txt"), title).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", title]);
    }

    #[test]
    fn checks_a_real_clone() {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let scratch = Scratch(std::env::temp_dir().join(format!("dockerplus-update-test-{}-{stamp}", std::process::id())));
        let (upstream, local) = (scratch.0.join("upstream"), scratch.0.join("local"));
        fs::create_dir_all(upstream.join("scripts")).unwrap();
        fs::write(upstream.join("scripts/install.sh"), "#!/bin/sh\n").unwrap();
        git(&upstream, &["init", "-q", "-b", "main"]);
        commit(&upstream, "first");
        git(&scratch.0, &["clone", "-q", "upstream", "local"]);

        let Check::Done(fresh) = check(Some(&local)) else { panic!("clean clone should check") };
        assert_eq!((fresh.ahead, fresh.behind, fresh.upstream.as_str()), (0, 0, "origin/main"));

        commit(&upstream, "second");
        commit(&upstream, "third");
        let Check::Done(behind) = check(Some(&local)) else { panic!("clone should check") };
        assert_eq!((behind.behind, behind.titles.clone(), behind.dirty), (2, vec!["third".into(), "second".into()], false));

        fs::write(local.join("file.txt"), "edited").unwrap();
        let Check::Done(dirty) = check(Some(&local)) else { panic!("clone should check") };
        assert!(dirty.dirty);
        commit(&local, "local");
        let Check::Done(diverged) = check(Some(&local)) else { panic!("clone should check") };
        assert_eq!((diverged.ahead, diverged.behind), (1, 2));

        git(&local, &["checkout", "-q", "-b", "topic"]);
        assert_eq!(check(Some(&local)), Check::NoUpstream { dir: local.clone(), branch: "topic".into() });
        git(&local, &["checkout", "-q", "--detach"]);
        assert_eq!(check(Some(&local)), Check::Detached(local.clone()));
        assert_eq!(check(Some(&scratch.0)), Check::NotCheckout(scratch.0.clone()));
        assert_eq!(check(None), Check::NoSource);
    }
}
