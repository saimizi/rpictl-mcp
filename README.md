# rpictl-mcp

`rpictl-mcp` is a Model Context Protocol (MCP) server for controlling and automating Raspberry Pi boards connected to a host machine. It provides safe, structured control over hardware power backends (e.g., Matter smart plugs via `chip-tool`), exclusive UART serial console capture, U-Boot bootloader interaction, and Linux operations.

It is designed to support **multiple AI agents (e.g. Codex, AGY, Claude Desktop, Cursor) running concurrently** through a shared daemon architecture without serial port conflicts.

---

## Key Features

- **Multi-Agent & Multi-Client Support**: Run multiple MCP servers from different AI agents simultaneously. A central daemon manages physical `/dev/ttyUSB*` ports and serial buffers, while agents connect via lightweight stdio MCP proxies over Unix domain socket IPC.
- **Hardware Power Control**: Integrated power control (e.g., Tapo P110M / Matter) with verified device identity, graceful shutdown, forced power-off, and safe power-cycle delays.
- **Bounded Serial Ring Buffer**: In-memory ring buffer keeps recent serial logs with byte cursors and generation tagging, allowing passive log capture without blocking.
- **Automated Boot Stages**:
  - **U-Boot**: Autoboot countdown interruption, prompt synchronization, and one-line U-Boot command execution.
  - **Linux**: Kernel boot detection, automatic serial login, command execution with completion/exit-code tracking, and safe reboots.
- **Board Leases**: Fine-grained per-board write leasing ensures that write operations are never interleaved, while non-intrusive operations (like reading logs) proceed concurrently.
- **Interactive CLI & Monitoring**: Built-in CLI for raw serial console interaction, streaming monitors, power management, and configuration diagnostics.

---

## Architecture

```
                 +-------------------+     +------------------+
                 |    AGY Agent      |     |   Codex Agent    |
                 | (rpictl-mcp serve)|     |(rpictl-mcp serve)|
                 +---------+---------+     +--------+---------+
                           | stdio                  | stdio
                           v                        v
                    [ MCP Client ]            [ MCP Client ]
                           |                        |
                           +-----------+------------+
                                       | Unix Domain Socket (/tmp/rpictl-mcp.sock)
                                       v
                     +------------------------------------+
                     |        rpictl-mcp Daemon           |
                     |  - BoardRegistry & Lease Manager   |
                     |  - Serial Ring Buffer Threads      |
                     +-----------------+------------------+
                                       |
                       +---------------+---------------+
                       |                               |
                       v                               v
             /dev/ttyUSB0 (UART)             chip-tool (Matter Power)
                       |                               |
                       +---------------+---------------+
                                       v
                            [ Raspberry Pi 4 / 5 ]
```

---

## Getting Started

### Prerequisites

- **Rust & Cargo**: Rust 1.75+ recommended.
- **Matter CLI (Optional for power control)**: `chip-tool` installed in PATH if using Matter smart plugs.

### Building

```bash
cargo build --release
```

The compiled binary is available at `target/release/rpictl-mcp`.

---

## Configuration

Create a JSON configuration file (e.g., `config/boards.json`):

```json
{
  "serial_ring_bytes": 262144,
  "boards": [
    {
      "board_id": "lab-rpi4",
      "display_name": "Lab Raspberry Pi 4",
      "serial": {
        "device": "/dev/ttyUSB0",
        "baud_rate": 115200,
        "data_bits": 8,
        "parity": "none",
        "stop_bits": 1,
        "flow_control": "none"
      },
      "power": {
        "backend": "matter",
        "identity": "node=1;endpoint=1;mac=60-15-6F-73-B3-A0"
      },
      "patterns": {
        "uboot_countdown": "Hit any key to stop autoboot",
        "uboot_prompt": "U-Boot>",
        "linux_boot": "Linux version",
        "linux_login": "login:",
        "linux_shell": "root@pi:#",
        "shutdown": "reboot: Power down"
      },
      "uboot": {
        "interrupt": " ",
        "boot_command": "boot"
      },
      "linux": {
        "account": "root",
        "credential_ref": "env:RPICTL_LAB_RPI4_ROOT_PASSWORD",
        "shutdown_command": "poweroff"
      },
      "timing": {
        "operation_timeout_ms": 30000,
        "power_cycle_off_ms": 5000,
        "serial_pacing_ms": 2
      }
    }
  ]
}
```

