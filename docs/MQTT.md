# MQTT客户端接口对接

## 概述

本文档定义 **MQTT 客户端（Client）** 与 **服务端（Server）** 的 Topic 约定与消息体（Payload）格式，用于客户端状态上报、任务事件通知等场景。

- Topic 统一以 `mt/client/` 为前缀。
- `{clientId}`：客户端唯一标识（示例：`auto-abc123`）。
- `eventAt`："yyyy-MM-dd HH:mm:ss"。
- JSON 字段命名使用 `camelCase`。

---

## 通用字段约定

| 字段       | 类型   | 是否必填             | 说明           | 示例                    |
| ---------- | ------ | -------------------- | -------------- | ----------------------- |
| `clientId` | string | 是（除广播类 Topic） | 客户端唯一标识 | `auto-abc123`           |
| `eventAt`  | string | 建议必填             | 事件发生时间   | `yyyy-MM-dd HH:mm:ss`   |
| `reason`   | string | 否                   | 原因说明       | `unexpected_disconnect` |

---

## 2.3 上行 Topic（客户端 → 服务端）

客户端发布（Publish）到服务端订阅（Subscribe）的 Topic。

### 2.3.1 设备上线事件

客户端成功连接并完成业务初始化后发送。

- **Topic**：`mt/client/{clientId}/online`

**字段说明**

| 字段       | 类型   | 必填 | 说明           |
| ---------- | ------ | ---- | -------------- |
| `clientId` | string | 是   | 客户端唯一标识 |
| `eventAt`  | string | 建议 | 上线时间       |

**Payload**

```json
{
  "clientId": "auto-abc123",
  "eventAt": "2025-03-03 12:00:00"
}
```

---

### 2.3.2 设备下线事件

客户端主动断开或准备退出时发送。

- **Topic**：`mt/client/{clientId}/offline`

**字段说明**

| 字段       | 类型   | 必填 | 说明           |
| ---------- | ------ | ---- | -------------- |
| `clientId` | string | 是   | 客户端唯一标识 |
| `eventAt`  | string | 建议 | 下线时间       |
| `reason`   | string | 否   | 下线原因       |

**Payload**

```json
{
  "clientId": "auto-abc123",
  "eventAt": "2025-03-03 12:00:00",
  "reason": "user_logout"
}
```

---

### 2.3.3 心跳上报

用于维持在线状态、上报活跃时间。

- **Topic**：`mt/client/{clientId}/heartbeat`

**字段说明**

| 字段       | 类型   | 必填 | 说明           |
| ---------- | ------ | ---- | -------------- |
| `clientId` | string | 是   | 客户端唯一标识 |
| `eventAt`  | string | 建议 | 心跳时间       |

**Payload**

```json
{
  "clientId": "auto-abc123",
  "eventAt": "2025-03-03 12:00:00"
}
```

---

### 2.3.4 任务状态事件

客户端在任务生命周期关键节点上报。

- **Topic**：`mt/client/{clientId}/task/event`

**字段说明**

| 字段        | 类型   | 必填 | 说明                 |
| ----------- | ------ | ---- | -------------------- |
| `clientId`  | string | 是   | 客户端唯一标识       |
| `taskId`    | string | 是   | 任务 ID              |
| `eventType` | string | 是   | 事件类型（建议枚举） |
| `eventAt`   | string | 建议 | 事件时间             |

**建议枚举（eventType）**

| 值         | 含义     | 备注 |
| ---------- | -------- | ---- |
| `started`  | 开始执行 | -    |
| `paused`   | 暂停     | -    |
| `continue` | 继续执行 | -    |
| `restart`  | 重新开始 | -    |
| `stopped`  | 停止     | -    |

**Payload**

```json
{
  "clientId": "auto-abc123",
  "taskId": "task-001",
  "eventType": "started",
  "eventAt": "2025-03-03 12:00:00"
}
```

---

### 2.3.5 LWT 遗嘱消息（异常断开自动发送）

当客户端异常断开连接时，由 MQTT Broker 自动发布（需在连接时设置 Will Message）。

- **Topic**：`mt/client/{clientId}/lwt`

**字段说明**

