# Mock 测试场景

## 使用方式

在 DB 设置中设置 `mock_scenario` 为场景文件名（不含 .json），或通过前端设置页修改。

默认场景为 `default`，即 `resources/mock/scenarios/default.json`。

## 场景列表

| 场景文件           | 说明                 | 测试的功能                  |
| ------------------ | -------------------- | --------------------------- |
| `default`          | 全部绑定成功，无冲突 | 正常启动同步流程            |
| `conflict_partial` | 1个成功 + 1个冲突    | 异地登录部分冲突清理        |
| `conflict_all`     | 全部冲突             | 异地登录全部失效 → 跳绑定页 |
| `new_tasks`        | 成功 + 额外新任务    | 服务端新增任务同步到本地    |

## 文件结构

```json
{
  "bind_phones": {          // mock bind_phones 响应
    "taskItems": ["task-1"], // 绑定成功后返回的任务 ID
    "conflicts": [          // 冲突列表
      { "mobile": "139...", "clientId": "atx-xxx" }
    ]
  },
  "phone_tasks": {          // mock batchTasks 数据源，key 为手机号
    "138...": [...]
  }
}
```
