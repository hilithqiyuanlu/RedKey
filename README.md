# RedKey

RedKey 是一个面向多需求协作的跨平台桌面工作台。每个需求绑定一个数字键，并在同一份需求文档中保存文本、录音、转写和 AI 梳理结果。

## 已实现

- 10 个稳定数字槽位，排序不会改变键位绑定
- 需求文档、联系人、链接、文本卡和最近使用排序
- 10 个数字键快速切换需求，完成后释放键位，返工时重新分配
- SQLite 本地持久化、完整 JSON 备份与恢复
- 全局快捷键、系统托盘、开机启动设置
- 控制台、快捷面板、透明置顶键帽宠物
- Figma URL 标题提取与公开元信息读取
- 默认浏览器打开任务链接
- 本地录音、转写、时间对齐和发言人分离
- DeepSeek 录音梳理，API Key 保存在系统钥匙串

## 本地开发

环境要求：Node.js 20+、Rust stable、macOS Xcode 或 Windows C++ Build Tools。

```bash
npm install
npm run tauri dev
```

只预览前端界面：

```bash
npm run dev
```

测试与构建：

```bash
npm run check
cd src-tauri && cargo test
# macOS
npm run tauri build -- --bundles app

# Windows
npm run tauri build -- --bundles nsis
```

SQLite 文件位于系统应用数据目录下的 `com.hilith.redkey/redkey.sqlite3`。关闭控制台只会隐藏窗口；从系统托盘选择“退出 RedKey”才会结束进程。

## 默认快捷键

| 动作 | 快捷键 |
| --- | --- |
| 槽位 1–0 | `Control+1` 至 `Control+0` |

快捷键可在设置中修改。若快捷键已被其他软件占用，RedKey 会拒绝保存新配置并恢复原配置。

## 暂未实现

- Figma Desktop 深链接
- OCR 图片卡
- 任务级 AI 快照
- 自然语言 Shortcut workflow
- 实体硬件串口连接
- 应用签名与正式分发

未来硬件协议见 [docs/HARDWARE_PROTOCOL.md](docs/HARDWARE_PROTOCOL.md)。