---

## Usage

### 1. Diagnose Environment

Verify your configuration, serial device availability, and daemon socket status:

```bash
rpictl-mcp doctor config/boards.json
```

### 2. Run the Daemon (Recommended for Multi-Agent Setup)

By default, `rpictl-mcp daemon` automatically detaches into the background and outputs its process ID and socket path:

```bash
# Start daemon in background
rpictl-mcp daemon config/boards.json

# Start daemon in background with a log file
rpictl-mcp daemon config/boards.json --log /var/log/rpictl.log

# Run in foreground (e.g., for systemd services or debugging)
rpictl-mcp daemon config/boards.json --foreground

# Check daemon status
rpictl-mcp daemon status

# Gracefully stop the running daemon
rpictl-mcp daemon stop
```

### 3. Run MCP Server (`serve`)

When an AI agent starts `rpictl-mcp serve`, it automatically connects to the running daemon if available. If no daemon is running, it starts in standalone primary mode.

```bash
# Standard MCP stdio server
rpictl-mcp serve config/boards.json

# With Gemini-compatible schemas (wraps top-level arrays in objects)
rpictl-mcp serve config/boards.json --gemini
```

### 4. Interactive CLI Tools

`rpictl-mcp` also provides handy interactive subcommands:

```bash
# Open interactive serial console session (with @poweron, @poweroff, @help escape commands)
rpictl-mcp console lab-rpi4

# Stream raw UART serial output
rpictl-mcp monitor lab-rpi4

# Query and control board power directly
rpictl-mcp power status lab-rpi4
rpictl-mcp power on lab-rpi4
rpictl-mcp power off lab-rpi4
rpictl-mcp power cycle lab-rpi4
```

---

## AI Agent / MCP Client Configuration

### Antigravity (AGY) / Gemini CLI

In `~/.gemini/antigravity-cli/mcp/rpictl-mcp.json` or config:

```json
{
  "command": "/path/to/rpictl-mcp",
  "args": ["serve", "/path/to/config/boards.json", "--gemini"]
}
```

### Claude Desktop / Cursor / Codex

In `claude_desktop_config.json` or `.codex/mcp.json`:

```json
{
  "mcpServers": {
    "rpictl": {
      "command": "/path/to/rpictl-mcp",
      "args": ["serve", "/path/to/config/boards.json"]
    }
  }
}
```

---

## MCP Tools Reference

| Tool | Description |
|---|---|
| `list_boards` | List configured boards and current lease availability. |
| `get_board_state` | Infer board state (`uboot_prompt`, `linux_login`, `linux_shell`, etc.) from serial evidence. |
| `get_power_state` | Query power state from the configured power backend. |
| `capture_serial` | Retrieve recent bounded serial logs from cursor without writing to UART. |
| `power_on` | Power on board and optionally wait for a specific boot state. |
| `power_on_uboot` | Power on board, interrupt fresh autoboot countdown, and reach U-Boot prompt. |
| `power_on_linux` | Power on board, detect Linux boot, and optionally log in to a shell. |
| `power_off` | Power off board gracefully or forcibly (`confirmation: true` required). |
| `power_cycle` | Safely power-cycle board with off-delay and automatic on-restoration. |
| `wait_for_state` | Block until one of the expected board states is observed. |
| `enter_uboot` | Synchronize to U-Boot prompt or interrupt countdown (`restart: auto\|always\|never`). |
| `run_uboot_command` | Execute a one-line U-Boot command and capture console output. |
| `boot_linux` | Trigger boot command from U-Boot and wait for Linux login/shell. |
| `linux_login` | Perform serial login using configured credentials. |
| `run_linux_command` | Execute a one-line Linux command and verify completion status via exit marker. |
| `reboot_linux` | Send reboot command and wait for next boot state (`confirmation: true` required). |

---

## License

Apache-2.0 / MIT
