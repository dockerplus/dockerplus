//! Runs `bin/dockerplus` against a fake `docker` that reports how it was called,
//! so these need neither Colima nor a real engine.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

const FAKE_COMPOSE: &str = r#"#!/bin/sh
echo "plain compose, host=$DOCKER_HOST"
printf '[%s]' "$@"
"#;

const FAKE_DOCKER: &str = r#"#!/bin/sh
echo "host=$DOCKER_HOST tls=${DOCKER_TLS_VERIFY-unset} cert=${DOCKER_CERT_PATH-unset}"
printf '[%s]' "$@"
exit 3
"#;

struct Sandbox {
    home: PathBuf,
    _socket: Option<UnixListener>,
}

impl Sandbox {
    fn new(colima_running: bool) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let name = format!("dpw-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let home = std::env::temp_dir().join(name);
        fs::create_dir_all(home.join("bin")).unwrap();
        fs::create_dir_all(home.join(".colima/default")).unwrap();
        let home = home.canonicalize().unwrap();

        let socket = colima_running.then(|| UnixListener::bind(home.join(".colima/default/docker.sock")).unwrap());
        let sandbox = Self { home, _socket: socket };
        sandbox.add_tool("docker", FAKE_DOCKER);
        sandbox
    }

    fn add_tool(&self, name: &str, script: &str) {
        let path = self.home.join("bin").join(name);
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn run_in(&self, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("bin/dockerplus");
        let mut command = Command::new(script);
        command
            .args(args)
            .current_dir(dir)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", format!("{}:/usr/bin:/bin", self.home.join("bin").display()));
        command.envs(env.iter().copied());
        command.output().unwrap()
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_in(&self.home, args, &[])
    }

    fn socket_url(&self) -> String {
        format!("unix://{}/.colima/default/docker.sock", self.home.display())
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.home);
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn reached_docker(output: &Output) -> bool {
    stdout(output).contains("host=")
}

#[test]
fn arguments_reach_docker_pointed_at_colima() {
    let sandbox = Sandbox::new(true);
    let output = sandbox.run(&["run", "--name", "a b", "alpine", "echo", "c'd"]);
    assert!(stdout(&output).contains(&format!("host={}", sandbox.socket_url())));
    assert!(stdout(&output).contains("[run][--name][a b][alpine][echo][c'd]"));
    assert_eq!(output.status.code(), Some(3), "docker's exit code comes through");
    assert_eq!(stderr(&output), "");
}

#[test]
fn inherited_engine_settings_are_overridden() {
    let sandbox = Sandbox::new(true);
    let env = [("DOCKER_HOST", "tcp://remote:2376"), ("DOCKER_TLS_VERIFY", "1"), ("DOCKER_CERT_PATH", "/certs")];
    let output = sandbox.run_in(&sandbox.home, &["ps"], &env);
    assert!(stdout(&output).contains(&format!("host={} tls=unset cert=unset", sandbox.socket_url())));
}

#[test]
fn options_that_pick_another_engine_are_refused() {
    let sandbox = Sandbox::new(true);
    for args in [
        &["--context", "desktop-linux", "ps"][..],
        &["--context=desktop-linux", "ps"],
        &["-c", "desktop-linux", "ps"],
        &["-cdesktop-linux", "ps"],
        &["-Dc", "desktop-linux", "ps"],
        &["--host", "unix:///other", "ps"],
        &["--host=unix:///other", "ps"],
        &["-H", "unix:///other", "ps"],
        &["--debug", "--context", "desktop-linux", "ps"],
    ] {
        let output = sandbox.run(args);
        assert!(!reached_docker(&output), "{args:?}");
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(stderr(&output).contains("would leave Colima"), "{args:?}");
    }
}

#[test]
fn context_commands_are_refused_however_they_are_reached() {
    let sandbox = Sandbox::new(true);
    for args in [&["context", "use", "orbstack"][..], &["--config", "/tmp/x", "context", "ls"], &["-D", "context", "ls"]] {
        let output = sandbox.run(args);
        assert!(!reached_docker(&output), "{args:?}");
        assert_eq!(output.status.code(), Some(2), "{args:?}");
    }
}

#[test]
fn the_same_letters_after_the_command_are_fine() {
    let sandbox = Sandbox::new(true);
    let output = sandbox.run(&["run", "-c", "512", "-h", "name", "alpine"]);
    assert!(reached_docker(&output));
    assert!(stdout(&output).contains("[run][-c][512]"));
}

#[test]
fn bind_mounts_outside_home_are_flagged() {
    let sandbox = Sandbox::new(true);
    for args in [
        &["run", "-v", "/usr/bin:/x", "alpine"][..],
        &["run", "--volume=/usr/bin:/x", "alpine"],
        &["create", "-v=/usr/bin:/x:ro", "alpine"],
    ] {
        let output = sandbox.run(args);
        assert!(stderr(&output).contains("/usr/bin is outside your home folder"), "{args:?}");
        assert!(reached_docker(&output), "a warning must not stop the command: {args:?}");
    }
}

#[test]
fn bind_mounts_that_will_work_stay_quiet() {
    let sandbox = Sandbox::new(true);
    let inside = format!("{}/bin:/x", sandbox.home.display());
    for args in [
        &["run", "-v", inside.as_str(), "alpine"][..],
        &["run", "-v", "named:/x", "alpine"],
        &["run", "-v", "/no/such/path:/x", "alpine"],
    ] {
        assert_eq!(stderr(&sandbox.run(args)), "", "{args:?}");
    }
}

#[test]
fn compose_warns_when_run_outside_home() {
    let sandbox = Sandbox::new(true);
    let outside = sandbox.run_in(Path::new("/usr"), &["compose", "up"], &[]);
    assert!(stderr(&outside).contains("/usr is outside your home folder"));
    assert!(reached_docker(&outside));
    assert_eq!(stderr(&sandbox.run(&["compose", "up"])), "");
}

#[test]
fn compose_uses_the_plain_binary_when_docker_has_no_plugin() {
    let sandbox = Sandbox::new(true);
    sandbox.add_tool("docker-compose", FAKE_COMPOSE);
    let output = sandbox.run(&["--debug", "compose", "up", "-d"]);
    assert!(stdout(&output).contains(&format!("plain compose, host={}", sandbox.socket_url())));
    assert!(stdout(&output).contains("[up][-d]"));
}

#[test]
fn compose_prefers_the_plugin() {
    let sandbox = Sandbox::new(true);
    sandbox.add_tool("docker", "#!/bin/sh\necho plugin\n");
    sandbox.add_tool("docker-compose", FAKE_COMPOSE);
    assert!(stdout(&sandbox.run(&["compose", "up"])).contains("plugin"));
}

#[test]
fn with_colima_stopped_docker_still_runs_and_the_user_gets_a_hint() {
    let sandbox = Sandbox::new(false);
    let output = sandbox.run(&["--version"]);
    assert!(reached_docker(&output));
    assert!(stderr(&output).contains("Colima is not running"));
}

#[test]
fn colima_home_moves_the_socket() {
    let sandbox = Sandbox::new(true);
    let moved = sandbox.home.join("co lima");
    let moved_text = moved.to_string_lossy().into_owned();
    let output = sandbox.run_in(&sandbox.home, &["ps"], &[("COLIMA_HOME", moved_text.as_str())]);
    assert!(stdout(&output).contains(&format!("host=unix://{moved_text}/default/docker.sock")));
}
