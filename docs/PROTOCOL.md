# WebSocket 协议规范

## 概述

ttyd-rs 使用 WebSocket 在浏览器客户端和服务器端终端之间传输数据。协议设计目标：
- **高效**：最小化开销，支持高频终端输出
- **简单**：易于实现和调试
- **扩展性**：支持未来功能扩展

## 连接流程

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

## 消息格式

所有消息使用 JSON 格式传输（未来可考虑二进制协议优化）。

### 消息结构

```json
{
  "type": "MESSAGE_TYPE",
  "data": { /* type-specific payload */ }
}
```

## 消息类型

### 1. 客户端 -> 服务器

#### 1.1 AUTH - 身份验证
```json
{
  "type": "auth",
  "data": {
    "method": "basic",  // "basic" | "token"
    "credentials": "base64_encoded_credentials"
  }
}
```

#### 1.2 INPUT - 用户输入
```json
{
  "type": "input",
  "data": {
    "payload": "user typed text or control sequences"
  }
}
```

**注意**：payload 包含原始键盘输入，包括控制字符（如 Ctrl+C = \x03）

#### 1.3 RESIZE - 终端大小调整
```json
{
  "type": "resize",
  "data": {
    "cols": 80,
    "rows": 24
  }
}
```

#### 1.4 PING - 保活
```json
{
  "type": "ping",
  "data": {
    "timestamp": 1718640000000
  }
}
```

### 2. 服务器 -> 客户端

#### 2.1 AUTH_OK - 认证成功
```json
{
  "type": "auth_ok",
  "data": {
    "session_id": "uuid",
    "readonly": false
  }
}
```

#### 2.2 AUTH_FAIL - 认证失败
```json
{
  "type": "auth_fail",
  "data": {
    "reason": "Invalid credentials"
  }
}
```

#### 2.3 OUTPUT - 终端输出
```json
{
  "type": "output",
  "data": {
    "payload": "terminal output data including ANSI escape codes"
  }
}
```

**注意**：payload 包含原始终端输出，包括 ANSI 转义序列

#### 2.4 PONG - 保活响应
```json
{
  "type": "pong",
  "data": {
    "timestamp": 1718640000000
  }
}
```

#### 2.5 ERROR - 错误消息
```json
{
  "type": "error",
  "data": {
    "code": "RATE_LIMIT_EXCEEDED",
    "message": "Too many requests",
    "fatal": false  // true = 连接将被关闭
  }
}
```

#### 2.6 DISCONNECT - 断开连接
```json
{
  "type": "disconnect",
  "data": {
    "reason": "Terminal process exited",
    "code": 0  // 进程退出码
  }
}
```

#### 2.7 READY - 终端就绪
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

## 状态机

### 服务器端状态

```
[Connected] --AUTH--> [Authenticating] --AUTH_OK--> [Ready]
                             |
                             +--AUTH_FAIL--> [Disconnected]

[Ready] --INPUT/RESIZE--> [Ready]
[Ready] --OUTPUT--------> [Ready]
[Ready] --DISCONNECT----> [Disconnected]
```

### 客户端状态

```
[Connected] --SEND_AUTH--> [Authenticating] --AUTH_OK--> [Active]
                                  |
                                  +--AUTH_FAIL--> [Failed]

[Active] --INPUT/RESIZE--> [Active]
[Active] --OUTPUT--------> [Active]
[Active] --DISCONNECT----> [Closed]
```

## 错误码

| 错误码 | 描述 |
|--------|------|
| `AUTH_REQUIRED` | 需要身份验证 |
| `AUTH_FAILED` | 认证失败 |
| `RATE_LIMIT_EXCEEDED` | 请求过于频繁 |
| `MESSAGE_TOO_LARGE` | 消息体过大 |
| `INVALID_MESSAGE` | 消息格式错误 |
| `PTY_ERROR` | 终端错误 |
| `PERMISSION_DENIED` | 权限不足（只读模式） |

## 性能考虑

### 批量处理
- 服务器可以在单个 OUTPUT 消息中批量发送多个终端输出块
- 客户端应该能够处理大块的输出数据

### 流量控制
- 如果客户端处理速度慢，服务器应该缓冲输出
- 缓冲区满时，服务器可以降低发送频率或丢弃旧数据

### 心跳
- 建议每 30 秒发送一次 PING/PONG
- 超过 60 秒无响应则认为连接断开

## 未来扩展

### 二进制协议（v2）
为了更高的性能，可以考虑使用二进制协议：

```
[1 byte: message_type][4 bytes: payload_length][N bytes: payload]
```

消息类型编码：
- 0x01: INPUT
- 0x02: OUTPUT
- 0x03: RESIZE
- 0x04: AUTH
- ...

### 会话恢复
```json
{
  "type": "resume",
  "data": {
    "session_id": "uuid",
    "last_sequence": 12345
  }
}
```

### 文件传输（zmodem 支持）
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

## 安全考虑

1. **消息大小限制**：单条消息最大 1MB（防止 DoS）
2. **速率限制**：每秒最多 100 条 INPUT 消息（防止滥用）
3. **认证超时**：连接后 10 秒内必须完成认证
4. **输入验证**：所有输入必须经过验证
5. **XSS 防护**：前端必须正确处理 OUTPUT，防止注入攻击

## 兼容性

### 与原 ttyd 的兼容性
- 消息格式与原 ttyd 类似但不完全相同
- 如需兼容原 ttyd 客户端，需要实现协议适配层

---

*协议版本：v0.1*  
*更新日期：2026-06-17*
