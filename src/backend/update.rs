//! "Check for Updates…": compares this build with the newest GitHub release.
//! Nothing here runs unless the user asks, and an update is always a step they
//! can watch: a release page, or a script in Terminal.

use super::colima::find_tool;
use super::docker::shell_quote;
use super::process::{Failure, run_limited};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Written by `scripts/install.sh` into the app's defaults.
pub const SOURCE_KEY: &str = "DPSourceDirectory";
const LATEST_RELEASE: &str = "https://api.github.com/repos/dockerplus/dockerplus/releases/latest";
/// Only there when Homebrew put this app here.
const CASKROOM: &str = "/opt/homebrew/Caskroom/dockerplus";
const CHECK_LIMIT: Duration = Duration::from_secs(20);
/// Lines of release notes shown in the dialog.
const NOTES_SHOWN: usize = 8;

#[derive(Debug, Deserialize, PartialEq)]
pub struct Release {
    pub tag_name: String,
    pub html_url: String,
    #[serde(default)]
    pub body: Option<String>,
}

/// How this copy got here, which decides how it updates.
#[derive(Debug, PartialEq)]
pub enum Install {
    Cask { brew: PathBuf },
    Source { git: PathBuf, dir: PathBuf },
    Download,
}

#[derive(Debug, PartialEq)]
pub enum Step {
    OpenPage(String),
    RunScript(String),
}

#[derive(Debug, PartialEq)]
pub struct Notice {
    pub title: String,
    pub detail: String,
    pub warning: bool,
    /// What the Update button does, when there is something to update to.
    pub step: Option<Step>,
}

pub fn how_installed(source: Option<&Path>) -> Install {
    if Path::new(CASKROOM).is_dir() {
        return Install::Cask { brew: find_tool("brew").unwrap_or_else(|| PathBuf::from("/opt/homebrew/bin/brew")) };
    }
    match source.filter(|dir| dir.join("scripts/install.sh").is_file()) {
        Some(dir) => Install::Source {
            // `/usr/bin/git` is always there on macOS.
            git: find_tool("git").unwrap_or_else(|| PathBuf::from("/usr/bin/git")),
            dir: dir.to_path_buf(),
        },
        None => Install::Download,
    }
}

#[cfg(not(feature = "devtools"))]
fn release_url() -> String {
    LATEST_RELEASE.to_string()
}

/// Lets a local server stand in for GitHub in automated checks.
#[cfg(feature = "devtools")]
fn release_url() -> String {
    std::env::var("DOCKERPLUS_RELEASE_URL").unwrap_or_else(|_| LATEST_RELEASE.to_string())
}

/// The newest release, or `None` while there is none. Curl rather than a TLS
/// library, which would weigh more than the app. Slow: call it off the main thread.
pub fn latest() -> Result<Option<Release>, String> {
    let url = release_url();
    let mut curl = Command::new("/usr/bin/curl");
    curl.args(["--fail", "--silent", "--show-error", "--location", &url]);
    let output = run_limited(&mut curl, CHECK_LIMIT).map_err(|failure| match failure {
        Failure::TimedOut => format!("GitHub gave no answer in {} seconds.", CHECK_LIMIT.as_secs()),
        Failure::Io(error) => format!("Could not run curl: {error}"),
    })?;
    if output.status.success() {
        return serde_json::from_slice(&output.stdout).map(Some).map_err(|e| format!("Unexpected reply from GitHub: {e}"));
    }
    let error = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if error.contains("404") { Ok(None) } else { Err(error) }
}

/// Compares `v1.10.0` and `1.9.2` as numbers. Anything else is never newer.
fn is_newer(tag: &str, current: &str) -> bool {
    let parts = |text: &str| -> Option<Vec<u64>> {
        text.trim_start_matches('v').split('.').map(|part| part.parse().ok()).collect()
    };
    matches!((parts(tag), parts(current)), (Some(tag), Some(current)) if tag > current)
}

fn notes(body: Option<&str>) -> Vec<&str> {
    let lines = body.unwrap_or("").lines().map(str::trim);
    lines.filter(|line| !line.is_empty() && !line.starts_with('#')).take(NOTES_SHOWN).collect()
}

