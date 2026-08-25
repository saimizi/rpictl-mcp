# rpictl-mcp software requirements specification

## 1. Purpose

`rpictl-mcp` is a general-purpose Model Context Protocol (MCP) server for
operating one or more Raspberry Pi boards connected to a host computer. It
provides structured power, serial-console, U-Boot, and Linux operations without
being tied to tvisor or any other workload.

This document defines externally visible requirements. It does not prescribe an
implementation language, MCP SDK, serial library, or power-controller vendor.

### 1.1 Version status

This revision describes the implemented Rust v0.1 behavior. Statements marked
"deferred" describe post-v0.1 work rather than current server guarantees.

## 2. Goals

The server shall:

1. Control and query board power through a configurable power backend.
2. Capture bounded serial logs without requiring an interactive terminal.
3. Detect U-Boot countdowns and prompts, Linux boot progress, login prompts, and
   authenticated Linux shells.
4. Enter U-Boot and execute caller-provided one-line U-Boot commands.
5. Boot Linux, log in with configured credentials, and execute caller-provided
   one-line Linux commands.
6. Serialize access to each physical console and prevent interleaved commands.
7. Return structured, auditable results with useful timeout and failure details.
8. Support named board profiles so hardware-specific settings are configuration,
   not application logic.

## 3. Scope

### 3.1 In scope

- Named Raspberry Pi board profiles.
- Pluggable on/off/state-query power backends.
- Local UART serial devices with configurable baud rate and framing.
- Passive and bounded serial capture.
- Detection of U-Boot, Linux boot, Linux login, and Linux shell states.
- U-Boot countdown interruption and prompt synchronization.
- Unrestricted one-line U-Boot command execution over the board console.
- Linux boot, serial-console login, and unrestricted one-line command execution.
- Graceful Linux shutdown and forced power control.
- Per-board locking and bounded operation timeouts.

### 3.2 Out of scope for the initial release

- A graphical or fully interactive terminal emulator.
- SSH, IPMI, USB gadget, and network-console transports.
- Operating-system installation, SD-card imaging, or firmware flashing workflows.
- Smart-plug commissioning and vendor-account management.
- Arbitrary command execution on the host running `rpictl-mcp`.
- Workload-specific build, deployment, or test logic such as tvisor/TFTP flows.

Workload-specific MCP servers or automation may use `rpictl-mcp` as a lower-level
board-control service.

## 4. Deployment and board profiles

Every board-facing tool shall take a `board_id`. A board profile shall define:

- serial device, baud rate, data bits, parity, stop bits, and flow control;
- power backend and a verified device identity;
- U-Boot prompt, countdown patterns, interrupt bytes, and boot command;
- Linux boot, login, password, shell-prompt, and shutdown patterns;
- login account references and credential-provider references;
- optional legacy command allowlists, accepted but not enforced;
- timeouts, serial pacing, line endings, and power-cycle off interval.

The current development board can be represented by this example profile:

| Item | Example value |
|---|---|
| Board ID | `lab-rpi4` |
| Serial device | `/dev/ttyUSB0` |
| Serial settings | 115200 baud, 8-N-1 |
| U-Boot prompt | `U-Boot>` |
| Linux login prompt | `js-virt login:` |
| Linux account | `root` |
| Power device | Tapo P110M (JP) |
| Power device IP | `192.168.10.108` |
| Power device MAC | `60-15-6F-73-B3-A0` |
| Matter node/endpoint | node 1, endpoint 1 |
| Power-cycle off interval | 5 seconds |

These values are deployment data, not mandatory defaults.

## 5. Terminology

- **Observed state**: state inferred only from serial or power evidence.
- **Expected state**: state predicted after a successful operation.
- **Board lease**: exclusive ownership of operations that can write to a board.
- **Console generation**: serial evidence produced since the latest verified
  power-on, reset, or explicitly recorded generation boundary.
- **Active probe**: a disclosed carriage return sent to solicit the current
  console prompt when passive evidence is unavailable.
- **Graceful power-off**: request shutdown through Linux and then verify power.
- **Forced power-off**: turn off the external power controller immediately.

