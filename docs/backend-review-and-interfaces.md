# AutomateX Rust Backend Review

## Scope

This document reviews the Rust backend under `/backends/src` with emphasis on:

- startup flow
- data flow and state ownership
- memory / performance / concurrency / security bottlenecks
- production-readiness observations
- backend interface summary for HTTP and MQTT

## Architecture Overview

Core modules:

- `lib.rs`: Tauri entrypoint, application state wiring, startup orchestration, invoke handler, shutdown cleanup
- `startup.rs`: first-run defaults, daily reset, startup task sync
- `commands/*`: Tauri command boundary exposed to frontend
- `engine/*`: task state machine, worker lifecycle, emit throttling
- `storage/*`: SQLite persistence on `deadpool-sqlite`
- `monitor/*`: ADB device tracking and property refresh
- `mqtt/*`: broker connection, topic routing, status events, heartbeat
- `http/*`: remote API abstraction (`MockApiClient` / `RealApiClient`)
- `task_provider/*`: task definition loading, cache rebuild, runtime task materialization
- `scrcpy/*`: device mirror control path

State ownership:

- `AppState.db`: persistent store for tasks, devices, settings, stats
- `AppState.engine`: runtime task state, worker map, emit path
- `AppState.http`: remote service access
- `AppState.mqtt`: broker session and publish/subscribe path
- `AppState.scrcpy`: mirror sessions

## Startup Flow

Primary startup sequence in `backends/src/lib.rs`:

1. Initialize SQLite database and application singletons.
2. Run sidecar integrity check and resolve the ADB server port.
3. Run daily reset and DB hygiene:
   - sync mock cache when no bound phone exists
   - cleanup orphan runs
   - mark all devices offline
   - cleanup stale task assignments
4. Ensure client ID and MQTT default configuration.
5. Initialize HTTP client:
   - `MockApiClient` if `api_base_url` is empty
   - `RealApiClient` otherwise
6. Initialize `TaskEngine`.
7. Run startup phone validation + task synchronization.
8. Start MQTT auto-connect in a background task.
9. Start ADB device monitor.
10. Wait for the first `device_ready` notification, then run remote device ownership sync.
11. Start MQTT listeners and the heartbeat timer.
12. Register Tauri invoke handlers.
13. On exit, stop scrcpy sessions and shut down the engine.

## Data Flow

### 1. Phone-based task sync

Frontend command:

- `sync_tasks_by_phones`

Backend flow:

1. Read `mqtt_client_id` and previous `synced_phones`.
2. Empty phone list:
   - local cleanup only
   - clear `synced_phones`
   - emit `account://sync-changed`
   - reload engine tasks
3. Non-empty phone list:
   - call HTTP `bind_phones`
   - resolve conflicts
   - call HTTP `batch_fetch_tasks`
   - batch upsert task defs into SQLite
   - delete stale local tasks
   - update `synced_phones`
   - reload engine and emit task snapshot

### 2. Runtime task execution

1. Frontend calls `engine_start_task`.
2. Engine selects a ready device from SQLite snapshot.
3. Engine creates round + run records in SQLite.
4. Engine spawns one worker task per executing task.
5. Worker wakes every 10 seconds and sends `TickRequest`.
6. Engine mutates task state serially in `event_loop.rs`.
7. Engine persists progress / task state / run state to SQLite.
8. Engine emits throttled `task://update` snapshots to frontend.

### 3. Device state updates

1. Monitor thread tracks ADB devices.
2. Device records are refreshed in SQLite.
3. Frontend gets `devices-changed`.
4. Engine may release tasks whose assigned device is now offline.

### 4. MQTT flow

1. `MqttManager` connects to broker and emits `mqtt-status`.
2. On `ConnAck`, client subscribes to:
   - `automatex/{client_id}/downstream/#`
   - `automatex/broadcast/#`
3. Incoming MQTT payloads are routed into Tauri events:
   - device kick
   - task reload
   - phone unbind
   - generic message
