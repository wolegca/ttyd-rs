# ttyd-rs 项目文档索引

**项目**: ttyd-rs - 基于 Rust 的 Web 终端共享工具  
**版本**: 0.1.0  
**状态**: M1 已完成 ✅

---

## 📂 文档结构

### 核心文档
- [README.md](../../README.md) - 项目说明和快速开始
- [CLAUDE.md](../../CLAUDE.md) - 开发指南（给 Claude Code 的指引）
- [DEVELOPMENT_GOALS.md](../../DEVELOPMENT_GOALS.md) - 完整开发路线图

### 技术文档
- [docs/PROTOCOL.md](../PROTOCOL.md) - WebSocket 协议规范

### 项目报告
- [CURRENT_STATUS.md](CURRENT_STATUS.md) - 📍 **项目当前状态（推荐阅读）**
- [M1_REPORT.md](M1_REPORT.md) - M1 里程碑完成报告
- [QUALITY_CHECK_REPORT.md](QUALITY_CHECK_REPORT.md) - 代码质量检查报告
- [PROJECT_STATUS.md](PROJECT_STATUS.md) - 详细项目状态报告
- [PROJECT_SUMMARY.md](PROJECT_SUMMARY.md) - 项目总结
- [INIT_REPORT.md](INIT_REPORT.md) - 项目初始化报告
- [FINAL_REPORT.md](FINAL_REPORT.md) - 最终完成报告

---

## 🎯 快速导航

### 想要了解项目当前状态？
👉 [CURRENT_STATUS.md](CURRENT_STATUS.md) - 最新状态、功能列表、下一步计划

### 想要开始开发？
👉 [CLAUDE.md](../../CLAUDE.md) - 开发规范、构建命令、代码质量要求

### 想要了解协议？
👉 [docs/PROTOCOL.md](../PROTOCOL.md) - WebSocket 消息协议详细规范

### 想要查看开发路线？
👉 [DEVELOPMENT_GOALS.md](../../DEVELOPMENT_GOALS.md) - 4 个阶段的完整规划

### 想要了解 M1 完成情况？
👉 [M1_REPORT.md](M1_REPORT.md) - M1 里程碑详细报告

### 想要查看代码质量？
👉 [QUALITY_CHECK_REPORT.md](QUALITY_CHECK_REPORT.md) - 质量检查执行报告

---

## 📊 报告说明

### 按时间顺序
1. **INIT_REPORT.md** - 项目初始化阶段（2026-06-17 早期）
2. **M1_REPORT.md** - M1 里程碑完成（2026-06-17 中期）
3. **QUALITY_CHECK_REPORT.md** - 代码质量检查（2026-06-17 21:42）
4. **CURRENT_STATUS.md** - 当前状态（2026-06-17 21:46）⭐

### 按用途分类

#### 阶段性报告
- **INIT_REPORT.md** - 记录项目启动时的初始化工作
- **M1_REPORT.md** - M1 里程碑的详细完成情况
- **FINAL_REPORT.md** - 整体项目完成报告（包含 M1+M2 部分）

#### 状态报告
- **CURRENT_STATUS.md** - 📍 实时状态（推荐）
- **PROJECT_STATUS.md** - 详细状态分析
- **PROJECT_SUMMARY.md** - 项目总结概览

#### 质量报告
- **QUALITY_CHECK_REPORT.md** - 代码质量工具执行结果

---

## 🔄 文档更新频率

| 文档 | 更新频率 | 说明 |
|------|---------|------|
| CURRENT_STATUS.md | 实时 | 反映项目最新状态 |
| M1_REPORT.md | 里程碑完成时 | M1 完成后不再更新 |
| QUALITY_CHECK_REPORT.md | 质量检查时 | 每次检查后更新 |
| PROJECT_STATUS.md | 定期 | 详细状态快照 |
| PROJECT_SUMMARY.md | 阶段性 | 阶段总结 |
| INIT_REPORT.md | 一次性 | 初始化完成后不更新 |
| FINAL_REPORT.md | 项目完成时 | 最终报告 |

---

## 💡 阅读建议

### 新手开发者
1. 先读 [README.md](../../README.md) 了解项目
2. 再读 [CURRENT_STATUS.md](CURRENT_STATUS.md) 了解现状
3. 然后读 [CLAUDE.md](../../CLAUDE.md) 学习开发规范

### 想要贡献代码
1. 阅读 [CLAUDE.md](../../CLAUDE.md) 了解规范
2. 阅读 [DEVELOPMENT_GOALS.md](../../DEVELOPMENT_GOALS.md) 了解路线图
3. 查看 [CURRENT_STATUS.md](CURRENT_STATUS.md) 的"下一步计划"

### 想要使用项目
1. 阅读 [README.md](../../README.md) 快速开始
2. 查看 [CURRENT_STATUS.md](CURRENT_STATUS.md) 了解限制
3. 参考 Nginx 反向代理配置

### 想要了解技术细节
1. 阅读 [docs/PROTOCOL.md](../PROTOCOL.md) 协议规范
2. 阅读 [M1_REPORT.md](M1_REPORT.md) 技术实现
3. 查看源代码注释

---

## 📋 文档维护

### 维护原则
- **CURRENT_STATUS.md** 始终保持最新
- 里程碑报告完成后不再修改
- 质量报告记录检查时的快照
- 所有报告放在 `docs/reports/` 目录

### 更新指南
- 有重大进展时更新 CURRENT_STATUS.md
- 完成里程碑时创建对应报告
- 运行质量检查时更新 QUALITY_CHECK_REPORT.md
- 项目根目录仅保留核心文档

---

*文档索引最后更新: 2026-06-17*
