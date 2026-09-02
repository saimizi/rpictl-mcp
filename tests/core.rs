use std::time::{Duration, SystemTime};

use rpictl_mcp::config::*;
use rpictl_mcp::lease::{BoardLease, LeaseOwner};
use rpictl_mcp::serial::SerialRing;
use rpictl_mcp::state::{BoardState, StateDetector};
use rpictl_mcp::{BoardRegistry, ErrorCode};

fn board(id: &str, device: &str, power_id: &str) -> BoardProfile {
    BoardProfile {
        board_id: id.into(),
        display_name: None,
        serial: SerialProfile {
            device: device.into(),
            baud_rate: 115200,
            data_bits: 8,
            parity: Parity::None,
            stop_bits: 1,
            flow_control: FlowControl::None,
        },
        power: PowerProfile {
            backend: "fake".into(),
            identity: power_id.into(),
        },
        patterns: StatePatterns {
            uboot_countdown: "Hit any key".into(),
            uboot_prompt: "U-Boot>".into(),
            linux_boot: "Linux version".into(),
            linux_login: "login:".into(),
            linux_shell: "root@pi:#".into(),
            shutdown: None,
            fatal: vec![],
        },
        uboot: UbootProfile::default(),
        linux: LinuxProfile::default(),
        timing: TimingProfile::default(),
    }
}

#[test]
fn validates_and_lists_profiles_stably() {
    let registry = BoardRegistry::new(Config {
        boards: vec![
            board("b", "/dev/fake1", "p1"),
            board("a", "/dev/fake0", "p0"),
        ],
        serial_ring_bytes: 32,
    })
    .unwrap();
    let boards = registry.list();
    let ids: Vec<_> = boards
        .iter()
        .map(|board| board.profile.board_id.as_str())
        .collect();
    assert_eq!(ids, ["a", "b"]);
}

#[test]
fn rejects_duplicate_serial_devices() {
    let error = BoardRegistry::new(Config {
        boards: vec![
            board("a", "/dev/fake0", "p0"),
            board("b", "/dev/fake0", "p1"),
        ],
        serial_ring_bytes: 32,
    })
    .err()
    .unwrap();
    assert_eq!(error.code, ErrorCode::InvalidConfiguration);
}

#[test]
fn lease_reports_owner_and_releases() {
    let lease = BoardLease::default();
    let owner = LeaseOwner {
        operation_id: "op-1".into(),
        operation: "power_cycle".into(),
    };
    let guard = lease.acquire(owner.clone(), Duration::ZERO).unwrap();
    assert_eq!(lease.owner(), Some(owner));
    let error = lease
        .acquire(
            LeaseOwner {
                operation_id: "op-2".into(),
                operation: "run_linux_command".into(),
            },
            Duration::ZERO,
        )
        .err()
        .unwrap();
    assert_eq!(error.code, ErrorCode::BoardBusy);
    drop(guard);
    assert!(lease.owner().is_none());
}

#[test]
fn serial_ring_is_bounded_and_exposes_truncation() {
    let mut ring = SerialRing::new(5);
    ring.append(SystemTime::UNIX_EPOCH, b"abc");
    ring.append(SystemTime::UNIX_EPOCH, b"defg");
    let snapshot = ring.snapshot_from(0, 10);
    let bytes: Vec<_> = snapshot
        .chunks
        .iter()
        .flat_map(|chunk| chunk.bytes.clone())
        .collect();
    assert_eq!(bytes, b"cdefg");
    assert!(snapshot.truncated);
}

#[test]
fn state_evidence_is_bound_to_console_generation() {
    let profile = board("a", "/dev/fake0", "p0");
    let evidence = StateDetector::new(&profile.patterns).observe(
        b"noise\nU-Boot> ",
        SystemTime::UNIX_EPOCH,
        7,
    );
    assert_eq!(evidence.state, BoardState::UbootPrompt);
    assert_eq!(evidence.generation, 7);
}

#[test]
fn generation_snapshot_excludes_stale_shell_prompt() {
    let mut ring = SerialRing::new(1024);
    ring.append(SystemTime::UNIX_EPOCH, b"root@pi:# ");
    ring.mark_generation();
    ring.append(SystemTime::UNIX_EPOCH, b"Linux version 6.6\nlogin:");
    let snapshot = ring.snapshot_generation(1, 1024);
    let bytes: Vec<_> = snapshot
        .chunks
        .iter()
        .flat_map(|chunk| chunk.bytes.clone())
        .collect();
    assert_eq!(bytes, b"Linux version 6.6\nlogin:");
    assert!(snapshot.chunks.iter().all(|chunk| chunk.generation == 1));
}

#[test]
fn state_detector_uses_most_recent_matching_pattern() {
    let profile = board("a", "/dev/fake0", "p0");
    let evidence = StateDetector::new(&profile.patterns).observe(
        b"root@pi:# reboot\nLinux version 6.6\nlogin:",
        SystemTime::UNIX_EPOCH,
        1,
    );
    assert_eq!(evidence.state, BoardState::LinuxLogin);
    assert_eq!(evidence.evidence, b"login:");
}

#[test]
fn console_socket_handles_power_commands_and_in_console_escapes() {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let registry = BoardRegistry::new(Config {
        boards: vec![board("board-a", "/dev/fake-serial-a", "node=1;endpoint=1")],
        serial_ring_bytes: 1024,
    })
    .unwrap();

    let sock_path = format!("/tmp/rpictl-test-{}.sock", uuid::Uuid::new_v4());
    let sock = rpictl_mcp::console::start(registry, std::path::Path::new(&sock_path)).unwrap();

    // Test POWER subcommand over socket
    {
        let mut stream = UnixStream::connect(&sock_path).unwrap();
        stream.write_all(b"POWER status board-a\n").unwrap();
        let mut reader = BufReader::new(stream);
        let mut response = String::new();
        reader.read_line(&mut response).unwrap();
        assert!(
            response.starts_with("err power status failed:")
                || response.starts_with("ok  power state:")
        );
    }

    // Test CONSOLE mode and in-console command
    {
        let mut stream = UnixStream::connect(&sock_path).unwrap();
        // Send the handshake and first command together to exercise BufReader
        // prefetching and protocol framing.
        stream.write_all(b"CONSOLE board-a\n\0help\n").unwrap();

        let mut reader = BufReader::new(stream);
        let mut response = String::new();
        // Read until we see @help output
        for _ in 0..5 {
            let mut line = String::new();
            if reader.read_line(&mut line).is_ok() && !line.is_empty() {
                response.push_str(&line);
                if response.contains("poweron") {
                    break;
                }
            }
        }
        assert!(response.contains("poweron"));
        assert!(!response.contains("@poweron"));
    }

    drop(sock);
}
