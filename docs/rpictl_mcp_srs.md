# rpictl-mcp software requirements specification

## 1. Purpose

`rpictl-mcp` is a general-purpose Model Context Protocol (MCP) server for
operating one or more Raspberry Pi boards connected to a host computer. It
provides structured power, serial-console, U-Boot, and Linux operations without
being tied to tvisor or any other workload.

This document defines externally visible requirements. It does not prescribe an
implementation language, MCP SDK, serial library, or power-controller vendor.

## 2. Goals

The server shall:

1. Control and query board power through a configurable power backend.
2. Capture bounded serial logs without requiring an interactive terminal.
3. Detect U-Boot countdowns and prompts, Linux boot progress, login prompts, and
   authenticated Linux shells.
4. Enter U-Boot and execute policy-authorized U-Boot commands.
5. Boot Linux, log in with configured credentials, and execute policy-authorized
   Linux commands.
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
- Guarded U-Boot command execution.
- Linux boot, serial-console login, and guarded command execution.
- Graceful Linux shutdown and forced power control.
- Per-board locking, operation timeouts, cancellation, and audit records.

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
- command-policy references;
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
- **Command policy**: configured allow, confirmation, and deny rules.
- **Graceful power-off**: request shutdown through Linux and then verify power.
- **Forced power-off**: turn off the external power controller immediately.

## 6. MCP protocol requirements

1. The server shall target the stable MCP `2026-07-28` specification or a
   documented later stable revision.
2. The initial transport shall be `stdio`.
3. Every tool shall publish complete input and output JSON Schemas.
4. Results shall contain structured content; human-readable text may be included
   as a concise summary.
5. Long operations should report progress or use MCP task support when available
   in the selected SDK.
6. Cancellation shall stop serial writes and waiting promptly. A power-cycle
   cleanup that must restore power shall be non-cancellable once power is off.

References:

