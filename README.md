# AlphaKey

AlphaKey 是一个跨平台桌面工作台，用全局快捷键把正在推进的任务绑定到数字槽位，并集中管理任务资料、录音、转写、图片 OCR 和 AI 总结。

## 主要功能

- 10 个稳定数字槽位，通过可配置前缀键快速切换任务。
- 任务文档、联系人、链接、文本卡和图片卡统一归档。
- Windows 原生录音，录音结束后进入本地语音转写队列。
- SenseVoice 转写、FSMN-VAD、CT-Transformer 标点和 CAM++ 发言人识别。
- RapidOCR 本地图片文字识别。
- DeepSeek 任务总结；关闭云端 API 后可改为复制 prompt。
- 桌面宠物、快捷面板、HUD、系统托盘和开机启动。
- SQLite 本地持久化，以及 JSON 数据备份与恢复。

## 本地 AI 组件

Windows 安装包不再内置大型 Python 目录和语音模型，以避免安装包体积过大或损坏。

- CPU Python runtime 在首次使用本地 AI 时下载，当前 Windows x64 版本约 347 MB。
- SenseVoiceSmall、FSMN-VAD、CT-Transformer 和 CAM++ 按需下载，可在设置页单独管理。
- OCR 使用 CPU runtime 中 RapidOCR 自带的 PP-OCRv4 mobile 模型。
- 下载完成后，OCR、录音转写和发言人识别均可离线运行。
- 普通录音、播放和保存不依赖 Python runtime 或模型。

Runtime 和模型来自本仓库的 GitHub Releases，客户端会校验 SHA-256、ZIP 路径和解压结果，校验通过后才会启用。

## 安装

朋友直接使用的简明说明见 [Windows 使用说明](docs/windows-quick-start.md)。

使用条件：Windows 10 或 Windows 11、64 位系统；第一次使用本地 AI 功能时需要联网下载组件。无需自行安装 Python、Node.js 或其他开发环境。

Windows 构建产物为：

```text
src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/AlphaKey_0.1.0_x64-setup.exe
```

双击安装即可。首次使用 OCR 或语音转写时，应用会提示下载所需组件。安装包尚未做正式代码签名，因此 Windows SmartScreen 可能提示确认；仅应安装来自可信来源的安装包。

应用数据目录：

```text
%APPDATA%\com.hilith.alphakey
```

下载的 runtime 和模型也保存在该应用数据目录中。关闭控制台窗口只会隐藏窗口；需要从系统托盘选择“退出 AlphaKey”才能完全结束进程。

## 技术栈

| 层级 | 技术 |
| --- | --- |
| 桌面框架 | Tauri 2 |
| 前端 | React 19、TypeScript、Vite |
| 后端 | Rust |
| 数据库 | SQLite |
| Windows 录音 | cpal、hound |
| 本地语音 | FunASR、PyTorch、CAM++ |
| 本地 OCR | RapidOCR、ONNX Runtime |
| 云端总结 | DeepSeek API |

## 本地开发

环境要求：

- Node.js 20+
- Rust stable
- Windows: Visual Studio 2022 Build Tools，包含 C++ 桌面开发工作负载
- 构建 Windows runtime 时需要 7-Zip

安装依赖并启动开发环境：

```bash
npm install
npm run tauri dev
```

只启动前端：

```bash
npm run dev
```

## 测试

```bash
# TypeScript 类型检查和前端测试
npm run check

# Rust 测试
cargo test --manifest-path src-tauri/Cargo.toml

# 校验本机构建用 CPU runtime
npm run verify:runtime
```

## Windows 构建

```powershell
.\scripts\build-windows-x64.ps1
```

构建脚本会：

1. 校验或准备固定版本的便携 Python 依赖。
2. 生成并测试 `python-runtime-win-x64-v1.zip`。
3. 写入 runtime manifest 和 SHA-256 文件。
4. 构建 React 前端和 Tauri NSIS 安装包。
5. 使用 `7z t` 检查最终安装包完整性。

runtime ZIP 不会进入安装包，需要将 ZIP 和对应的 `.sha256` 上传到 `runtime-v1` Release。

## 项目结构

```text
src/                              React 前端
src-tauri/src/                    Tauri/Rust 后端
src-tauri/src/runtime.rs          Python runtime 下载与校验
src-tauri/src/speech.rs           语音模型与 worker 管理
src-tauri/src/ocr.rs              OCR worker 管理
workers/                          Python 语音和 OCR worker
runtime/requirements.lock         固定 Python 依赖
scripts/build-windows-x64.ps1     Windows 构建脚本
scripts/verify-runtime.mjs        构建前 runtime 检查
```

## 当前限制

- Windows 本地 AI 当前只使用 CPU。
- OCR 首次加载及超大图片识别可能耗时较长，但识别会在后台执行，不应阻塞界面。
- Windows 安装包尚未进行正式代码签名。
