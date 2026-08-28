# WebSocket Protocol Specification

## Overview

ttyd-rs uses WebSocket to transmit data between the browser client and the server-side terminal. The protocol design goals are:
- **Efficient**: Minimize overhead, support high-frequency terminal output
- **Simple**: Easy to implement and debug
- **Extensible**: Support for future feature extensions

## Connection Flow

```
Client                          Server
  |                               |
  |--- HTTP Upgrade Request ----->|
  |<-- 101 Switching Protocols ---|
  |                               |
  |--- AUTH (only if configured) ->|
  |<-- AUTH_OK / AUTH_FAIL -------|
  |                               |
  |--- RESIZE (cols, rows) ------>|   (RESIZE and optional JOIN may
  |--- JOIN (session_id)? ------->|    arrive in either order)
  |<-- READY (session_id, size) --|
  |                               |
  |<-- OUTPUT (terminal data) ----|
  |--- INPUT (user keystrokes) -->|
  |<-- OUTPUT (terminal data) ----|
  |              ...              |
  |<-- DISCONNECT ----------------|
```

`AUTH` is expected first only when the server has an `[auth]` section; with no
`[auth]` configured the server skips straight to the resize/join handshake.
If no `RESIZE` arrives, the session starts at the 80x24 default.

## Message Format

All messages are transmitted in JSON format (binary protocol optimization may be considered in the future).

### Message Structure

```json
{
  "type": "MESSAGE_TYPE",
  "data": { /* type-specific payload */ }
}
```

## Message Types

### 1. Client -> Server

#### 1.1 AUTH - Authentication
```json
{
  "type": "auth",
  "data": {
    "method": "basic",  // "basic" | "token"
    "credentials": "base64_encoded_credentials"
  }
}
```

`method` must name the method the server was configured with, otherwise
authentication fails. `credentials` is method-dependent:

| `method`  | `credentials`                            |
|-----------|------------------------------------------|
| `basic`   | base64 of `username:password`            |
| `token`   | the raw token string (not base64-wrapped) |

This message is only consumed during the auth phase; a mismatched or missing
first message ends the connection with `auth_fail`.

#### 1.2 INPUT - User Input
```json
{
  "type": "input",
  "data": {
    "payload": "user typed text or control sequences"
  }
}
```

**Note**: The payload contains raw keyboard input, including control characters (e.g., Ctrl+C = `\x03`). It is written to the PTY verbatim — no quoting or escaping is applied. Payloads longer than `[validation] max_input_size`, or containing NUL bytes, are rejected with `INVALID_INPUT`.

#### 1.3 RESIZE - Terminal Resize
```json
{
  "type": "resize",
  "data": {
    "cols": 80,
    "rows": 24
  }
}
```

**Note**: sizes outside `[validation]` (`min_cols`/`max_cols`, `min_rows`/`max_rows`)
are rejected with `INVALID_SIZE`; mid-session, the previous size stays in
effect. A valid resize is applied to the PTY and affects every client of the
session.

#### 1.4 PING - Keepalive
```json
{
  "type": "ping",
  "data": {
    "timestamp": 1718640000000
  }
}
```

#### 1.5 JOIN - Join Existing Session
```json
{
  "type": "join",
  "data": {
    "session_id": "uuid"
  }
}
```

**Note**: honored only during the handshake, and only for shared sessions.
Joining an `isolated` session is rejected with `CANNOT_JOIN`; an id that no
longer exists is rejected with `SESSION_NOT_FOUND` rather than silently
spawning a new PTY. Omitting `join` creates a fresh session.

#### 1.6 FILE_LIST - Request Directory Listing
```json
{
  "type": "file_list",
  "data": {
    "path": ".",
    "show_hidden": false
  }
}
```

**Note**: both fields are optional and default as shown. `path` is relative to
the session's current working directory; the server resolves it against the
session bound to this WebSocket connection, so clients cannot browse outside
their own session. Rejected with `FILE_TRANSFER_DISABLED` when file transfer is
off, and with `RATE_LIMITED` when the file-operation limit is hit. The same
listing is available over HTTP as `GET /api/files/list`.

### 2. Server -> Client

#### 2.1 AUTH_OK - Authentication Success
```json
{
  "type": "auth_ok",
  "data": {
    "client_id": "uuid",
    "readonly": false
  }
}
```

