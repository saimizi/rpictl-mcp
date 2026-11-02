#![cfg(target_os = "linux")]
//! Exercise the real daemon, serial reader and power command using a PTY and
//! a child-local fake chip-tool. No board, global PATH change or new dependency.
use std::ffi::CStr;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::{fs::PermissionsExt, net::UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

// Each fixture launches a daemon and subprocesses. Avoid host contention
// invalidating short protocol deadlines; concurrency inside each test remains.
static DAEMON_FIXTURE: Mutex<()> = Mutex::new(());

struct Fixture {
    _resource_guard: MutexGuard<'static, ()>,
    directory: PathBuf,
    daemon: Child,
    master: File,
    _slave: File,
}

fn rpc(socket: &Path, method: &str, params: Value) -> Value {
    let mut stream = UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    writeln!(
        stream,
        "RPC {}",
        json!({"method": method, "params": params})
    )
    .unwrap();
    let mut response = String::new();
    BufReader::new(stream).read_line(&mut response).unwrap();
    serde_json::from_str(&response).unwrap()
}

fn wait_for(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready() {
        assert!(Instant::now() < deadline, "simulated controller stalled");
        thread::sleep(Duration::from_millis(10));
    }
}

impl Fixture {
    fn new(power_fails: bool) -> Self {
        Self::with_timeout(power_fails, 30_000)
    }

    fn with_timeout(power_fails: bool, timeout_ms: u64) -> Self {
        let resource_guard = DAEMON_FIXTURE
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let directory = std::env::temp_dir().join(format!("rpictl-boot-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        let mut master = -1;
        let mut slave = -1;
        let mut name = [0 as libc::c_char; 128];
        // SAFETY: valid output pointers; null termios/winsize request defaults.
        // On success each returned descriptor is transferred to one File owner.
        let result = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                name.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        assert_eq!(result, 0, "{}", std::io::Error::last_os_error());
        let (master, slave) = unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) };
        // SAFETY: openpty returned a NUL-terminated device name in this buffer.
        let device = unsafe { CStr::from_ptr(name.as_ptr()) }.to_str().unwrap();
        let config = json!({"boards": [{
            "board_id": "simulated", "serial": {"device": device},
            "power": {"backend": "matter", "identity": "node=991;endpoint=1"},
            "timing": {"operation_timeout_ms": timeout_ms},
            "patterns": {"uboot_countdown": "Hit any key", "uboot_prompt": "U-Boot>",
                "linux_boot": "Linux version", "linux_login": "login:", "linux_shell": "root@fake:#"}
        }]});
        fs::write(directory.join("boards.json"), config.to_string()).unwrap();
        fs::write(directory.join("state"), "off").unwrap();
        // Confirmation cannot return until the test releases it. This enforces
        // the problematic ordering without depending on a particular sleep.
        let fake_power = directory.join("chip-tool");
        fs::write(
            &fake_power,
            r#"#!/bin/sh
set -eu
if [ -e "$FAKE_RELAY_DIR/hang-descendant" ]; then
    echo $$ > "$FAKE_RELAY_DIR/hung-pid"
    sleep 60 &
    echo $! > "$FAKE_RELAY_DIR/inherited-pipe-pid"
    exit 0
fi
if [ -e "$FAKE_RELAY_DIR/noise-$2" ]; then
    mode=$(cat "$FAKE_RELAY_DIR/noise-$2")
    if [ "$mode" != below ]; then
        echo $$ > "$FAKE_RELAY_DIR/noise-pid"
        sleep 60 &
        echo $! > "$FAKE_RELAY_DIR/noise-descendant"
    fi
    case "$mode" in
    stdout) head -c 2097152 /dev/zero ;;
    stderr) head -c 2097152 /dev/zero >&2 ;;
    combined) head -c 600000 /dev/zero; head -c 600000 /dev/zero >&2 ;;
    below) head -c 131072 /dev/zero; head -c 131072 /dev/zero >&2 ;;
    esac
    if [ "$mode" != below ]; then wait; fi
fi
if [ -e "$FAKE_RELAY_DIR/hang-$2" ] ||
   { [ "$2" = read ] && [ -e "$FAKE_RELAY_DIR/hang-verify" ] && [ "$(cat "$FAKE_RELAY_DIR/state")" = on ]; }; then
    echo $$ > "$FAKE_RELAY_DIR/hung-pid"
    # Exceed a pipe's capacity before hanging to exercise output handling too.
    head -c 131072 /dev/zero
    exec sleep 60
