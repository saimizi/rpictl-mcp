use std::path::Path;

use rpictl_mcp::client::{SocketClient, ping_daemon};
use rpictl_mcp::config::*;
use rpictl_mcp::mcp::{BoardRequest, BoardStateRequest, CaptureSerialRequest};
use rpictl_mcp::{BoardRegistry, console};

fn create_test_profile(id: &str, device: &str, power_id: &str) -> BoardProfile {
    BoardProfile {
        board_id: id.into(),
        display_name: Some(format!("Display {id}")),
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
fn test_ping_daemon() {
    let non_existent = format!("/tmp/rpictl-nonexistent-{}.sock", uuid::Uuid::new_v4());
    assert!(!ping_daemon(Path::new(&non_existent)));

    let registry = BoardRegistry::new(Config {
        boards: vec![create_test_profile("b1", "/dev/fake-serial-1", "p1")],
        serial_ring_bytes: 1024,
    })
    .unwrap();

    let sock_path = format!("/tmp/rpictl-test-{}.sock", uuid::Uuid::new_v4());
    let _sock = console::start(registry, Path::new(&sock_path)).unwrap();

    assert!(ping_daemon(Path::new(&sock_path)));
}

#[test]
fn test_multi_agent_concurrent_rpc() {
    let registry = BoardRegistry::new(Config {
        boards: vec![
            create_test_profile("pi4-a", "/dev/fake-serial-a", "node=1;endpoint=1"),
            create_test_profile("pi4-b", "/dev/fake-serial-b", "node=1;endpoint=2"),
        ],
        serial_ring_bytes: 4096,
    })
    .unwrap();

    let sock_path = format!("/tmp/rpictl-test-{}.sock", uuid::Uuid::new_v4());
    let _sock = console::start(registry, Path::new(&sock_path)).unwrap();

    // Agent 1 (e.g. AGY)
    let agent_agy = SocketClient::new(Path::new(&sock_path));
    // Agent 2 (e.g. Codex)
    let agent_codex = SocketClient::new(Path::new(&sock_path));
    // Agent 3 (e.g. Claude)
    let agent_claude = SocketClient::new(Path::new(&sock_path));

    // Agent 1 lists boards
    let boards = agent_agy.list_boards().unwrap();
    assert_eq!(boards.len(), 2);
    assert_eq!(boards[0].board_id, "pi4-a");
    assert_eq!(boards[1].board_id, "pi4-b");
    assert!(boards[0].available);

    // Agent 2 checks power state (returns error as expected for fake backend)
    let power_err = agent_codex
        .get_power_state(BoardRequest {
            board_id: "pi4-a".into(),
        })
        .unwrap_err();
    assert!(power_err.message.contains("POWER_BACKEND_FAILED"));

    // Agent 3 gets board state
    let state = agent_claude
        .get_board_state(BoardStateRequest {
            board_id: "pi4-b".into(),
            observe_duration_ms: Some(10),
            active_probe: Some(false),
        })
        .unwrap();
    assert_eq!(state.board_id, "pi4-b");

    // Agent 1 captures serial
    let capture = agent_agy
        .capture_serial(CaptureSerialRequest {
            board_id: "pi4-a".into(),
            max_bytes: Some(100),
            cursor: None,
        })
        .unwrap();
    assert_eq!(capture.board_id, "pi4-a");
    assert_eq!(capture.start_cursor, 0);

    // Concurrent requests from 10 threads simulating concurrent agents
    let mut handles = vec![];
    for i in 0..10 {
        let path = sock_path.clone();
        handles.push(std::thread::spawn(move || {
            let client = SocketClient::new(Path::new(&path));
            let boards = client.list_boards().unwrap();
            assert_eq!(boards.len(), 2);
            let state = client
                .get_board_state(BoardStateRequest {
                    board_id: if i % 2 == 0 { "pi4-a".into() } else { "pi4-b".into() },
                    observe_duration_ms: Some(1),
                    active_probe: Some(false),
                })
                .unwrap();
            assert!(state.board_id.starts_with("pi4-"));
        }));
    }

    for h in handles {
        h.join().unwrap();
    }
}