4. Backend listeners consume those Tauri events and mutate engine state.
5. Heartbeat publishes device and executing-task summary every 30 seconds.

## Implemented Hardening (2026-03-30)

- startup path now starts device monitoring before remote sync, and runs startup task sync plus MQTT auto-connect in parallel instead of serially
- startup device ownership sync no longer waits forever for `device_ready`; it degrades to the current DB device snapshot after a timeout
- `RealApiClient` now uses explicit connect/request timeouts, status-code validation, and a small retry budget for transient upstream failures
- production no longer silently falls back to `MockApiClient` or mock task definitions when `api_base_url` or task cache is empty unless `AUTOMATEX_ENABLE_MOCK=1`
- arbitrary ADB shell execution is now disabled by default in production unless `AUTOMATEX_ALLOW_DEVICE_SHELL=1`
- several device commands were moved from `spawn_blocking + std::process::Command` to async `tokio::process::Command`
- task pause / offline / risk / unbind / release-offline paths now close task rounds consistently and resume through `resume_round()`
- task progress subscription now uses a `watch` snapshot stream instead of per-subscriber 2-second polling
- MQTT router now validates required payload fields before routing control messages into engine mutations

## Production Review

### High Risk

1. `RealApiClient` has no explicit connect/read/request timeout, no retry policy, and no status-code validation.
   Impact:
   - startup sync can hang on slow upstreams
   - reload paths can stall worker-facing control flow
   - 5xx / 4xx responses are treated as JSON parse errors instead of classified failures
     Files:
   - `backends/src/http/real.rs`

2. Arbitrary shell execution is exposed to the frontend in production, with only a weak keyword denylist.
   Impact:
   - device-destructive commands can still bypass the filter
   - any renderer compromise becomes direct ADB command execution
     Files:
   - `backends/src/commands/device.rs`

3. Mock task definitions can become production fallback data when `api_base_url` is empty or local task cache is empty.
   Impact:
   - production nodes may boot with mock tasks
   - operational mistakes are hard to detect because startup remains “successful”
     Files:
   - `backends/src/lib.rs`
   - `backends/src/task_provider/mod.rs`

### Medium Risk

1. Error paths stop task runs without consistently closing task rounds.
   Affected flows:
   - device offline during tick
   - simulated risk-control path
   - release-offline handler
     Impact:
   - analytics drift
   - stale `running` rounds until later cleanup
     Files:
   - `backends/src/engine/event_loop.rs`

2. `subscribe_task_progress` is implemented as per-subscriber polling of the whole task snapshot every 2 seconds.
   Impact:
   - O(subscribers × tasks × cities) steady-state cost
   - repeated cloning and JSON allocation
   - poor scaling under multiple open dashboards
     Files:
   - `backends/src/commands/engine_cmd.rs`

3. Device ownership sync is blocked on `device_ready.notified()`.
   Impact:
   - if ADB tracking never produces the first notification, remote device ownership sync never runs
   - startup success appears partial with no explicit degraded-mode fallback
     Files:
   - `backends/src/lib.rs`

4. MQTT router trusts topic suffixes plus optional `source`/`ts` fields only.
   Impact:
   - no application-layer integrity/signature protection
   - malformed-but-valid payloads can still trigger engine-side mutations
     Files:
   - `backends/src/mqtt/router.rs`

### Low Risk / Observations

1. Database settings store secrets (`mqtt_password`) in plaintext SQLite.
2. Logging includes operational details such as task IDs, phone conflicts, and device identifiers.
3. Device monitor uses mixed threading models (`std::thread`, `spawn_blocking`, Tokio tasks), which is workable but harder to reason about during incidents.
4. Unused API methods (`report_progress`, `unbind_phones`) indicate integration drift between intended and actual production traffic.

## Performance Notes

Positive points:

- task runtime state is owned by a single event loop, which avoids lock contention
- task snapshot emit is throttled and hash-deduplicated
- bulk progress and task loading already avoid several N+1 query patterns
- SQLite WAL and connection pool hook are configured

