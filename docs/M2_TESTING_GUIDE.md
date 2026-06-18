# M2 安全功能测试指南

## 快速开始

### 构建项目
```bash
cargo build --release
```

### 运行服务器（启用认证）
```bash
./target/release/ttyd-rs --auth --username admin --password secret123
```

### 运行服务器（无认证 - 仅用于开发）
```bash
./target/release/ttyd-rs
```

## M2 新增安全功能

### 1. Basic Authentication

启用认证后，客户端必须通过WebSocket发送认证消息：

```json
{
  "type": "auth",
  "data": {
    "method": "basic",
    "credentials": "YWRtaW46c2VjcmV0MTIz"
  }
}
```

`credentials` 是 `username:password` 的base64编码。

**认证成功响应**:
```json
{
  "type": "auth_ok",
  "data": {
    "session_id": "uuid-string",
    "readonly": false
  }
}
```

**认证失败响应**:
```json
{
  "type": "auth_fail",
  "data": {
    "reason": "Invalid credentials"
  }
}
```

### 2. Rate Limiting

- 默认限制：**10次认证尝试 / 60秒**
- 超限后会阻塞客户端2个时间窗口（120秒）
- 认证成功后自动重置计数器

**超限响应**:
```json
{
  "type": "auth_fail",
  "data": {
    "reason": "Rate limit exceeded. Try again in 120 seconds"
  }
}
```

### 3. 输入验证

#### 终端尺寸验证
```json
{
  "type": "resize",
  "data": {
    "cols": 80,    // 范围: 10-500
    "rows": 24     // 范围: 5-200
  }
}
```

**验证失败响应**:
```json
{
  "type": "error",
  "data": {
    "code": "INVALID_SIZE",
    "message": "Invalid terminal size: cols=5 (valid range: 10-500)",
    "fatal": false
  }
}
```

#### 输入Payload验证
```json
{
  "type": "input",
  "data": {
    "payload": "ls -la"  // 最大 16KB
  }
}
```

**验证失败响应**:
```json
{
  "type": "error",
  "data": {
    "code": "INVALID_INPUT",
    "message": "Invalid input: Input payload too large: 20000 bytes (max: 16384 bytes)",
    "fatal": false
  }
}
```

### 4. 审计日志

启用审计日志（通过配置文件或默认行为）：

**日志事件类型**:
- `connection_opened` - WebSocket连接建立
- `connection_closed` - 连接关闭
- `auth_success` - 认证成功
- `auth_failure` - 认证失败
- `session_started` - 终端会话开始
- `error_occurred` - 错误事件（如验证失败）

**日志格式** (JSON):
```json
{
  "timestamp": "2026-06-17T10:30:00.123Z",
  "event_type": "auth_success",
  "remote_addr": "127.0.0.1",
  "username": "admin",
  "session_id": "abc-123-def",
  "details": "Authentication attempt: success"
}
```

## 测试认证流程

### 使用 websocat 测试

1. 安装 websocat:
```bash
cargo install websocat
```

2. 连接到服务器（无认证）:
```bash
websocat ws://127.0.0.1:7681/ws
```

3. 连接到服务器（需要认证）:
```bash
# 启动服务器
./target/release/ttyd-rs --auth --username admin --password secret

# 连接并发送认证消息
websocat ws://127.0.0.1:7681/ws
# 发送以下JSON:
{"type":"auth","data":{"method":"basic","credentials":"YWRtaW46c2VjcmV0"}}
```

生成base64凭证：
```bash
echo -n "admin:secret" | base64
# 输出: YWRtaW46c2VjcmV0
```

## 测试Rate Limiting

使用脚本快速发送多次认证请求：

```bash
for i in {1..15}; do
  echo "Attempt $i"
  echo '{"type":"auth","data":{"method":"basic","credentials":"d3Jvbmc6cGFzcw=="}}' | websocat -n1 ws://127.0.0.1:7681/ws
  sleep 0.5
done
```

前10次会返回 `auth_fail`，第11次开始会返回rate limit错误。

## 测试输入验证

### 测试终端尺寸限制
```bash
# 发送无效的终端尺寸（太小）
{"type":"resize","data":{"cols":5,"rows":24}}

# 发送无效的终端尺寸（太大）
{"type":"resize","data":{"cols":600,"rows":24}}
```

### 测试Payload大小限制
```bash
# 生成20KB的输入（超过16KB限制）
{"type":"input","data":{"payload":"$(python3 -c 'print("x"*20000)')"}}
```

## 运行单元测试

```bash
# 运行所有测试
cargo test

# 运行特定模块的测试
cargo test auth::
cargo test audit::
cargo test validation::
cargo test rate_limit::

# 显示测试输出
cargo test -- --nocapture
```

## 代码质量检查

```bash
# 格式检查
cargo fmt -- --check

# Lint检查（零警告）
cargo clippy --all-targets -- -D warnings

# 完整测试套件
cargo test
```

## 安全最佳实践

1. **生产环境必须启用认证**
   ```bash
   ./ttyd-rs --auth --username YOUR_USER --password STRONG_PASSWORD
   ```

2. **使用强密码**
   - 至少12个字符
   - 混合大小写、数字和符号

3. **监控审计日志**
   - 定期检查 `auth_failure` 事件
   - 警惕异常的连接模式

4. **配置防火墙**
   - 仅允许信任的IP访问
   - 考虑使用反向代理（Nginx/Caddy）

5. **HTTPS/WSS**
   - 在生产环境使用TLS
   - 通过反向代理终止SSL

## 配置选项

```bash
./ttyd-rs --help
```

主要选项：
- `-p, --port <PORT>` - 监听端口（默认：7681）
- `-b, --bind <ADDR>` - 绑定地址（默认：127.0.0.1）
- `--auth` - 启用认证
- `--username <USER>` - Basic Auth用户名
- `--password <PASS>` - Basic Auth密码
- `--log-level <LEVEL>` - 日志级别（trace/debug/info/warn/error）

## 故障排除

### 认证一直失败
1. 检查凭证的base64编码是否正确
2. 确认用户名和密码匹配
3. 查看服务器日志获取详细错误

### Rate limit太严格
目前hardcoded为10次/60秒，未来版本将支持配置。

### WebSocket连接失败
1. 检查服务器是否运行
2. 确认端口和地址正确
3. 检查防火墙设置

## 下一步

M3将添加：
- 多客户端会话管理
- TOML配置文件
- 可配置的rate limiting参数
- Token认证
- IP白名单/黑名单

---

**完成报告**: 详细的M2实施报告请参阅 [docs/M2_COMPLETION_REPORT.md](M2_COMPLETION_REPORT.md)
