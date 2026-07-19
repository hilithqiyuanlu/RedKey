# RedKey 硬件协议草案 v1

硬件通过 USB CDC 串口发送 UTF-8 JSON Lines。每行是一个完整 JSON 对象，以 `\n` 结束。设备只报告输入，桌面应用负责任务判断和数据保存。

## 设备发送

```json
{"event":"slot","slot":0}
{"event":"progress","delta":5}
{"event":"priority","delta":1}
{"event":"complete"}
{"event":"rework"}
{"event":"cancel_rework"}
```

`slot` 使用内部编号 `0–9`，对应可见按键 `1–9、0`。未知事件应被忽略，格式错误的行应记录但不能中断后续读取。

## 桌面应用发送

首版实体硬件接入时增加以下消息：

```json
{"command":"state","connected":true,"slot":0,"title":"阿伟 · 登录页改版","progress":65,"priority":3,"status":"active","revision":1}
{"command":"notice","level":"attention","text":"槽位未绑定"}
```

状态值为 `active`、`completed` 或 `rework`。硬件可根据能力选择显示标题、进度或状态灯，不应依赖未声明字段。

## 接入边界

Rust 中的 `HardwareInputAdapter` 负责把一行设备消息转换成 `AppAction`。串口连接层只负责发现设备、重连、逐行读取和发送状态，不得直接读写 SQLite。