## 6. MCP protocol requirements

1. The v0.1 server uses `rmcp` 3.1 and negotiates the MCP protocol supported by
   that SDK (currently verified with `2025-06-18`).
2. The initial transport shall be `stdio`.
3. Every tool shall publish complete input and output JSON Schemas.
4. Results shall contain structured content; human-readable text may be included
   as a concise summary.
5. v0.1 operations are synchronous and bounded by explicit or configured
   timeouts. MCP task progress and cancellation are deferred.

References:

- [MCP 2026-07-28 release](https://blog.modelcontextprotocol.io/posts/2026-07-28/)
- [MCP server tools specification](https://modelcontextprotocol.io/specification/2026-07-28/server/tools)

### 6.1 Command-line interface

The v0.1 binary provides:

- `rpictl-mcp doctor <config.json>` to validate configuration and report basic
  serial-device and power-identity readiness;
- `rpictl-mcp serve <config.json> [monitor-socket]` to start the stdio MCP server
  and real-time monitor socket; and
- `rpictl-mcp monitor <board_id> [monitor-socket]` to stream that board's serial
  bytes without taking ownership of the UART.

The default monitor socket is `/tmp/rpictl-mcp.sock`.

## 7. Common data model

### 7.1 Board states

`get_board_state` and state-changing operations shall use:

- `powered_off`
- `powering_on`
- `boot_rom_or_firmware`
- `uboot_countdown`
- `uboot_prompt`
- `linux_booting`
- `linux_login`
- `linux_shell`
- `shutdown_in_progress`
- `application_running`
- `unresponsive`
- `unknown`

A state result shall include confidence, timestamp, evidence, console generation,
and whether active probing was used.

### 7.2 Common operation result

Every v0.1 operation result reports:

- `operation_id`, `board_id`, and operation name;
- start and finish timestamps and elapsed time;
- success/failure;
- expected and observed state when applicable;
- bounded command output when the operation completes successfully;
- actions performed, including recovery actions;
- a policy-decision label and requested actions when applicable.

Errors are returned as MCP tool errors containing a stable error code, phase,
and message. A timed-out command does not yet embed its partial transcript;
callers can retrieve retained bytes with `capture_serial`.

Secrets shall never appear in results.

### 7.3 Resources

The following read-only MCP resources are deferred beyond v0.1:

- `rpictl://boards`
- `rpictl://boards/{board_id}/status`
- `rpictl://boards/{board_id}/serial/recent`
- `rpictl://boards/{board_id}/config` with secrets redacted
- `rpictl://operations/{operation_id}`

## 8. Tool requirements

### 8.1 Discovery and state

#### `list_boards`

Returns configured board IDs, display names, serial device, power backend,
availability, and current lease owner when present.

#### `get_board_state`

Inputs:

- `board_id`
- `observe_duration_ms`
- `active_probe`, default `false`

Passive observation reads the current serial generation without writing.
`active_probe=true` records a cursor, sends one carriage return, observes fresh
serial bytes for the requested duration (default 500 ms, capped at 5 seconds),
and discloses the probe in the result. The caller queries power separately with
`get_power_state`.

#### `get_power_state`

Returns `on`, `off`, or `unknown`, verified power-device identity, and raw
backend metadata safe for disclosure.

### 8.2 Power

#### `power_on`

Inputs include `board_id` and optional `wait_for`, using a board-state name such
as `uboot_countdown`, `uboot_prompt`, `linux_login`, or `linux_shell`. The
operation verifies relay state. v0.1 callers that need atomic power-on and
U-Boot entry should immediately call `enter_uboot` after waiting for the fresh
countdown.

#### `power_on_uboot`

Atomically verifies that the board is off, powers it on, marks a fresh console
generation, waits for the U-Boot countdown, sends the configured interrupt, and
verifies `uboot_prompt`. Inputs are `board_id` and optional `timeout_ms`. If the
board is already on, callers use `enter_uboot` instead.

#### `power_on_linux`

Atomically verifies that the board is off, powers it on, marks a fresh console
generation, and waits for `linux_login` or an already-present `linux_shell`.
Inputs are `board_id`, optional `timeout_ms`, and optional `login`. With
`login=true`, the tool submits the configured account at `linux_login` and
verifies `linux_shell`. If the board is already on, callers use `boot_linux` or
`linux_login` as appropriate.

#### `power_off`

Inputs include:

- `board_id`
- `mode`: `graceful` or `force`
- `reason`
- explicit confirmation

Graceful mode shall require a Linux shell, run the configured shutdown sequence,
observe shutdown progress, and verify the final relay state. It shall not
silently fall back to forced power-off. Forced mode shall be visibly identified
as destructive.

#### `power_cycle`

Inputs include `board_id`, `mode`, `reason`, explicit confirmation, and optional
`wait_for`. v0.1 validates `mode` and accepts `wait_for`, but power cycling is a
verified forced off/on sequence and does not yet implement graceful shutdown or
the requested post-power-on wait. The server verifies off, waits the configured
minimum interval, restores power, and verifies on. Once power is off, a
best-effort restoration takes priority over ordinary command failure.

### 8.3 Serial

#### `capture_serial`

Inputs are `board_id`, optional `max_bytes` (default 65536), and an optional
cursor. The result includes decoded text, start and next cursors, console
generation, truncation, and invalid-UTF-8 metadata. Capture does not write to
the console or require a board lease. Timed capture and stop-pattern filtering
are deferred.

#### `wait_for_state`

Waits for one or more named states with a bounded timeout. v0.1 returns the
matched state with standard confidence and a current-generation evidence label;
exact matched bytes and generation reporting are future refinements.

### 8.4 U-Boot

#### `enter_uboot`

Inputs include `board_id` and `restart`: `auto`, `always`, or `never`.
The tool shall:

1. Return immediately after synchronizing an existing U-Boot prompt.
2. Interrupt a detected autoboot countdown with configured input.
3. With `restart=auto`, reboot from a verified Linux shell when required. With
   `restart=always`, reset from an existing U-Boot prompt. Both paths interrupt
   the next fresh countdown atomically.
4. Verify the prompt before reporting success.

#### `run_uboot_command`

Inputs:

- `board_id`
- one-line `command`
- `timeout_ms`
- compatibility fields `expected_patterns` and `allow_reboot` (not enforced in
  v0.1)

The server shall reject control characters and embedded newlines, synchronize
the prompt, send any caller-provided one-line command,
capture output, and wait for the next detected U-Boot prompt or timeout.

### 8.5 Linux

#### `boot_linux`

Sends the profile-defined U-Boot boot command and waits for Linux boot, login,
or shell evidence. The current board profile uses `reset`. The `login` field is
accepted, but automatic login within this operation is deferred; callers use
`linux_login` after a login prompt is observed.

#### `linux_login`

Selects a configured account by name and performs the profile-defined serial
login exchange. Credentials shall come from protected configuration or a secret
provider, never MCP arguments. Success requires a verified shell prompt or a
configured probe with a unique completion marker.

#### `run_linux_command`

Inputs:

- `board_id`
- one-line `command`
- `timeout_ms`
- compatibility fields `expected_patterns` and `allow_reboot` (not enforced in
  v0.1)

The caller shall first establish a Linux shell with `linux_login`. The server
sends any caller-provided one-line command and appends a collision-resistant
completion marker that captures the exit status. v0.1 returns the bounded raw
console transaction, including command echo and marker. Nonzero exit and timeout
are MCP errors; retained serial bytes remain available through `capture_serial`.

#### `reboot_linux`

Requires explicit confirmation, sends `reboot` from the Linux console, marks a
new console generation, and optionally waits for `uboot_countdown`,
`linux_booting`, or `linux_login`. `uboot_prompt` is intentionally not accepted
because this passive reboot operation does not interrupt autoboot. `linux_shell`
is not accepted because the operation does not perform login. Callers use
`enter_uboot(restart="auto")` to stop at U-Boot, or call `linux_login` after
waiting for `linux_login` to reach a shell.

## 9. Command transport policy

U-Boot and Linux command tools accept any caller-provided one-line command and
return the captured serial log. Embedded newlines, NUL bytes, and control
characters are rejected because they would escape the single-command transport
framing. Command allowlists in older profiles are accepted for compatibility
but are not enforced.

## 10. Serial subsystem requirements

1. One process shall own each configured serial device.
2. A single reader shall timestamp bytes and fan them out to capture, state
   detection, and the active command transaction.
3. Only one operation at a time may hold a board's write lease.
4. Serial writes shall use configurable pacing and line endings.
5. Raw bytes shall be retained in a bounded ring buffer; decoded output shall
   preserve invalid-byte information.
6. ANSI escape handling and line-ending normalization shall not alter raw logs.
7. If another process such as `screen` owns the device, the server shall fail
   clearly and shall not steal it.
8. Writes queued before a reset or power transition shall be discarded.

## 11. State detection

Profiles shall define patterns for:

- firmware or bootloader banners;
- U-Boot autoboot countdown and prompt;
- Linux kernel boot evidence;
- Linux login, password, and shell prompts;
- shutdown or reboot progress;
- configured application states and fatal-error signatures.

State detection shall:

1. prefer fresh evidence from the current console generation;
2. attach confidence and the exact bounded evidence used;
3. prevent stale prompts from satisfying a post-reset wait;
4. distinguish `unknown` from `unresponsive`;
5. use finite timeouts for every wait; and
6. disclose any active probe that changes the console.

## 12. Power subsystem requirements

1. A power backend shall provide query, on, and off operations.
2. Before control, it shall verify configured identity such as Matter node,
   endpoint, MAC address, or backend-native immutable ID.
3. Identity mismatch shall fail without toggling power.
4. Relay changes shall be verified by a subsequent state query.
5. Power-cycle off time shall meet the profile minimum.
6. Failure after verified power-off shall trigger a best-effort restore-on
   sequence and prominently report whether power was restored.
7. No operation except explicit `power_off` may normally finish with a board
   that began powered on left powered off.

## 13. Concurrency and lifecycle

- Operations on different boards may run concurrently.
- Console-writing and power-changing operations require an exclusive board
  lease.
- Read-only serial captures may coexist with an active operation.
- Lease acquisition has a bounded timeout and reports the owning operation.
- After restart, passive state begins as `unknown` until new serial evidence is
  received. Callers may request an active probe to solicit a prompt.
- Serial ring size is configurable. Persistent completed-operation retention is
  deferred.

## 14. Security

1. The server shall run with the least host privileges needed for serial and
   configured power backends.
2. It shall not expose a host-shell tool.
3. Serial devices, board IDs, power identities, and executable backend paths
   shall come from administrator-controlled configuration.
4. Passwords, tokens, fabric credentials, and private keys shall be stored
   outside ordinary profile files or protected with appropriate permissions.
5. Secrets shall be redacted from MCP results, logs, errors, and audit records.
6. MCP callers shall not supply arbitrary executable paths or power-device IDs.
7. Power-off, power-cycle, and Linux reboot tools require explicit confirmation.
   Arbitrary console command tools intentionally do not classify or restrict
   caller-provided commands in v0.1.

## 15. Error model

Stable error codes shall include at least:

- `BOARD_NOT_FOUND`
- `BOARD_BUSY`
- `SERIAL_OPEN_FAILED`
- `SERIAL_DISCONNECTED`
- `POWER_BACKEND_FAILED`
- `POWER_IDENTITY_MISMATCH`
- `POWER_STATE_UNVERIFIED`
- `STATE_TIMEOUT`
- `UBOOT_PROMPT_NOT_FOUND`
- `LINUX_LOGIN_FAILED`
- `SHELL_NOT_SYNCHRONIZED`
- `COMMAND_DENIED`
- `CONFIRMATION_REQUIRED`
- `COMMAND_TIMEOUT`
- `COMMAND_EXIT_NONZERO`
- `OUTPUT_TRUNCATED`
- `RECOVERY_FAILED`
- `OPERATION_CANCELLED`

v0.1 errors include the failed phase and message. Structured recovery guidance,
observed-state attachment, and embedded partial transcripts are deferred.
Truncation is metadata and does not make a successful capture fail.

## 16. Audit status

Operation results currently carry IDs, timestamps, actions, state, and command
output for the connected caller. Persistent append-oriented audit storage is
deferred. A future audit record should contain:

- operation and board IDs;
- caller/session identity when available;
- timestamps and elapsed time;
- normalized command with secrets redacted;
- policy decision and confirmation;
- power actions and verified states;
- initial and final board states;
- result or error code;
- transcript reference and content hash.

Future audit records should be append-oriented and have configurable retention.

## 17. Non-functional requirements

- State queries should complete within 2 seconds when current evidence is
  available.
- No wait may be unbounded.
- Default serial transcript and ring-buffer sizes shall be bounded.
- Power-cycle logic makes a best-effort restore-on attempt after power is
  switched off.
- Configuration is schema-validated at startup.
- Unit tests cover configuration, lease behavior, bounded serial buffering,
  state detection, and stale-generation rejection.
- Hardware tests shall be opt-in and identify the target board explicitly.

## 18. Configuration requirements

Configuration shall be separated into:

1. board profiles;
2. serial and state-detection profiles;
3. power-backend definitions;
4. U-Boot and Linux console settings, including legacy allowlist fields retained
   for configuration compatibility;
5. secret-provider references;
6. retention and audit settings.

Startup rejects duplicate serial devices, invalid serial settings, unsupported
power backends, and invalid or incomplete profiles. The `doctor` command reports
missing serial devices as warnings and confirms profile count, configured power
identity, and stdio transport.

## 19. v0.1 acceptance status

The implemented initial release has demonstrated:

1. Listing configured boards and exclusively leasing write operations per board.
2. Querying, turning on, gracefully or forcibly turning off, and cycling the
   configured real Matter power backend with identity verification.
3. Capturing bounded serial output without writing to the device.
4. Detecting U-Boot countdown and reaching a verified U-Boot prompt.
5. Running any caller-provided one-line U-Boot command and returning bounded
   serial output.
6. Rejecting multiline and control-character input that would break transport
   framing.
7. Detecting Linux boot and login prompts from fresh serial evidence.
8. Logging in using a configured credential without disclosing it.
9. Running any caller-provided one-line Linux command with a completion marker.
10. Requiring confirmation for explicit power and Linux-reboot tools.
11. Rejecting stale U-Boot and Linux prompts after a generation boundary.
12. Making a best-effort restoration after failure during a power cycle.
13. Keeping read-only capture usable while serial writes remain
    exclusive.
14. Detecting U-Boot, Linux login, and Linux shell states after server restart
    using the disclosed Enter-based active probe.

MCP resources, task/cancellation support, persistent audit storage, automatic
unknown-state recovery, and richer transcript separation remain post-v0.1 work.

## 20. Delivery status

1. Complete: profiles, board registry, serial ownership, capture, state
   detection, active probing, and real-time Unix-socket monitoring.
2. Complete: verified Matter on/off/cycle operations, atomic power-on-to-U-Boot
   and power-on-to-Linux flows, and graceful shutdown.
3. Complete: U-Boot synchronization, atomic reboot/reset-to-U-Boot, and
   unrestricted one-line command execution.
4. Complete: Linux boot detection, login, one-line command execution, reboot,
   and shutdown.
5. Deferred: MCP resources, task/progress support, cancellation, persistent
   audit records, richer command transcript results, and broader qualification.

## 21. Remaining design decisions

- Whether credentials should use OS keyrings, environment-backed secret files,
  or an external secret provider.
- Whether SSH should be added later as an alternative Linux transport.
- Which state-machine and transcript data should survive server restart.
- Whether command results should always be successful transcript envelopes,
  including on timeout and nonzero exit, instead of MCP tool errors.
- Whether to add a generalized state-aware reboot tool.
