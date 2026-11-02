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
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

struct Fixture {
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