**Note**: the server always sends `false` here. The authoritative read-only
flag for the session arrives in `ready`; clients must use that one.

#### 2.2 AUTH_FAIL - Authentication Failure
```json
{
  "type": "auth_fail",
  "data": {
    "reason": "Invalid credentials"
  }
}
```

The connection is closed immediately after this message. `reason` is one of:

| `reason` | Cause |
|----------|-------|
| `Server authentication misconfigured` | `[auth]` present but its validator could not be built (fails closed) |
| `Rate limit exceeded. Try again in N seconds` | Too many connection attempts from this client |
| `Expected auth message` | First frame after upgrade was not `auth` |
| `Invalid authentication method: <detail>` | `method` was neither `basic` nor `token`, or did not match the server |
| `Invalid credentials format` | `credentials` failed length/charset validation |
| `Invalid credentials` | Basic auth rejected (bad username or password) |
| `Invalid token` | Token auth rejected |

#### 2.3 OUTPUT - Terminal Output
```json
{
  "type": "output",
  "data": {
    "payload": "terminal output data including ANSI escape codes"
  }
}
```

**Note**: The payload contains raw terminal output, including ANSI escape sequences

#### 2.4 PONG - Keepalive Response
```json
{
  "type": "pong",
  "data": {
    "timestamp": 1718640000000
  }
}
```

#### 2.5 ERROR - Error Message
```json
{
  "type": "error",
  "data": {
    "code": "RATE_LIMITED",
    "message": "File operation rate limit exceeded",
    "fatal": false  // true = connection will be closed
  }
}
```

