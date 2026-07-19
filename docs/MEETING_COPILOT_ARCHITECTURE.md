# RedKey 对接录音与会议副驾设计

状态：架构讨论稿

本文讨论 RedKey 的录音、转写、发言人分离和需求总结功能。目标是让用户在当前任务上下文中按一次实体宏键开始录音，结束后得到可检索的对接记录，并由 AI 提取双方最终对齐的需求。

## 1. 产品目标

### 核心流程

```text
当前任务
  -> 宏键按下
  -> 电脑开始录音，同时本地 SenseVoice 生成实时转写
  -> 宏键再次按下
  -> 保存音频和实时转写
  -> 后台进行最终转写、发言人分离和质量修正
  -> DeepSeek 生成需求对齐总结
  -> 用户确认、修改并保存到当前任务
```

### 第一版必须做到

- 一个宏键切换录音开始/停止。
- 录音始终绑定当时的当前任务。
- 录音可以独立运行，不依赖 AI；音频和原始转写本地保存。
- 录音过程中显示低延迟的本地转写。
- 录音结束后生成 `Speaker A / Speaker B` 分段。
- 用户可以把 A、B 改成具体联系人。
- DeepSeek 根据完整对话生成需求、决策和待办事项。
- 网络不可用时保留音频，之后可以重新处理。

### 第一版明确不做

- 不做声纹身份识别，不自动判断 A 是哪位同事。
- 不承诺多人同时说话时的完美识别。
- 不让 Arduino/宏键盘独立保存音频；录音由电脑完成，硬件只负责输入动作。
- 不强制实时完成发言人标记；实时字幕和会后精确分离分成两条处理链。

## 2. 导航与界面

左侧导航建议调整为：

```text
进行中
已完成
归档
对接记录
设置
```

“对接记录”放在“设置”上方。它既是历史会议列表，也是录音入口，不把录音功能藏进设置。

### 对接记录页布局

桌面端采用三栏固定布局：

```text
┌──────────────┬──────────────────────────────┬──────────────────┐
│ 对接记录列表  │ 当前对话 / Speaker A/B        │ AI 对齐分析       │
│              │                              │                  │
│ 日期、任务、   │ 时间轴、文本、发言人标签      │ 需求总结          │
│ 时长、状态     │ 搜索、编辑、播放音频          │ 决策、待办、风险  │
│              │                              │ 追问输入          │
└──────────────┴──────────────────────────────┴──────────────────┘
```

#### 左栏：对接记录列表

- 按最近录音时间倒序。
- 每行显示任务标题、录音时间、时长、处理状态。
- 状态包括：录音中、待处理、转写中、待总结、已完成、处理失败。
- 支持按任务、日期和关键词筛选。
- 删除操作必须二次确认，同时删除音频、转写和 AI 结果。

#### 中栏：对话详情

- 顶部显示任务标题、录音时间、总时长和处理状态。
- 录音中显示实时字幕和音量状态。
- 会后显示按时间排列的分段：

  ```text
  00:12  Speaker A  这个页面需要改成两步流程。
  00:18  Speaker B  移动端也需要同步调整。
  ```

- Speaker A/B 可重命名；重命名只影响当前对接记录，不建立永久声纹身份。
- 支持点击分段播放对应音频区间。
- 原始转写和用户修改后的文本都要保留，避免 AI 结果无法追溯。

#### 右栏：AI 对齐分析

固定展示以下模块：

- 一句话结论。
- 已确认需求。
- 需求变更。
- 已做决定。
- 待办事项：内容、负责人、截止时间。
- 未解决问题和风险。
- “继续提问”输入框，问题上下文限定在当前对接记录。

AI 结果必须显示生成状态和来源版本，例如“基于第 32 段转写生成”。转写被用户修改后，提示重新生成总结。

### 录音中状态

录音中不打开新的大窗口，保留当前工作流：

- 宠物显示红色录音状态和计时。
- 快捷面板显示当前任务、录音时长、实时字幕尾部和停止按钮。
- 控制台“对接记录”页显示完整实时转写。
- 再次按宏键停止录音，不需要鼠标切换窗口。

## 3. 技术架构

继续使用现有架构：

- 桌面壳：Tauri 2。
- 前端：React + TypeScript。
- 业务和持久化：Rust。
- 数据库：SQLite。
- 音频采集：平台原生 helper，通过统一接口接入 Rust。
- 本地 ASR：SenseVoice-Small，优先使用 sherpa-onnx/FunASR 可部署运行时。
- VAD 和音频质量：本地 VAD、降噪、静音过滤、音量和重叠检测。
- 发言人分离：3D-Speaker/CAM++ 或同类 ONNX 推理组件。
- AI 分析：DeepSeek OpenAI-compatible API；后续可增加本地 LLM。

### 为什么不把所有逻辑放进 Rust