fi
case "$2" in
read)
    if [ "$(cat "$FAKE_RELAY_DIR/state")" = on ]; then
        echo 'OnOff: TRUE'
    else
        echo 'OnOff: FALSE'
    fi
    ;;
on)
    echo on > "$FAKE_RELAY_DIR/state"
    count=0
    while [ ! -e "$FAKE_RELAY_DIR/release" ]; do
        count=$((count + 1))
        [ "$count" -lt 500 ] || exit 1
        sleep 0.01
    done
    if [ "$FAKE_RELAY_FAIL" = yes ]; then
        echo 'simulated relay confirmation failure'
        exit 1
    fi
    echo SUCCESS
    ;;
*) exit 1 ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(&fake_power, fs::Permissions::from_mode(0o755)).unwrap();
        let mut paths = vec![directory.clone()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let log = File::create(directory.join("daemon.log")).unwrap();
        let daemon = Command::new(env!("CARGO_BIN_EXE_rpictl-mcp"))
            .arg("daemon")
            .arg(directory.join("boards.json"))
            .arg("--foreground")
            .arg(directory.join("controller.sock"))
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env("FAKE_RELAY_DIR", &directory)
            .env("FAKE_RELAY_FAIL", if power_fails { "yes" } else { "no" })
            .env("TOKIO_WORKER_THREADS", "2")
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        let mut fixture = Self {
            _resource_guard: resource_guard,
            directory,
            daemon,
            master,
            _slave: slave,
        };
        wait_for(|| {
            assert!(
                fixture.daemon.try_wait().unwrap().is_none(),
                "daemon exited: {}",
                fs::read_to_string(fixture.directory.join("daemon.log")).unwrap()
            );
            UnixStream::connect(fixture.socket()).is_ok()
        });
        fixture
    }

    fn socket(&self) -> PathBuf {
        self.directory.join("controller.sock")
    }

    fn start_boot(&self, timeout_ms: u64) -> thread::JoinHandle<Value> {
        let socket = self.socket();
        thread::spawn(move || {
            rpc(
                &socket,
                "power_on_uboot",
                json!({"board_id": "simulated", "timeout_ms": timeout_ms}),
            )
        })
    }

    fn powered(&self) {
        wait_for(|| {
            fs::read_to_string(self.directory.join("state"))
                .unwrap()
                .trim()
                == "on"
        });
    }

    fn available(&self) -> bool {
        let response = rpc(&self.socket(), "list_boards", Value::Null);
        assert_eq!(response["ok"], true);
        response["result"][0]["available"].as_bool().unwrap()
    }

    fn interrupt(&mut self) {
        self.master
            .write_all(b"\r\nHit any key to stop autoboot: 1\r\n")
            .unwrap();
        let mut descriptor = libc::pollfd {
            fd: self.master.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one initialized pollfd, owned PTY descriptor, bounded wait.
        assert_eq!(
            unsafe { libc::poll(&mut descriptor, 1, 2000) },
            1,
            "countdown was not interrupted while relay confirmation was pending"
        );
        let mut byte = [0];
        self.master.read_exact(&mut byte).unwrap();
        assert_eq!(byte, [b' '], "expected the configured interrupt key");
        self.master.write_all(b"\r\nU-Boot> ").unwrap();
        // Wait for the real serial reader to consume the fresh prompt, while
        // relay confirmation is still blocked in the fake command.
        wait_for(|| {
            rpc(
                &self.socket(),
                "get_board_state",
                json!({"board_id": "simulated", "active_probe": false, "observe_duration_ms": 1}),
            )["result"]["state"]
                == "uboot_prompt"
        });
    }

    fn release(&self) {
        fs::write(self.directory.join("release"), "").unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::write(self.directory.join("release"), "");
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn interrupts_countdown_before_relay_confirmation_finishes() {
    let mut fixture = Fixture::new(false);
    let boot = fixture.start_boot(3000);
    fixture.powered();
    fixture.interrupt();
    assert!(
        !boot.is_finished(),
        "success must wait for power verification"
    );
    assert!(
        !fixture.available(),
        "operation must retain the board lease"
    );
    fixture.release();
    let response = boot.join().unwrap();
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["result"]["success"], true);
    assert_eq!(response["result"]["observed_state"], "uboot_prompt");
    assert!(fixture.available());
}

#[test]
fn prompt_does_not_hide_failed_power_verification() {
    let mut fixture = Fixture::new(true);
    let boot = fixture.start_boot(3000);
    fixture.powered();
    fixture.interrupt();
    fixture.release();
    let response = boot.join().unwrap();
    assert_eq!(response["ok"], false, "{response}");
    assert!(
        response["error"]
            .as_str()
            .unwrap()
            .contains("POWER_BACKEND_FAILED")
    );
    assert!(fixture.available());
}

#[test]
fn countdown_timeout_joins_power_worker_before_releasing_lease() {
    let fixture = Fixture::new(false);
    let boot = fixture.start_boot(200);
    fixture.powered();
    thread::sleep(Duration::from_millis(400));
    assert!(!boot.is_finished());
    assert!(!fixture.available());
    fixture.release();
    let response = boot.join().unwrap();
    assert_eq!(response["ok"], false, "{response}");
    assert!(
        response["error"]
            .as_str()
            .unwrap()
            .contains("STATE_TIMEOUT")
    );
    assert!(fixture.available());
}

#[test]
fn power_failure_returns_without_waiting_for_countdown() {
    let fixture = Fixture::new(true);
    // Much longer than the fixture's bounded wait for an early response.
    let boot = fixture.start_boot(30_000);
    fixture.powered();
    fixture.release();
    wait_for(|| boot.is_finished());
    let response = boot.join().unwrap();
    assert_eq!(response["ok"], false, "{response}");
    assert!(
        response["error"]
            .as_str()
            .unwrap()
            .contains("POWER_BACKEND_FAILED")
    );
    assert!(
        fixture.available(),
        "failed operation must release the lease"
    );
}

fn assert_hung_command_times_out(marker: &str) {
    let fixture = Fixture::with_timeout(false, 800);
    fs::write(fixture.directory.join(marker), "").unwrap();
    fixture.release();
    let boot = fixture.start_boot(200);
    wait_for(|| fixture.directory.join("hung-pid").exists());
    let pid: i32 = fs::read_to_string(fixture.directory.join("hung-pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    wait_for(|| boot.is_finished());
    let response = boot.join().unwrap();
    assert_eq!(response["ok"], false, "{response}");
    assert!(
        response["error"]
            .as_str()
            .unwrap()
            .contains("COMMAND_TIMEOUT"),
        "{response}"
    );
    // No live process or unreaped zombie may remain when the lease is released.
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    if let Ok(descendant) = fs::read_to_string(fixture.directory.join("inherited-pipe-pid")) {
        wait_for(
            || match fs::read_to_string(format!("/proc/{}/stat", descendant.trim())) {
                Ok(stat) => stat.rsplit_once(") ").unwrap().1.starts_with('Z'),
                Err(error) => error.kind() == std::io::ErrorKind::NotFound,
            },
        );
    }
    assert!(fixture.available());
    fs::remove_file(fixture.directory.join(marker)).unwrap();
    assert_eq!(
        rpc(
            &fixture.socket(),
            "get_power_state",
            json!({"board_id": "simulated"})
        )["ok"],
        true
    );
}

#[test]
fn hanging_power_query_is_terminated_and_reaped() {
    assert_hung_command_times_out("hang-read");
}

#[test]
fn countdown_timeout_still_terminates_and_reaps_hanging_power_worker() {
    assert_hung_command_times_out("hang-on");
}

#[test]
fn hanging_post_power_verification_is_terminated_and_reaped() {
    assert_hung_command_times_out("hang-verify");
}

fn check_already_reached_prompt(output: &[u8]) {
    let mut fixture = Fixture::new(false);
    let boot = fixture.start_boot(3000);
    fixture.powered();
    fixture.master.write_all(output).unwrap();
    let mut descriptor = libc::pollfd {
        fd: fixture.master.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    assert_eq!(
        unsafe { libc::poll(&mut descriptor, 1, 2000) },
        1,
        "fresh prompt was not synchronized"
    );
    let mut byte = [0];
    fixture.master.read_exact(&mut byte).unwrap();
    assert_eq!(
        byte,
        [b'\r'],
        "send Enter rather than a countdown interrupt at an existing prompt"
    );
    fixture.master.write_all(b"\r\nU-Boot> ").unwrap();
    fixture.release();
    let response = boot.join().unwrap();
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["result"]["observed_state"], "uboot_prompt");
    let actions = response["result"]["actions"].to_string();
    assert!(actions.contains("already reached"), "{response}");
    assert!(!actions.contains("interrupted"), "{response}");
    assert!(fixture.available());
}

#[test]
fn combined_countdown_and_prompt_synchronizes_existing_prompt() {
    check_already_reached_prompt(b"Hit any key to stop autoboot: 1\r\nU-Boot> ");
}

#[test]
fn fresh_prompt_without_countdown_is_reported_as_already_reached() {
    check_already_reached_prompt(b"\r\nU-Boot> ");
}

#[test]
fn stale_prompt_before_power_on_does_not_satisfy_boot_wait() {
    let mut fixture = Fixture::new(false);
    fixture.master.write_all(b"\r\nU-Boot> ").unwrap();
    wait_for(|| {
        rpc(
            &fixture.socket(),
            "get_board_state",
            json!({
                "board_id": "simulated", "active_probe": false, "observe_duration_ms": 1
            }),
        )["result"]["state"]
            == "uboot_prompt"
    });
    fixture.release();
    let boot = fixture.start_boot(200);
    let response = boot.join().unwrap();
    assert_eq!(response["ok"], false, "{response}");
    assert!(
        response["error"]
            .as_str()
            .unwrap()
            .contains("STATE_TIMEOUT"),
        "{response}"
    );
    assert!(fixture.available());
}

fn assert_excess_output_is_cleaned_up(command: &str, mode: &str) {
    let fixture = Fixture::new(false);
    let marker = fixture.directory.join(format!("noise-{command}"));
    fs::write(&marker, mode).unwrap();
    fixture.release();
    let boot = fixture.start_boot(30_000);
    wait_for(|| boot.is_finished());
    let response = boot.join().unwrap();
    assert_eq!(response["ok"], false, "{response}");
    let error = response["error"].as_str().unwrap();
    assert!(error.contains("OUTPUT_TRUNCATED"), "{response}");
    assert!(error.contains("1048576"), "{response}");
    assert!(
        error.len() < 512,
        "error must not include excessive command output"
    );
    let pid: i32 = fs::read_to_string(fixture.directory.join("noise-pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "command must be reaped before RPC completion"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    let descendant = fs::read_to_string(fixture.directory.join("noise-descendant")).unwrap();
    wait_for(
        || match fs::read_to_string(format!("/proc/{}/stat", descendant.trim())) {
            Ok(stat) => stat.rsplit_once(") ").unwrap().1.starts_with('Z'),
            Err(error) => error.kind() == std::io::ErrorKind::NotFound,
        },
    );
    assert!(
        fixture.available(),
        "worker must finish before lease release"
    );
    fs::remove_file(marker).unwrap();
    assert_eq!(
        rpc(
            &fixture.socket(),
            "get_power_state",
            json!({"board_id": "simulated"})
        )["ok"],
        true
    );
}

#[test]
fn excessive_query_stdout_is_bounded_and_reaped() {
    assert_excess_output_is_cleaned_up("read", "stdout");
}

#[test]
fn excessive_worker_stderr_is_bounded_and_reaped() {
    assert_excess_output_is_cleaned_up("on", "stderr");
}

#[test]
fn output_limit_is_shared_across_stdout_and_stderr() {
    assert_excess_output_is_cleaned_up("on", "combined");
}

#[test]
fn substantial_output_on_both_streams_below_limit_does_not_deadlock() {
    let fixture = Fixture::new(false);
    fs::write(fixture.directory.join("noise-read"), "below").unwrap();
    let response = rpc(
        &fixture.socket(),
        "get_power_state",
        json!({"board_id": "simulated"}),
    );
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["result"]["state"], "off");
}

#[test]
fn exited_leader_with_inherited_output_pipes_still_times_out_and_cleans_up() {
    assert_hung_command_times_out("hang-descendant");
}