See [Error Codes](#error-codes) for the full list. Non-fatal errors leave the
connection open, so clients should surface them without tearing down the
terminal.

#### 2.6 DISCONNECT - Disconnect
```json
{
  "type": "disconnect",
  "data": {
    "reason": "Session ended",
    "code": 0
  }
}
```

`code` is currently always `0`; treat `reason` as the descriptive field.
Observed reasons: `Session ended` (the client's session was released) and
`Shell exited` (the PTY child was already gone). The connection closes after it.

#### 2.7 READY - Terminal Ready
```json
{
  "type": "ready",
  "data": {
    "session_id": "uuid",
    "cols": 80,
    "rows": 24,
    "readonly": false
  }
}
```

**Note**: `cols`/`rows` are the size the PTY was actually created or resized to.
`session_id` is the value a client replays in `join` to reattach (and the same
value the `/api/files/*` endpoints take as their `session_id` query parameter).
`readonly` is the authoritative write permission for this client — when it is
`true`, the server rejects `input` with a `READONLY` error.

#### 2.8 FILE_LIST_RESULT - Directory Listing Response
```json
{
  "type": "file_list_result",
  "data": {
    "path": "resolved/absolute/path",
    "entries": [
      {
        "name": "file.txt",
        "size": 1024,
        "is_dir": false,
        "modified": "2026-07-30T10:00:00Z"
      },
      {
        "name": "subdir",
        "size": 4096,
        "is_dir": true,
        "modified": "2026-07-29T08:30:00Z"
      }
    ]
  }
}
```

**Note**: Hidden files (starting with `.`) are excluded unless `show_hidden: true` was requested.

## State Machine

### Server-side State

```
[Connected] --AUTH--> [Authenticating] --AUTH_OK--> [Handshake] --READY--> [Active]
                             |                          |
                             +--AUTH_FAIL---------------+--fatal error / heartbeat timeout--> [Closed]

[Active] --INPUT / RESIZE / FILE_LIST / ping--> [Active]
[Active] --OUTPUT / PONG / FILE_LIST_RESULT--> [Active]
```

### Client-side State

```
[Connected] --SEND_AUTH--> [Authenticating] --AUTH_OK--> [Handshake]
                                  |                          |
                                  +--AUTH_FAIL--> [Failed]   +--send RESIZE (+ optional JOIN)
                                                                 |
[Active] <--READY-------------------------------------------------+
[Active] --INPUT/RESIZE--> [Active]
[Active] --OUTPUT--------> [Active]
[Active] --DISCONNECT----> [Closed]
```

Clients must not send `input` before `ready` arrives; messages sent during the
handshake phase are ignored.

## Error Codes

The `code` values below are the only ones the server emits in `error` messages.
A `fatal` error closes the connection; non-fatal ones leave the terminal usable.

| Error Code | Fatal | When it is sent |
|------------|-------|-----------------|
| `INVALID_SIZE` | yes in handshake, no mid-session | `cols`/`rows` outside the configured `[validation]` bounds — fatal while establishing the session, non-fatal for a later `resize` (the previous size stays in effect) |
| `INVALID_INPUT` | no | `input` payload exceeded `[validation] max_input_size` or contained a NUL byte |
| `READONLY` | no | `input` received from a client whose session is read-only |
| `RATE_LIMITED` | no | `file_list` exceeded the dedicated file-operation rate limit (the HTTP file endpoints answer with 429 instead) |
| `FILE_TRANSFER_DISABLED` | no | `file_list` while `[file_transfer] enabled = false` |
| `FILE_LIST_ERROR` | no | Directory listing failed (missing path, permission denied, traversal rejected) |
| `OUTPUT_LAGGED` | no | This client's broadcast buffer overflowed, so some terminal output was dropped |
| `HEARTBEAT_TIMEOUT` | yes | No pong received within the heartbeat timeout |
| `CANNOT_JOIN` | yes | `join` targeted an existing session that is `isolated`, so it accepts no additional clients |
| `SESSION_NOT_FOUND` | yes | `join` named a session id that no longer exists |

## Performance Considerations

### Batch Processing
- PTY output is read in 16 KB chunks and coalesced into a single `output`
  message of up to 256 KB, which keeps syscall and frame overhead down during
  high-volume output (`cat` of a large file, a busy TUI redraw)
- Clients must therefore handle `output` messages arriving in large, irregular
  chunks and must not assume a one-to-one mapping between messages and PTY reads

### Flow Control
- Each client consumes the session's bounded broadcast channel (capacity 1024
  messages), so a slow client never blocks the PTY reader or the other clients
- When a client's buffer overflows it receives a non-fatal `OUTPUT_LAGGED`
  error and the missed output is dropped — there is no backpressure to the PTY
  and no replay

### Heartbeat
- The server sends protocol-level WebSocket ping frames every 30 seconds and
  closes the connection if no pong arrives within 90 seconds (three missed
  intervals), reporting `HEARTBEAT_TIMEOUT`
- The JSON `ping`/`pong` pair is a separate, client-driven keepalive: the server
  echoes the client's `timestamp` back unchanged so the client can measure
  round-trip time

## Future Extensions

### Binary Protocol (v2)
For higher performance, a binary protocol can be considered:

```
[1 byte: message_type][4 bytes: payload_length][N bytes: payload]
```

Message type encoding:
- 0x01: INPUT
- 0x02: OUTPUT
- 0x03: RESIZE
- 0x04: AUTH
- ...

### Session Resumption
```json
{
  "type": "resume",
  "data": {
    "session_id": "uuid",
    "last_sequence": 12345
  }
}
```

### File Transfer (zmodem support)

> **Note**: Basic file transfer is already implemented via HTTP endpoints (`/api/files/upload`, `/api/files/download`) and WebSocket file listing. Zmodem support remains a future enhancement for in-band terminal file transfer.

```json
{
  "type": "file_transfer",
  "data": {
    "protocol": "zmodem",
    "direction": "upload",
    "filename": "file.txt"
  }
}
```

## Security Considerations

1. **Frame size limit**: an inbound WebSocket message may not exceed 64 KB; this cap is fixed in the server
2. **Payload limit**: `max_input_size` (16 KB by default) bounds only the `input` payload inside that frame
3. **Rate limiting**: 10 requests per 60 seconds per client by default (`max_requests` / `window_seconds`), with a separate bucket for file operations so browsing directories cannot exhaust the authentication budget
4. **Connection limit**: once `max_connections` is reached, further upgrades are refused with HTTP 503 rather than a WebSocket message
5. **Input validation**: terminal size, payload size, and credential format are all validated
6. **XSS protection**: the terminal is the output surface — clients must render `output` through the emulator and never inject it as markup

## Compatibility

### Compatibility with Original ttyd
- Message format is similar but not identical to the original ttyd
- A protocol adaptation layer is needed for compatibility with original ttyd clients

---

*Protocol version: v1.2.0*