Rust 负责生命周期、权限、文件、SQLite、快捷键和事件广播；ASR/diarization 模型的推理依赖较重，放在独立 worker 中更容易升级和跨平台打包。

推荐进程边界：

```text
Tauri/Rust 主进程
  ├─ SQLite、任务绑定、全局快捷键
  ├─ 录音会话状态和音频文件管理
  ├─ Worker 生命周期、进度和错误
  └─ 前端事件广播

AudioCaptureHelper
  └─ 麦克风 PCM 音频流

SpeechWorker
  ├─ VAD
  ├─ SenseVoice 实时/最终转写
  ├─ 3D-Speaker 发言人分离
  └─ 时间戳和分段合并

DeepSeek Provider
  └─ 结构化总结和当前对接记录问答
```

### 平台采集策略

#### macOS 第一阶段

借鉴 `ai-meeting` 的 Swift `AVFoundation` helper：

- 读取默认或用户选择的麦克风。
- 输出 16 kHz、单声道、PCM 16-bit 分块。
- 通过 JSON Lines 或标准输出传给 Tauri worker。
- 显式处理麦克风权限、设备断开和录音结束。

#### Windows 后续阶段

- 使用 WASAPI/Windows Audio Capture helper。
- 保持与 macOS 相同的 PCM 输出协议。
- 前端和业务层不感知平台差异。

## 4. 语音处理链路

### 实时链路

实时目标是“快速可读”，不是最终稿：

```text
PCM chunk
  -> 音量/质量检测
  -> VAD 分段
  -> SenseVoice-Small
  -> 临时字幕
  -> 去重、尾段修正、术语替换
```

建议实时分段 2-5 秒，保留前后少量 overlap，避免句首句尾被截断。实时字幕标记为 provisional，停止录音后必须重新整理。

### 会后最终链路

```text
完整音频
  -> VAD
  -> 3D-Speaker 发言人分离
  -> Speaker A/B/C 时间片段
  -> SenseVoice 最终转写
  -> 时间戳合并
  -> 用户可编辑的最终 transcript
```

SenseVoice 负责“说了什么”，3D-Speaker 负责“谁在什么时候说话”。两者不要混成一个模型能力来设计。

### 两人场景的实际策略

- 默认自动估计发言人数，优先支持 2 人，允许扩展到 3-5 人。
- 对短句、同时说话和噪声片段保留 `unclear` 或 `overlap` 标记。
- 不把 Speaker A/B 直接绑定到联系人姓名。
- 用户修改 A/B 名称后，只修改当前会议记录。
- 先保证“可回听、可修正、可追溯”，再优化自动分离准确率。

## 5. 数据模型

在现有 SQLite 中新增以下表：

### `meeting_sessions`

- `id`
- `task_id`：可为空，录音时没有当前任务也允许保存
- `title`
- `status`
- `started_at`
- `ended_at`
- `duration_ms`
- `audio_path`
- `capture_device`
- `processing_status`
- `created_at`
- `updated_at`

### `meeting_transcript_segments`

- `id`
- `session_id`
- `seq`
- `speaker_id`：例如 `speaker_0`
- `speaker_name`：用户重命名后的名称，可为空
- `start_ms`
- `end_ms`
- `text`
- `raw_text`
- `is_final`
- `quality`
- `overlap_detected`
- `created_at`

### `meeting_speakers`

- `session_id`
- `speaker_id`
- `display_name`
- `sort_order`

### `meeting_summaries`

- `id`
- `session_id`
- `overview`
- `decisions_json`
- `requirements_json`
- `changes_json`
- `action_items_json`
- `risks_json`
- `source_segment_seq`
- `source_transcript_hash`
- `model`
- `status`
- `created_at`
- `updated_at`

### `meeting_questions`

- `id`
- `session_id`
- `question`
- `answer`
- `model`
- `created_at`

音频文件不直接塞进 SQLite，保存在应用数据目录，数据库只保存路径、格式、大小和校验信息。默认录音格式优先使用 WAV/PCM 便于处理；归档或导出时可以转为 Opus/AAC。

## 6. 统一输入接口

实体宏键、电脑快捷键、宠物按钮和未来 Arduino 都转换为统一动作：

```ts
type MeetingAction =
  | { type: "toggle_recording" }
  | { type: "stop_recording" }
  | { type: "pause_recording" }
  | { type: "resume_recording" }
  | { type: "open_meetings" };
```

第一版只需要 `toggle_recording`。录音动作执行时读取当前任务 ID，并在会话创建时固化，之后即使当前任务切换，对接记录也不会被错误归类。

## 7. AI 分析设计

### DeepSeek 调用原则

- Rust 后端调用 DeepSeek，API Key 不暴露给前端。
- 使用 OpenAI-compatible client，后续可替换其他模型。
- Prompt 输入包含：任务标题、联系人、完整转写、用户修改、历史总结（可选）。
- 强制要求 JSON 结构输出，解析失败时保留原始响应并提示人工检查。
- 总结结果必须能追溯到转写版本。