Current hotspots:

- `subscribe_task_progress` polling loop
- full task reloads after sync and some MQTT events
- repeated `load_all_devices()` / `get_tasks()` snapshots in heartbeat and device release paths
- ADB monitor property collection under high device counts

## Security Notes

Recommended before production rollout:

1. Replace `execute_shell` with an allowlisted action model.
2. Move MQTT credentials out of renderer-readable settings or store them encrypted.
3. Add HTTP client timeouts, retry budget, and upstream response code checks.
4. Add topic-level authorization expectations and payload schema validation for MQTT control messages.
5. Split mock mode behind an explicit environment gate so production cannot silently fall back.

## HTTP Interface Summary

Trait: `backends/src/http/mod.rs::ApiClient`

### Phone Bind

- `POST /mttl_tools/v1/meituanTraffic/client/bind`
- request: `PhoneBindRequest`
  - `clientId`
  - `mobiles[]`
  - `forceBind`
- response: `PhoneBindResponse`
  - `conflicts[]`
    - `mobile`
    - `clientId`
  - `taskItems[]`

### Batch Tasks

- `POST /mttl_tools/v1/meituanTraffic/client/batchTasks`
- request: `taskIds[]`
- response: `data[]`
  - `taskId`
  - `taskName`
  - `intervalMinute`
  - `mobile`
  - `cityItems[]`
    - `cityName`
    - `pointName`
    - `keywords[]`
- single task refresh reuses this endpoint with one `taskId`

### Progress Report

- `POST /mttl_tools/v1/meituanTraffic/client/scan/upload`
- request: `ProgressReportRequest`
  - `clientId`
  - `taskId`
  - `taskName`
  - `cityName`
  - `keyword`
  - `deviceNo`
  - `roundNo`
  - `storeList[]`
  - `scanFinishedTime`
- response: `ApiResponse`
  - `data` may be `null`

### Phone Unbind

- `POST /mttl_tools/v1/meituanTraffic/client/unbind`
- request: `UnbindPhonesRequest`
  - `clientId`
  - `mobiles[]`
- response: `ApiResponse`
  - `data` may be `null`

## MQTT Interface Summary

Topic helper base:

- client scoped: `automatex/{client_id}/{suffix}`
- broadcast scoped: `automatex/{suffix}`

### Upstream topics

- `upstream/device/online`
- `upstream/device/offline`
- `upstream/heartbeat`
- `upstream/task/event`
- `upstream/offline` (LWT)

Current payloads explicitly used by backend:

- heartbeat publish payload
  - `ts`
  - `devices[]` (device hardware serials)
  - `tasks_executing[]`

### Downstream topics

- `downstream/device/kick`
  - routed to Tauri event: `mqtt-device-kick`
  - expected payload field: `hw_serials[]`

- `downstream/task/reload`
  - routed to Tauri event: `mqtt-task-reload`
  - expected payload fields:
    - `action`: `reload_all | reload_task | delete_task`
    - `task_id?`

- `downstream/phones/unbind`
  - routed to Tauri event: `mqtt-phones-unbind`
  - expected payload field: `phones[]`

### Broadcast topics

- `broadcast/task/update`
  - currently mapped to the same Tauri event as task reload

### Router-side payload filters

- drop message when JSON parse fails
- drop message when `ts < connect_ts`
- drop message when `source == current_client_id`

## Recommended Next Steps

1. Add hardened `reqwest::ClientBuilder` with connect timeout, total timeout, user-agent, and retry wrapper.
2. Remove production mock fallback unless explicitly enabled by environment.
3. Replace `subscribe_task_progress` polling with engine-side fanout from existing `emit_update`.
4. Normalize round finalization in every non-success terminal path.
5. Replace raw shell execution with typed ADB operations.
6. Add a startup health summary event that reports:
   - HTTP mode
   - MQTT status
   - device monitor state
   - startup sync result