pub fn notice(current: &str, latest: Result<Option<Release>, String>, install: &Install) -> Notice {
    let plain = |title: &str, detail: String, warning| Notice { title: title.into(), detail, warning, step: None };
    let release = match latest {
        Err(error) => return plain("Could not check for updates", error, true),
        Ok(None) => return plain("No releases yet", format!("You have DockerPlus {current}."), false),
        Ok(Some(release)) => release,
    };
    let version = release.tag_name.trim_start_matches('v');
    if !is_newer(&release.tag_name, current) {
        return plain("DockerPlus is up to date", format!("You have {current}, and the latest release is {version}."), false);
    }
    let (how, step) = match install {
        Install::Cask { brew } => {
            ("Update upgrades it with Homebrew in Terminal.", Step::RunScript(cask_script(brew)))
        }
        Install::Source { git, dir } => {
            ("Update pulls the new code and reinstalls in Terminal.", Step::RunScript(source_script(git, dir)))
        }
        Install::Download => ("Download opens the release page.", Step::OpenPage(release.html_url.clone())),
    };
    let quits = matches!(step, Step::RunScript(_)).then_some(" DockerPlus quits first; Colima keeps running.").unwrap_or("");
    let mut detail = format!("You have {current}.\n");
    for line in notes(release.body.as_deref()) {
        detail.push_str(&format!("\n{line}"));
    }
    detail.push_str(&format!("\n\n{how}{quits}"));
    Notice { title: format!("DockerPlus {version} is available"), detail, warning: false, step: Some(step) }
}

/// A Terminal .command. It deletes itself first, like the docker exec scripts,
/// so temp files do not pile up.
fn cask_script(brew: &Path) -> String {
    format!("#!/bin/sh\nrm -f \"$0\"\nexec {} upgrade --cask dockerplus\n", shell_quote(&brew.to_string_lossy()))
}

fn source_script(git: &Path, dir: &Path) -> String {
    format!(
        "#!/bin/sh\nrm -f \"$0\"\ncd {} || exit 1\n{} pull --ff-only && exec scripts/install.sh\n",
        shell_quote(&dir.to_string_lossy()),
        shell_quote(&git.to_string_lossy()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, body: Option<&str>) -> Release {
        Release { tag_name: tag.into(), html_url: "https://example.test/r".into(), body: body.map(String::from) }
    }

    #[test]
    fn versions_compare_as_numbers() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0", "0.2.0"));
        assert!(!is_newer("nightly", "0.1.0"));
    }

    #[test]
    fn a_release_reads_from_github_json() {
        let json = r#"{"tag_name":"v1.2.0","html_url":"https://example.test/r","body":null,"draft":false}"#;
        assert_eq!(serde_json::from_str::<Release>(json).unwrap(), release("v1.2.0", None));
    }

    #[test]
    fn nothing_to_do_offers_nothing() {
        let same = notice("0.2.0", Ok(Some(release("v0.2.0", None))), &Install::Download);
        assert_eq!(same.title, "DockerPlus is up to date");
        assert_eq!(same.step, None);
        let none = notice("0.2.0", Ok(None), &Install::Download);
        assert_eq!(none.title, "No releases yet");
        assert_eq!(none.step, None);
        let failed = notice("0.2.0", Err("offline".into()), &Install::Download);
        assert!(failed.warning);
        assert_eq!(failed.detail, "offline");
    }

    #[test]
    fn a_download_opens_the_release_page() {
        let found = notice("0.1.0", Ok(Some(release("v0.2.0", Some("## Changes\n\n* one\n* two\n")))), &Install::Download);
        assert_eq!(found.title, "DockerPlus 0.2.0 is available");
        assert_eq!(found.detail, "You have 0.1.0.\n\n* one\n* two\n\nDownload opens the release page.");
        assert_eq!(found.step, Some(Step::OpenPage("https://example.test/r".into())));
    }

    #[test]
    fn each_install_updates_its_own_way() {
        let cask = Install::Cask { brew: "/opt/homebrew/bin/brew".into() };
        let step = notice("0.1.0", Ok(Some(release("v0.2.0", None))), &cask).step;
        assert_eq!(step, Some(Step::RunScript("#!/bin/sh\nrm -f \"$0\"\nexec '/opt/homebrew/bin/brew' upgrade --cask dockerplus\n".into())));

        let source = Install::Source { git: "/usr/bin/git".into(), dir: "/Users/a/my code".into() };
        let Some(Step::RunScript(script)) = notice("0.1.0", Ok(Some(release("v0.2.0", None))), &source).step else {
            panic!("a source install runs a script");
        };
        assert!(script.starts_with("#!/bin/sh\nrm -f \"$0\"\n"));
        assert!(script.contains("cd '/Users/a/my code' || exit 1"));
        assert!(script.contains("pull --ff-only && exec scripts/install.sh"));
    }

    #[test]
    fn long_notes_are_cut_short() {
        let body = (1..=20).map(|n| format!("* {n}")).collect::<Vec<_>>().join("\n");
        assert_eq!(notes(Some(&body)).len(), NOTES_SHOWN);
        assert!(notes(None).is_empty());
    }

    #[test]
    fn only_a_folder_with_the_script_counts_as_a_source_install() {
        assert_eq!(how_installed(Some(Path::new("/nowhere"))), how_installed(None));
    }
}
