# RedKey

RedKey 是一个跨平台的桌面工作台，用键盘快捷键将需求任务绑定到数字键上，支持录音、转写、发言人分离和 AI 梳理，适用于设计评审、需求对接等协作场景。

## 技术栈

| 层级 | 技术 |
|------|------|
| 桌面框架 | Tauri 2 |
| 前端 | React 19 + TypeScript |
| 后端 | Rust |
| 数据库 | SQLite |
| 录音 (macOS) | Swift AVFoundation helper |
| 录音 (Windows) | cpal + hound (WAV) |
| 全局快捷键 (macOS) | CoreGraphics Event Tap |
| 全局快捷键 (Windows) | SetWindowsHookExW + WH_KEYBOARD_LL |
| 实时转写 | SenseVoice-Small (Python worker) |
| 发言人分离 | 3D-Speaker / CAM++ (Python worker) |
| AI 总结 | DeepSeek API |

## 功能

### 需求管理
- 10 个稳定数字槽位 (Control+1~0)，排序不改变键位绑定
- 需求文档、联系人、链接、文本卡，支持最近使用排序
- 完成任务后释放键位，返工时重新分配
- SQLite 本地持久化，完整 JSON 备份与恢复

### 录音与语音处理
- 原生录音（macOS 使用 AVFoundation，Windows 使用 cpal），输出 WAV 格式
- 录音过程中实时本地转写（SenseVoice）
- 录音结束后自动进行发言人分离（二人场景精确切分 A/B 对话）
- 转写时间对齐，支持点击分段回放对应音频区间
- 可修改 Speaker A/B 名称，修正转写文本

### AI 梳理
- DeepSeek 根据完整对话生成需求对齐总结
- 输出需求、决策、待办和风险
- API Key 加密保存在系统钥匙串，不暴露给前端
- 总结可追溯到原始转写版本

### 桌面交互
- 全局快捷键：按下修饰键 （默认 Control）显示 HUD 悬浮提示条，松开隐藏
- 透明置顶键帽宠物，可拖拽，悬浮显示快捷面板
- 快捷面板支持快速切换任务、拖入链接创建新任务
- 系统托盘：打开控制台、休眠/唤醒宠物、设置、退出
- 开机启动
- Figma URL 自动提取标题

## 本地开发

### 环境要求

- Node.js 20+
- Rust stable
- macOS: Xcode（含 Command Line Tools）
- Windows: Visual Studio 2022 Build Tools（含 C++ 桌面开发工作负载）

### 快速开始

```bash
npm install
npm run tauri dev
```

仅预览前端（不启动 Rust 后端）：

```bash
npm run dev
```

### 测试与构建

```bash
# 类型检查 + 测试
npm run check

# Rust 测试
cd src-tauri && cargo test

# macOS 构建
npm run tauri build -- --bundles app

# Windows 构建 (NSIS 安装包)
npm run tauri build -- --bundles nsis
```

构建产物位于 `src-tauri/target/release/bundle/`。

### 项目结构

```
├── src/                    # React 前端
│   ├── App.tsx             # 主视图路由 (console / pet / quick / hud / settings)
│   ├── api.ts              # Tauri invoke 封装
│   ├── domain.ts           # 领域模型与状态管理
│   └── types.ts            # TypeScript 类型定义
├── src-tauri/              # Tauri + Rust 后端
│   ├── src/
│   │   ├── lib.rs          # 主入口，命令注册，录音/转写/分离管线
│   │   ├── db.rs           # SQLite 持久层
│   │   ├── models.rs       # 数据模型
│   │   ├── speech.rs       # 语音处理 worker 管理
│   │   ├── llm.rs          # DeepSeek API 调用
│   │   ├── keyboard_windows.rs   # Windows 全局键盘钩子
│   │   ├── recording_windows.rs  # Windows 原生录音 (cpal)
│   │   └── hardware.rs     # 硬件接口模型
│   └── tauri.conf.json     # Tauri 配置（窗口、打包、权限）
├── workers/                # Python 语音处理 worker
│   ├── qwen_asr_worker.py          # SenseVoice 实时转写
│   └── diarization_worker.py       # 3D-Speaker 发言人分离
└── docs/
    └── MEETING_COPILOT_ARCHITECTURE.md  # 录音与会议副驾架构设计
```

### 窗口说明

| 窗口标签 | 用途 |
|----------|------|
| `console` | 主控制台，需求管理、对接记录、设置 |
| `pet` | 透明置顶宠物，悬浮快捷面板入口 |
| `quick-panel` | 快捷面板，快速切换任务、链接拖入 |
| `hud` | 悬浮提示条，修饰键按下时显示槽位任务概览 |

### 数据存储

SQLite 数据库位于系统应用数据目录下的 `com.hilith.redkey/redkey.sqlite3`。关闭控制台窗口只是隐藏；从系统托盘选择"退出 RedKey"才会完全结束进程。

## 默认快捷键

| 动作 | 快捷键 |
|------|--------|
| 槽位 1–0 | `Control+1` 至 `Control+0` |

快捷键可在设置中修改。若新快捷键冲突，RedKey 会拒绝保存并恢复原配置。

## 暂未实现

- Figma Desktop 深链接
- OCR 图片卡
- 任务级 AI 快照
- 自然语言 Shortcut workflow
- 实体硬件串口连接
- 应用签名与正式分发

未来硬件协议见 [docs/HARDWARE_PROTOCOL.md](docs/HARDWARE_PROTOCOL.md)。
