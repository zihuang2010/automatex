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
    "bound": ["138..."],    // 绑定成功的手机号
    "conflicts": [          // 冲突列表
      { "phone": "139...", "current_client": "atx-xxx" }
    ]
  },
  "fetch_tasks": {          // mock fetch_tasks_by_phones 响应
    "use_mock_tasks": true, // 是否使用 mock_tasks.json
    "extra_tasks": [...]    // 附加的额外任务
  }
}
```
