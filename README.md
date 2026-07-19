# RedKey

RedKey 是一个面向 Figma 任务的跨平台桌面指令台。它先用电脑全局快捷键模拟实体宏键盘，后续可通过统一动作接口接入 Arduino 或其他 USB 控制器。

## 已实现

- 10 个稳定数字槽位，排序不会改变键位绑定
- 任务标题、链接、联系人、进度、优先级、置顶与归档
- 完成任务后可恢复为进行中，恢复进度固定为 50%
- SQLite 本地持久化、完整 JSON 备份与恢复
- 全局快捷键、系统托盘、开机启动设置
- 控制台、快捷面板、透明置顶键帽宠物
- Figma URL 标题提取与公开元信息读取
- 默认浏览器打开任务链接

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
| 槽位 1–0 | `Control+Alt+1` 至 `Control+Alt+0` |
| 进度减少 / 增加 | `Control+Alt+Minus / Equal` |
| 优先级减少 / 增加 | `Control+Alt+Shift+Minus / Equal` |
| 完成当前任务 | `Control+Alt+Enter` |
| 恢复已完成任务 | `Control+Alt+R` |
| 打开控制台 | `Control+Alt+Space` |

快捷键可在设置中修改。若快捷键已被其他软件占用，RedKey 会拒绝保存新配置并恢复原配置。

## 暂未实现

- Figma Desktop 深链接
- 任务或窗口计时
- AI 排序
- 实体硬件串口连接
- 应用签名与正式分发

未来硬件协议见 [docs/HARDWARE_PROTOCOL.md](docs/HARDWARE_PROTOCOL.md)。