### 推荐输出结构

```json
{
  "overview": "双方确认登录流程改为两步，并同步修改移动端。",
  "requirements": ["..."],
  "changes": ["..."],
  "decisions": ["..."],
  "actionItems": [
    { "text": "补充移动端流程稿", "owner": "Speaker B", "due": null }
  ],
  "risks": ["接口改动范围尚未确认"]
}
```

问答只允许基于当前会议的转写和总结回答，并标记“根据当前记录推断”，避免把模型猜测伪装成双方已经确认的事实。

## 8. 参考 `ai-meeting` 的内容

参考项目：[hilithqiyuanlu/ai-meeting](https://github.com/hilithqiyuanlu/ai-meeting)，当前核对版本为 `v0.4.6`。

可以直接借鉴的思路：

- Swift 音频 helper 与平台采集隔离。
- SQLite 保存会议、转写和纪要，而不是只保存一个最终文本。
- SenseVoice 本地模型下载、模型状态和失败恢复。
- VAD、音频质量指标、静音跳过、重叠检测。
- ASR/LLM provider 抽象，允许本地和云端切换。
- 会议纪要版本与源转写段数绑定。
- 术语库和文本后处理。

不直接照搬的部分：

- `ai-meeting` 使用 Electron；RedKey 继续 Tauri + Rust。
- `ai-meeting` 的系统音频/BlackHole 是会议场景扩展，RedKey 第一版先聚焦面对面对接的麦克风。
- `ai-meeting` 的大量实时质量指标可以逐步吸收，不在第一版一次性全部暴露。

## 9. 分阶段实施

### Phase 0：录音骨架

- 增加“对接记录”导航页签。
- 增加开始/停止录音动作和当前任务绑定。
- macOS 麦克风 helper 输出 PCM。
- SQLite 保存录音会话和音频文件。
- 支持纯录音，不依赖 ASR 或 AI。

验收：按一次宏键开始，再按一次停止；重启应用后记录仍存在，音频可以播放。

### Phase 1：本地实时 SenseVoice

- 管理 SenseVoice 模型下载和状态。
- 加入 VAD、分段、实时字幕和错误状态。
- 录音中显示最后若干条字幕。
- 停止时补齐最后一段并保存原始转写。

验收：普通中文对话中，实时字幕延迟可接受；网络断开不影响录音和本地转写。

### Phase 2：会后发言人分离

- 集成 3D-Speaker/CAM++ worker。
- 产生 Speaker A/B 分段和时间戳。
- 允许用户重命名和修正分段。
- 支持回放对应音频区间。

验收：两人轮流发言的录音能够生成可读的 A/B 对话；重叠片段明确标记，不伪装成确定结果。

### Phase 3：DeepSeek 需求总结与问答

- 实现结构化总结 JSON。
- 右侧 AI 分析栏。
- 支持重新生成和当前记录问答。
- 保存模型、源文本版本和生成时间。

验收：用户可以看到需求、决策、待办、风险，并能追溯到原始对话。

### Phase 4：跨平台与硬件输入

- Windows WASAPI helper。
- Windows 安装包和权限引导。
- Arduino/宏键盘通过 `MeetingAction` 接入，不修改会议业务层。
- 录音处理队列、失败重试和导出。

## 10. 风险与约束

- 单个远场麦克风的发言人分离准确率受距离、噪声、抢话影响；必须允许人工修正。
- “实时转写准确”与“会后最终准确”是两个目标，不能用同一套短片段结果替代最终稿。
- 本地模型需要占用 CPU、内存和磁盘；首次下载必须显示进度和取消状态。
- 云端 ASR/DeepSeek 会产生音频或文本出站传输，必须明确提示并提供纯本地模式。
- 录音前应有明确的录音状态和停止操作；涉及同事录音时需要取得同意，并提供删除能力。
- API Key 只放在 Rust 后端配置，不放入 React 构建产物。

## 11. 第一版技术决策

| 问题 | 决策 |
|---|---|
| 桌面框架 | 继续 Tauri 2 + React + TypeScript + Rust |
| 数据 | SQLite + 本地音频文件 |
| 录音 | macOS Swift helper，16 kHz 单声道 PCM |
| 实时转写 | SenseVoice-Small + VAD，本地运行 |
| 会后分离 | 3D-Speaker/CAM++，输出匿名 Speaker A/B |
| AI | DeepSeek OpenAI-compatible API |
| 录音模式 | 电脑录音，宏键只发送开始/停止动作 |
| 第一版身份识别 | 不做声纹，用户手动命名 A/B |
| 第一版联网依赖 | 录音和转写可离线；AI 总结需要配置 DeepSeek 或后续本地 LLM |
