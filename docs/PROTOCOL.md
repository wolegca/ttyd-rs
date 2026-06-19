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
  |--- AUTH (if required) ------->|
  |<-- AUTH_OK / AUTH_FAIL -------|
  |                               |
  |--- RESIZE (cols, rows) ------>|
  |<-- OUTPUT (terminal data) ----|
  |--- INPUT (user keystrokes) -->|
  |<-- OUTPUT (terminal data) ----|
  |              ...              |
  |<-- DISCONNECT ----------------|
```

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

#### 1.2 INPUT - User Input
```json
{
  "type": "input",
  "data": {
    "payload": "user typed text or control sequences"
  }
}
```

**Note**: The payload contains raw keyboard input, including control characters (e.g., Ctrl+C = \x03)

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

#### 2.2 AUTH_FAIL - Authentication Failure
```json
{
  "type": "auth_fail",
  "data": {
    "reason": "Invalid credentials"
  }
}
```

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
    "code": "RATE_LIMIT_EXCEEDED",
    "message": "Too many requests",
    "fatal": false  // true = connection will be closed
  }
}
```

#### 2.6 DISCONNECT - Disconnect
```json
{
  "type": "disconnect",
  "data": {
    "reason": "Terminal process exited",
    "code": 0  // process exit code
  }
}
```

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

## State Machine

### Server-side State

```
[Connected] --AUTH--> [Authenticating] --AUTH_OK--> [Ready]
                             |
                             +--AUTH_FAIL--> [Disconnected]

[Ready] --INPUT/RESIZE--> [Ready]
[Ready] --OUTPUT--------> [Ready]
[Ready] --DISCONNECT----> [Disconnected]
```

### Client-side State

```
[Connected] --SEND_AUTH--> [Authenticating] --AUTH_OK--> [Active]
                                  |
                                  +--AUTH_FAIL--> [Failed]

[Active] --INPUT/RESIZE--> [Active]
[Active] --OUTPUT--------> [Active]
[Active] --DISCONNECT----> [Closed]
```

## Error Codes

| Error Code | Description |
|------------|-------------|
| `AUTH_REQUIRED` | Authentication required |
| `AUTH_FAILED` | Authentication failed |
| `RATE_LIMIT_EXCEEDED` | Too many requests |
| `MESSAGE_TOO_LARGE` | Message body too large |
| `INVALID_MESSAGE` | Invalid message format |
| `PTY_ERROR` | Terminal error |
| `PERMISSION_DENIED` | Permission denied (read-only mode) |

## Performance Considerations

### Batch Processing
- The server can batch multiple terminal output blocks into a single OUTPUT message
- Clients should be able to handle large chunks of output data

### Flow Control
- If the client is processing slowly, the server should buffer output
- When the buffer is full, the server can reduce sending frequency or discard old data

### Heartbeat
- Recommended to send PING/PONG every 30 seconds
- Connection is considered broken if no response for 60 seconds

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

1. **Message size limit**: Single message max 16KB (default, configurable via `max_input_size`)
2. **Rate limiting**: Default 10 requests/60 seconds (configurable via `max_requests` / `window_seconds`)
3. **Input validation**: Terminal size, payload size, and credential format are all validated
4. **XSS protection**: Frontend must properly handle OUTPUT to prevent injection attacks

## Compatibility

### Compatibility with Original ttyd
- Message format is similar but not identical to the original ttyd
- A protocol adaptation layer is needed for compatibility with original ttyd clients

---

*Protocol version: v0.1*
*Last updated: 2026-06-19*