| 字段       | 类型   | 必填 | 说明           |
| ---------- | ------ | ---- | -------------- |
| `clientId` | string | 是   | 客户端唯一标识 |
| `reason`   | string | 建议 | 断开原因       |

**Payload**

```json
{
  "clientId": "auto-abc123",
  "reason": "unexpected_disconnect"
}
```

---

## 2.4 下行 Topic（服务端 → 客户端）

服务端发布到客户端订阅的 Topic。

### 2.4.1 任务数据变更通知

用于提示客户端任务数据有变更，需重新拉取/刷新。

- **Topic**：`mt/client/{clientId}/taskChanged`

**字段说明**

| 字段      | 类型   | 必填 | 说明                       |
| --------- | ------ | ---- | -------------------------- |
| `taskId`  | string | 是   | 任务 ID                    |
| `action`  | string | 是   | 客户端处理动作（建议枚举） |
| `eventAt` | string | 建议 | 通知时间                   |

**建议枚举（action）**

| 值            | 含义                      | 备注       |
| ------------- | ------------------------- | ---------- |
| `reload_task` | 重新拉取任务（按 taskId） | 推荐       |
| `delete_task` | 本地删除任务              | 按业务需要 |

**Payload**

```json
{
  "taskId": "task-001",
  "action": "reload_task",
  "eventAt": "2025-03-03 12:00:00"
}
```

---

### 2.4.2 手机号解绑通知

服务端通知客户端：指定手机号被抢占/解绑，需要客户端执行账号/设备绑定逻辑的更新。

- **Topic**：`mt/client/{clientId}/unbind`

**字段说明**

| 字段      | 类型     | 必填 | 说明             |
| --------- | -------- | ---- | ---------------- |
| `mobiles` | string[] | 是   | 受影响手机号列表 |
| `reason`  | string   | 建议 | 解绑原因         |
| `eventAt` | string   | 建议 | 通知时间         |

**Payload**

```json
{
  "mobiles": ["13800138000", "123123123"],
  "reason": "occupied",
  "eventAt": "2025-03-03 12:00:00"
}
```

---

### 2.4.3 截止工作时间到达（广播）

服务端在截止时间到达时广播，通知所有客户端停止工作/进入离线状态。

> 注意：**工作时间限制的收口建议放在「绑定账号接口」处理**。即在绑定账号成功/续期时下发或刷新“可工作时间窗口”，超出窗口则拒绝工作/拉任务，并可配合本 Topic 做兜底广播下线，确保所有客户端状态一致。

- **Topic**：`mt/client/broadcast/offline`

**字段说明**

| 字段      | 类型   | 必填 | 说明     |
| --------- | ------ | ---- | -------- |
| `eventAt` | string | 建议 | 广播时间 |

**Payload**

```json
{
  "eventAt": "2025-03-03 12:00:00"
}
```

---

## 附录：Topic 汇总

| 方向 | Topic                              | 发布方 → 订阅方         | 用途             | 关键字段                                     |
| ---- | ---------------------------------- | ----------------------- | ---------------- | -------------------------------------------- |
| 上行 | `mt/client/{clientId}/online`      | Client → Server         | 设备上线         | `clientId`, `eventAt`                        |
| 上行 | `mt/client/{clientId}/offline`     | Client → Server         | 设备下线         | `clientId`, `eventAt`, `reason?`             |
| 上行 | `mt/client/{clientId}/heartbeat`   | Client → Server         | 心跳上报         | `clientId`, `eventAt`                        |
| 上行 | `mt/client/{clientId}/task/event`  | Client → Server         | 任务状态事件     | `clientId`, `taskId`, `eventType`, `eventAt` |
| 上行 | `mt/client/{clientId}/lwt`         | Broker → Server（自动） | LWT 遗嘱消息     | `clientId`, `reason`                         |
| 下行 | `mt/client/{clientId}/taskChanged` | Server → Client         | 任务数据变更通知 | `taskId`, `action`, `eventAt`                |
| 下行 | `mt/client/{clientId}/unbind`      | Server → Client         | 手机号解绑通知   | `mobiles`, `reason`, `eventAt`               |
| 下行 | `mt/client/broadcast/offline`      | Server → Client（广播） | 截止工作时间到达 | `eventAt`                                    |
