# ttyd-rs 开发目标

## 项目概述

ttyd-rs 是 [ttyd](https://github.com/tsl0922/ttyd) 的 Rust 重写版本，旨在提供一个更安全、更高性能的 Web 终端共享解决方案。

**平台支持**：Linux 和 macOS（Unix-like 系统）

### 核心价值主张
- **内存安全**：利用 Rust 的所有权系统杜绝内存泄漏和悬垂指针
- **并发安全**：通过 Rust 的类型系统保证线程安全
- **更强的安全控制**：增强身份验证、授权和审计功能
- **现代化架构**：采用异步 I/O 和零拷贝技术提升性能

---

## 阶段一：核心功能实现（MVP）

### 1.1 基础架构
- [ ] HTTP/WebSocket 服务器框架
  - 选择异步运行时：tokio
  - WebSocket 实现：axum + axum-tungstenite
  - HTTP 静态文件服务
  
- [ ] PTY（伪终端）管理
  - 使用 `nix` crate（Unix 特定，功能完整）
  - 终端进程生命周期管理
  - 信号处理（SIGWINCH, SIGCHLD）
  - 优雅的进程清理机制

- [ ] 前端集成
  - 集成 xterm.js
  - WebSocket 消息协议定义
  - 终端大小调整（resize）支持

### 1.2 基本功能
- [ ] 命令行参数解析（使用 clap）
  - 端口配置
  - 绑定地址
  - Shell 命令配置
  - 日志级别设置

- [ ] 终端 I/O 双向通信
  - stdin/stdout 桥接
  - 二进制数据传输
  - 流量控制

- [ ] 多客户端连接支持
  - 会话隔离（每个连接独立终端）
  - 或会话共享（多个连接同一终端）

---

## 阶段二：安全性增强

### 2.1 身份验证
- [x] 基本认证（Basic Auth）
- [ ] Token 认证

### 2.2 授权与访问控制
- [ ] 用户角色系统（只读/读写）
- [ ] IP 白名单/黑名单
- [x] Rate limiting（防止暴力破解）
- [ ] CORS 配置

### 2.3 审计与日志
- [x] 连接日志（谁在什么时间连接）
- [x] 命令审计（可选记录所有命令）
- [ ] 会话录制（typescript 格式或自定义格式）
- [x] 安全事件告警

### 2.4 输入验证与防护
- [x] 命令注入防护
- [ ] 路径遍历防护
- [ ] XSS 防护（CSP 头）
- [ ] 最大连接数限制
- [x] WebSocket 消息大小限制

---

## 阶段三：功能增强

### 3.1 会话管理
- [ ] 会话持久化（断线重连）
- [ ] 会话列表 API
- [ ] 会话超时配置
- [ ] 会话分享（生成临时 URL）

### 3.2 终端功能
- [ ] 多种 Shell 支持（bash/zsh/fish）
- [ ] 环境变量配置
- [ ] 工作目录设置
- [ ] 终端主题配置
- [ ] UTF-8 完整支持

### 3.3 监控与诊断
- [ ] Prometheus metrics
- [ ] 健康检查端点
- [ ] 实时连接统计
- [ ] 性能指标收集

---

## 阶段四：高级特性（可选）

### 4.1 新特性探索
- [ ] 多终端标签页支持
- [ ] 终端历史搜索
- [ ] 文件上传/下载（通过 zmodem 或自定义协议）
- [ ] 终端协作（多人实时查看）
- [ ] 容器集成（直接连接 Docker/K8s 容器）

### 4.2 集成与扩展
- [ ] 反向代理支持（Nginx/Caddy）
- [ ] 与 OAuth2/OIDC 集成
- [ ] Webhook 通知
- [ ] 插件系统（基于动态库或 WASM）

### 4.3 Unix 特性深度支持
- [ ] systemd 集成（socket activation）
- [ ] 完整的信号处理
- [ ] Unix domain socket 支持
- [ ] 权限降级（以非特权用户运行）

---

## 技术栈选型

### 核心依赖
```toml
[dependencies]
# 异步运行时
tokio = { version = "1", features = ["full"] }

# Web 框架
axum = { version = "0.7", features = ["ws"] }
tower = "0.4"
tower-http = { version = "0.5", features = ["fs", "cors", "trace"] }

# WebSocket
axum-tungstenite = "0.1"

# PTY（Unix 特定）
nix = { version = "0.29", features = ["pty", "process", "signal"] }

# 命令行
clap = { version = "4", features = ["derive", "env"] }

# 日志
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }

# 错误处理
anyhow = "1"
thiserror = "1"

# 序列化
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"  # 配置文件

# 认证
base64 = "0.22"
sha2 = "0.10"
rand = "0.8"

# 其他
bytes = "1"
futures = "0.3"
libc = "0.2"
```

### 开发工具
- 测试：`cargo test`, `cargo-nextest`
- 基准测试：`criterion`
- 模糊测试：`cargo-fuzz`
- 安全审计：`cargo-audit`, `cargo-deny`
- 代码覆盖率：`cargo-tarpaulin`
- 内存检查：`valgrind`, `heaptrack`

---

## 代码质量标准

### Lint 规则（已配置）
```toml
[workspace.lints.clippy]
unwrap-used = "deny"      # 禁止 unwrap，强制错误处理
expect-used = "deny"      # 禁止 expect
panic = "deny"            # 禁止 panic
```

### 额外建议规则
```toml
# 推荐添加到 Cargo.toml
[workspace.lints.clippy]
unwrap-used = "deny"
expect-used = "deny"
panic = "deny"
needless-pass-by-value = "warn"
missing-docs = "warn"
unsafe-code = "warn"      # 审查所有 unsafe 代码（PTY 操作需要）
```

### 测试覆盖率目标
- 核心模块：>= 80%
- 安全相关代码：>= 90%
- 集成测试：覆盖所有主要用例

---

## 性能目标

- **启动时间**：< 50ms
- **内存占用**：< 10MB（空闲）
- **延迟**：< 5ms（本地网络）
- **并发连接**：> 1000（单实例）
- **吞吐量**：> 100MB/s（终端输出）

---

## 安全目标

1. **CVE-free**：无已知安全漏洞
2. **安全默认值**：默认配置即安全（默认需要认证）
3. **最小权限**：不需要 root 运行
4. **依赖安全**：定期审计依赖项（cargo-audit）
5. **安全文档**：提供安全配置最佳实践
6. **内存安全**：零 unsafe 或充分审查的 unsafe

---

## 兼容性目标

### 与原 ttyd 的兼容性
- [ ] 命令行参数兼容（尽可能）
- [ ] WebSocket 协议兼容（可连接原 ttyd 前端）
- [ ] 前端可复用（xterm.js）

### 破坏性改进（可接受）
- 更严格的默认安全配置
- 配置文件格式（TOML 代替大量命令行参数）
- 增强的 API（更多元数据和控制能力）

---

## 项目结构规划

```
ttyd-rs/
├── src/
│   ├── main.rs           # 入口点
│   ├── config.rs         # 配置解析
│   ├── server/           # HTTP/WebSocket 服务器
│   │   ├── mod.rs
│   │   ├── http.rs
│   │   └── websocket.rs
│   ├── pty/              # PTY 管理
│   │   ├── mod.rs
│   │   ├── process.rs
│   │   └── session.rs
│   ├── auth/             # 身份验证
│   │   ├── mod.rs
│   │   ├── basic.rs
│   │   └── token.rs
│   ├── protocol/         # WebSocket 协议
│   │   ├── mod.rs
│   │   └── message.rs
│   └── audit/            # 审计日志
│       ├── mod.rs
│       └── recorder.rs
├── static/               # 前端资源
│   ├── index.html
│   ├── xterm.js
│   └── app.js
├── tests/                # 集成测试
├── benches/              # 基准测试
└── docs/                 # 文档
```

---

## 文档规划

- [ ] README：快速开始
- [ ] INSTALL.md：安装文档
- [ ] CONFIG.md：配置参考
- [ ] SECURITY.md：安全最佳实践
- [ ] ARCHITECTURE.md：架构设计文档
- [ ] API.md：WebSocket API 规范
- [ ] CONTRIBUTING.md：贡献指南

---

## 里程碑

### M1：基础可用（2-3周）✅ 已完成
- [x] HTTP + WebSocket 服务器
- [x] PTY 基本功能（使用 nix）
- [x] 单客户端终端交互
- [x] 基本的前端集成

### M2：安全加固（2-3周）✅ 已完成 (2026-06-17)
- [x] 身份验证（Basic Auth）
- [x] 基本审计日志
- [x] 输入验证
- [x] Rate limiting（额外完成）

详细信息请参阅：[docs/M2_COMPLETION_REPORT.md](docs/M2_COMPLETION_REPORT.md)

### M3：功能完善（3-4周）✅ 已完成 (2026-06-17)
- [x] 多客户端支持（架构完成）
- [x] 会话管理（SessionManager）
- [x] 配置系统（TOML）
- [x] 完整的命令行接口
- [x] REST API端点（额外完成）
- [x] 自动会话清理（额外完成）

详细信息请参阅：[docs/M3_COMPLETION_REPORT.md](docs/M3_COMPLETION_REPORT.md)

### M4：生产就绪（持续）✅ 已完成 (2026-06-17)
- [x] WebSocket集成SessionManager（核心完成）
- [x] 前端会话功能（基本完成）
- [x] 文档完善
- [x] 代码质量保证
- [x] README更新

详细信息请参阅：[docs/M4_COMPLETION_REPORT.md](docs/M4_COMPLETION_REPORT.md)

**项目状态**: 生产就绪（单用户场景）

### 未来增强（可选）
- [ ] 完整的共享会话支持（多客户端同一PTY）
- [ ] 前端会话列表UI
- [ ] 集成测试套件
- [ ] 性能优化（零拷贝）
- [ ] Docker支持
- [ ] 安全审计

---

## 风险与挑战

1. **PTY 信号处理**：正确处理 SIGWINCH/SIGCHLD 等信号
2. **性能优化**：高频终端输出的零拷贝传输
3. **WebSocket 协议设计**：需要高效的二进制协议
4. **安全审计**：需要专业安全审查
5. **并发管理**：多会话下的资源管理和隔离

---

## 下一步行动

1. ✅ 创建开发目标文档
2. ✅ 设计 WebSocket 消息协议规范
3. ✅ 更新 Cargo.toml 添加核心依赖
4. ✅ 实现基础项目结构
5. ✅ 实现 HTTP 服务器骨架（axum）
6. ✅ 实现 PTY 基本功能（nix）
7. ✅ 前端集成 PoC
8. ✅ **M2: 安全加固完成** (2026-06-17)
   - ✅ Basic Auth实现
   - ✅ 审计日志系统
   - ✅ 输入验证模块
   - ✅ Rate limiting系统
9. ✅ **M3: 功能完善完成** (2026-06-17)
   - ✅ 会话管理核心（SessionManager）
   - ✅ 配置系统增强（TOML支持）
   - ✅ REST API端点
   - ✅ 自动会话清理
   - ✅ 完整CLI接口
10. ✅ **M4: 生产就绪完成** (2026-06-17)
   - ✅ WebSocket集成SessionManager
   - ✅ 前端会话ID显示
   - ✅ 文档完善
   - ✅ 代码质量验证

**🎉 项目核心功能已完成！**

未来可选增强：
- 完整的共享会话支持
- 前端高级功能
- Docker部署支持
- 性能优化

---

*文档版本：v0.1*  
*创建日期：2026-06-17*  
*维护者：wcx*  
*平台：Linux/macOS only*