- [MCP 2026-07-28 release](https://blog.modelcontextprotocol.io/posts/2026-07-28/)
- [MCP server tools specification](https://modelcontextprotocol.io/specification/2026-07-28/server/tools)

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

Every operation shall report:

- `operation_id`, `board_id`, and operation name;
- start and finish timestamps and elapsed time;
- success/failure and stable error code;
- initial, expected, and observed final state when applicable;
- bounded serial transcript with truncation metadata;
- actions performed, including recovery actions;
- policy decision and confirmation status when applicable.

Secrets shall never appear in results.

### 7.3 Resources

The server should expose read-only MCP resources:

- `rpictl://boards`
- `rpictl://boards/{board_id}/status`
- `rpictl://boards/{board_id}/serial/recent`
- `rpictl://boards/{board_id}/config` with secrets redacted
- `rpictl://operations/{operation_id}`

## 8. Tool requirements

### 8.1 Discovery and state

#### `list_boards`

Returns configured board IDs, display names, capabilities, availability, and
current lease status.

#### `get_board_state`

Inputs:

- `board_id`
- `observe_duration_ms`
- `active_probe`, default `false`

It shall combine power-backend state and fresh serial evidence. Passive
observation must not write to the console. Active probing shall be disclosed in
the result and shall use only profile-approved probe input.

#### `get_power_state`

Returns `on`, `off`, or `unknown`, verified power-device identity, and raw
backend metadata safe for disclosure.

### 8.2 Power

#### `power_on`

Inputs include `board_id` and optional `wait_for`: `none`, `uboot`,
`linux_login`, or `linux_shell`. The operation shall verify relay state and,
when requested, wait for fresh console evidence.

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

Inputs include `board_id`, `mode`, `reason`, and optional `wait_for`.
The server shall verify off, wait the configured minimum interval, restore power,
and verify on. Once power is off, restoration takes priority over cancellation
and ordinary command failure.

### 8.3 Serial

#### `capture_serial`

Inputs:

- `board_id`
- `duration_ms`
- `max_bytes`
- optional `stop_patterns`
- `from`: `now`, `recent`, or a console-generation cursor

The result shall include raw-safe encoded bytes or decoded text, timestamps,
matched pattern, cursor, and truncation information. Capture shall not write to
the console or require a board lease.

#### `wait_for_state`

Waits for one or more named states with a bounded timeout. It shall return the
evidence establishing the state rather than only a Boolean.

### 8.4 U-Boot

#### `enter_uboot`

Inputs include `board_id` and `restart`: `auto`, `always`, or `never`.
The tool shall:

1. Return immediately after synchronizing an existing U-Boot prompt.
2. Interrupt a detected autoboot countdown with configured input.
3. If restart is permitted and required, perform the configured reset or power
   cycle and interrupt the next fresh countdown.
4. Verify the prompt before reporting success.

#### `run_uboot_command`

Inputs:

- `board_id`
- one-line `command`
- `timeout_ms`
- optional `expected_patterns`
- `allow_reboot`, default `false`

The server shall reject control characters and embedded newlines, synchronize
the prompt, send any caller-provided one-line command,
verify command echo, capture output, and wait for either the prompt, an expected
terminal state, or timeout. Prompt-like text in command output shall not alone
prove completion.

### 8.5 Linux

#### `boot_linux`

Starts or resumes the profile-defined U-Boot boot path and waits for Linux boot,
login, or shell evidence. An optional `login` input may request automatic
serial login after the login prompt appears.

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
- optional `expected_patterns`
- optional policy-approved `stdin_entries`

The server shall verify or establish a Linux shell, send any caller-provided
one-line command, and append a collision-resistant completion marker that
captures the exit status. The result shall separate command output from echoed
input and the marker. On timeout it may send configured interrupt input, then
must resynchronize or mark the console state unknown.

#### `reboot_linux`

Runs the configured Linux reboot sequence, observes the old console generation
ending, and optionally waits for U-Boot or Linux in the new boot generation.

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
- Server restart shall reconcile power and serial state rather than trusting
  cached expected state.
- Completed operation records and serial buffers shall have configurable
  retention limits.

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
7. Destructive operations require explicit, operation-specific confirmation.

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

Errors shall include the failed phase, safe recovery guidance, observed state,
and bounded non-secret evidence. Truncation is metadata and need not make an
otherwise successful operation fail.

## 16. Audit requirements

Each mutating operation shall create an audit record containing:

- operation and board IDs;
- caller/session identity when available;
- timestamps and elapsed time;
- normalized command with secrets redacted;
- policy decision and confirmation;
- power actions and verified states;
- initial and final board states;
- result or error code;
- transcript reference and content hash.

Audit records shall be append-oriented and have configurable retention.

## 17. Non-functional requirements

- State queries should complete within 2 seconds when current evidence is
  available.
- No wait may be unbounded.
- Default serial transcript and ring-buffer sizes shall be bounded.
- A transient MCP client disconnect shall not prevent mandatory power
  restoration.
- Backend failures shall use bounded retries with backoff.
- Profiles and policies shall be schema-validated at startup.
- Unit tests shall use fake serial and power backends.
- Integration tests shall cover cancellation, stale prompt rejection, policy
  denial, login failure, timeout recovery, and power restoration.
- Hardware tests shall be opt-in and identify the target board explicitly.

## 18. Configuration requirements

Configuration shall be separated into:

1. board profiles;
2. serial and state-detection profiles;
3. power-backend definitions;
4. U-Boot and Linux command policies;
5. secret-provider references;
6. retention and audit settings.

Startup shall reject duplicate serial devices, duplicate power identities,
invalid patterns, unknown policy references, unavailable required backends, and
profiles that lack safe timeout limits.

## 19. Acceptance criteria

The initial release is acceptable when it can demonstrate:

1. Listing and independently locking at least two simulated board profiles.
2. Querying, turning on, turning off, and cycling a fake and one real power
   backend with identity verification.
3. Capturing bounded serial output without writing to the device.
4. Detecting U-Boot countdown and reaching a verified U-Boot prompt.
5. Running an allowed U-Boot command and returning its bounded output.
6. Denying a persistent or destructive U-Boot command by default.
7. Detecting Linux boot and login prompts from fresh serial evidence.
8. Logging in using a configured credential without disclosing it.
9. Running an allowed Linux command and returning its true exit status.
10. Requiring confirmation for a configured high-risk Linux command.
11. Rejecting stale U-Boot and Linux prompts after reset or power cycle.
12. Restoring power after cancellation or failure during a power cycle.
13. Keeping concurrent read-only capture usable while serial writes remain
    exclusive.
14. Producing a complete redacted audit record for every mutating operation.

## 20. Suggested delivery phases

1. Profiles, board registry, serial ownership, capture, and state detection.
2. Power abstraction plus verified on/off/cycle operations.
3. U-Boot synchronization and guarded command execution.
4. Linux boot detection, login, guarded command execution, reboot, and shutdown.
5. MCP resources, task/progress support, audit persistence, and real-hardware
   qualification.

## 21. Open design decisions

- Whether the first implementation should use Rust or Python.
- Which MCP SDK best supports the selected stable protocol and task model.
- Whether credentials should use OS keyrings, environment-backed secret files,
  or an external secret provider.
- Whether SSH should be added later as an alternative Linux transport.
- Whether command policy should use declarative templates only or permit signed
  extension modules.
- Which state-machine and transcript data should survive server restart.
