# YunXi Next 开发路线图

> 版本：v1.2
> 基线提交：`0a73001`
> 制定日期：2026-08-22；更新日期：2026-08-28
> 状态：Phase 4 多智能体隔离基线已接入；异步并行和执行中即时中断仍在开发

这份文档是 YunXi Next 后续开发的排序、范围和完成判定。它不是功能愿望
清单。没有通过本文件规定的验收门槛，能力只能标记为 `planned` 或
`baseline`，不能因为已经创建了 Rust crate 就宣称迁移完成。

## 1. 总目标

YunXi Next 最终应成为一个以 Rust trusted kernel 为中心、所有业务能力均可
插拔的 Agent Runtime：

1. 文字 CLI 永远是最小可用和故障回退路径。
2. 每个可选能力运行在独立进程中，插件崩溃只影响自己的路由。
3. 每个能力都有稳定的插件 id、版本化协议、明确授权和可观察失败状态。
4. Web、TUI、微信和语音都复用同一条 Host -> Plugin 路径，不能另起一套
   绕过审批、记忆或权限的运行时。
5. `D:\Yunxi Agent` 永远保持只读和可运行，作为旧版 fallback。

## 2. 当前基线

| 领域 | 当前状态 | 说明 |
| --- | --- | --- |
| Kernel / Plugin Host | 已接入 | 进程监督、握手、能力路由、崩溃隔离和兄弟插件存活测试已完成。 |
| Model Chat | 基础版已接入 | OpenAI/DeepSeek 兼容 Chat Completions；当前是完整响应，不是 token streaming。 |
| Context / Persona | 基础版已接入 | `AGENTS.md`、Persona profile 和 `soul.txt` 的受限读取与组合。 |
| Memory | 基础版已接入 | 召回、规则提取、隐私过滤、去重、待审核写入；默认关闭。 |
| Storage | 基础版已接入 | 会话 append/list/load/resume 和旧记录只读投影。 |
| Companion | 基础版已接入 | 确定性语气/跟进决策；默认关闭。 |
| Mailbox / Scheduler | 基础版已接入 | 加密信箱和受限主动计划；跟随 Companion 默认关闭。 |
| Shell / Patch | 基础版已接入 | 独立进程、Host 审批、workspace grant、超时和输出限制；默认关闭。 |
| File search / view | 基础版已接入 | 独立只读进程、`tool.files@1`、workspace read grant、路径和内容大小限制；默认关闭。 |
| MCP stdio/HTTP bridge | 基础版已接入 | 独立二层进程、MCP initialize/list/call/cancel、JSON/SSE、session id、动态工具投影、Host approval 和崩溃恢复；默认关闭。HTTP 需要显式 network scope，Secret 只通过 reference grant 发放。 |
| Skills | 基础版已接入 | 独立只读进程、受限发现/上下文、禁用过滤和 metadata-only 动态工具声明；默认关闭。 |
| Multi-agent | 基础版已接入 | 独立协调进程、父子图、预算、持久化、恢复和独立子模型进程；默认关闭。当前子回合同步执行，不宣称后台并行或执行中即时中断。 |
| Composition | 基础版已接入 | Profile、Bundle、Layer 和插件 inventory 已有；13 个可选能力支持版本化持久化开关，顺序和任意第三方 bundle 配置仍未开放。 |
| Web | Phase 3 本地基线已接入 | 固定 dsh Web 客户端、session create/history/prompt、审批响应、mux/host 事件、健康/inventory/session projection 和 Settings > Plugins 能力开关共用同一个 CLI Host；当前仍是 loopback、单 Host、完整响应模式。 |

当前默认开关由 `yunxi-settings` 定义：Context、Persona、Storage 开启；Memory、
Companion、Mailbox、Scheduler、Shell、Patch、Files、MCP、Skills、Multi-agent
关闭。Web 写入
`YUNXI_NEXT_HOME\settings.json` 并在下一次 Host 启动时应用；显式环境变量拥有
更高优先级，作为 CI 和故障恢复入口。

## 3. 开发排序

开发按依赖和风险排序。每次只推进一个主阶段；新阶段不能用“先写一个
placeholder”绕过上一阶段的验收。

### Phase 0：基线封版和契约校准

**目标：** 把当前最小内核变成可持续扩展的稳定基线。

**交付物：**

- 校准 `capability-migration.md` 的实际状态，补齐 Model、Shell、Patch 的
  manifest、grant 和失败测试记录。
- 固化 plugin inventory、disabled-no-launch、crash、timeout、malformed
  frame、API failure 的跨插件测试矩阵。
- 明确 `baseline`、`integrated`、`planned` 三种状态的使用规则。
- 保持当前 CLI、API 配置、旧目录只读兼容，不在此阶段引入 TUI、Web UI 或
  大型新依赖。

**退出门槛：** `cargo fmt --check`、严格 Clippy、全量测试通过；每个当前
插件均能证明关闭后不启动、崩溃后不影响 Model 和至少一个兄弟插件。

### Phase 1：模型工具调用和 Action Path

**目标：** 让模型能够在 Host 控制下使用工具，形成完整的
`model -> approval -> tool plugin -> tool result -> model` 闭环。

**交付物：**

- 在 `yunxi-protocol` 中增加有版本的 tool-call、tool-result、取消和最大
  轮次契约。
- 扩展 Model plugin，使工具声明和工具调用不会泄漏到 Kernel 内部实现。
- 将 Shell、Patch 接入模型自动调用；用户拒绝时不能产生副作用。
- 新增文件搜索、文件查看和用户输入等最小辅助工具，分别设定授权边界。
- 保留 `/shell`、`/patch` 手动入口作为诊断和回退路径。

**退出门槛：** fixture model 能完成至少一次工具调用；拒绝、超时、取消、
错误响应、插件崩溃和超出最大轮次均可见且可恢复；工具插件不能自行提高
approval、workspace、network 或 secret 权限。

### Phase 2：MCP 和 Skills

**目标：** 把旧版外部工具生态接入插件目录，而不是把 MCP 或 Skill 逻辑
重新塞回 CLI。

**建议包：** `yunxi-tool-mcp`、`yunxi-tool-skills`。

**交付物：**

- MCP stdio/HTTP Server 的注册、握手、工具发现、调用、取消和状态快照。
- Skill 的发现、元数据校验、受限指令注入和动态工具声明。
- 外部 Server、Skill 文件、网络和 Secret 均通过 Host grant 获得权限。
- `/plugins` 和后续 Web inventory 能显示 MCP/Skill 的独立生命周期。

**退出门槛：** MCP/Skill 任一实例崩溃或返回坏帧时，Model、Shell 和其他
插件继续可用；禁用后不启动、不注册路由、不向模型暴露工具。

### Phase 3：Web Gateway、dsh 前端和 Tag 设置

**目标：** 复用 dsh Web 的交互层，同时让插件开关成为真正可持久化的
用户设置。

**建议包和目录：** `yunxi-web-gateway`，以及独立的 `web/` 前端目录；
dsh 源码和 MIT 版权声明必须单独记录，不能混入 Rust Kernel 代码。

**交付物：**

- Rust Gateway 实现 dsh unary RPC、`events.mux`、`events.host`、健康状态、
  session projection 和 plugin inventory。
- 将 Composition profile 保存为受限配置，支持 Tag 的启用、禁用、顺序和
  bundle/profile overlay。
- Web Chat、插件状态、失败提示和基础设置均复用 CLI 使用的 Host。
- 禁用插件不加载代码；Web 断开、前端错误或可选插件失败不能影响文字 CLI。

**退出门槛：** dsh Web 客户端能完成启动、聊天、查看插件、切换插件和恢复
会话；浏览器请求不能绕过 Host approval 或直接携带 Provider credential。

### Phase 4：Multi-agent

**目标：** 将旧版多代理协调能力变成可监督的独立能力，而不是在主会话中
创建不可控的线程或任务。

**建议包：** `yunxi-multi-agent`。

**交付物：** Agent spawn/list/message/interrupt、父子图、最大深度、预算、
取消传播、子会话持久化和状态事件。

**当前基线：** `P4-01` 已冻结 `tool.multi-agent@1` typed contract，并由
`yunxi-multi-agent` 独立进程持久化父子图、转录、预算和有界事件；协调进程重启
只把遗留的 Running 分支标记为失败。`P4-02` 已把 `agent.spawn/list/message/interrupt`
接入模型工具目录，spawn/message 复用 Host approval，每次子回合启动单独的 Model
plugin 进程，且不向子进程发放父会话工具目录或 Provider credential 协议字段。

**当前进展：** `P4-03` 已先完成 Web 只读投影切片：dsh 现有的
`subagent.list` / `subagent.history` 能读取真实父子图、子会话状态和有界 transcript，
`session.list` 会携带 `origin: subagent` 与 `parentSessionId`。查询使用只读 grant，
协调器重启后的运行中分支会在只读视图中降级为失败，不阻断历史查看。

**待完成：** 后台并行 worker、执行中子模型请求的真实取消、子 Agent 工具 grant 和
模型选择。当前 `interrupt` 在回合之间递归更新持久化状态，不能终止已经发出的同步
HTTP 请求；Web 端暂只支持 one-shot 子会话查看，不支持 continuable 子会话续写。

**退出门槛：** 子 Agent 不能继承超出父任务的 grant；子 Agent 崩溃、超时或
取消只结束对应分支；主会话、Kernel 和同级 Agent 仍可继续工作。

当前已通过 grant 子集、失败分支和同级存活测试，但在执行中即时取消和并行调度
完成前，Phase 4 仍保持 `baseline`，不标记为完整 `integrated`。

### Phase 5：Voice 和 Weixin Channel

**目标：** 把语音和微信做成可替换的媒体/渠道适配器，文字聊天始终保持
独立可用。

**建议包：** `yunxi-voice`、`yunxi-channel-weixin`。

**交付物：**

- `voice.transcribe@1`：有界音频、partial/final transcript、取消、背压。
- `voice.synthesize@1`：有界文本、音频分块、取消、背压。
- 麦克风、扬声器、设备和语音 Runtime URL 均由 Host grants 控制；默认不
  持久化原始音频。
- 微信 login/status/doctor/serve/pair/session/logout 和文字收发。
- 微信语音输入复用 Voice plugin 和同一个 YunXi session；渠道不嵌入第二套
  Agent runtime。

**退出门槛：** 设备拒绝、服务超时、坏音频、插件崩溃、取消和背压都有可见
回退；微信网关或 Voice 失败时，CLI 文本聊天仍能继续。

### Phase 6：旧版管理和交互 parity

**目标：** 在核心能力稳定后补齐旧版 YunXi 的用户可见管理面。

**交付物：**

- Memory provider extraction、搜索、详情、删除、清空、批量审核和旧邮箱/旧
  状态迁移。
- Session show/history/rollout/graph/fork/archive/pin 等命令和完整旧事件投影。
- Persona、Companion、Controls 的持久化开关、历史、审计和清理命令。
- Model token streaming、`--json`、`--jsonl`、结构化输出以及必要的
  `--cwd`、provider/model、approval/sandbox 配置兼容层。
- 在 Web 稳定后再实现完整 TUI；TUI 只调用现有 Host facade。

**退出门槛：** 旧版数据仍只读可回退；新命令不会覆盖旧文件；结构化输出有
固定 fixture；streaming 和交互取消不会留下插件进程或半写状态。

## 4. 明确暂缓

以下内容不进入上述主线，除非后续单独批准产品设计和安全边界：

- Desktop app、Cloud Tasks、SDK packaging。
- update、doctor、completion、marketplace。
- 完整 app-server、remote-control 和云端任务面。
- `yunxi-agent-eval` 运行时能力；它只作为开发期 acceptance fixtures。
- 操作系统级文件、网络、CPU、内存沙箱，以及自动重启和 backoff。它们需要
  单独的平台方案，不能把当前 workspace grant 误称为系统沙箱。

## 5. 通用完成门槛

任何能力只有同时满足以下条件，才能把迁移账本从 `planned` 改为
`integrated`：

1. 独立进程、稳定 plugin id 和非零版本化 capability。
2. 完整的 typed request/response fixture。
3. crash、timeout、坏帧、API failure 的隔离和可见报告。
4. disabled 状态不启动进程、不注册路由、不暴露模型工具。
5. 文件、网络、Secret、设备和 approval grant 明确且由 Host 发放。
6. Kernel、Model 和至少一个兄弟插件在故障测试后仍健康。
7. crate、`src/`、`tests/` 或新增目录均有职责 README。
8. 文档、迁移账本、命令示例和验证命令同步更新。

每个开发批次结束前必须运行：

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
```

## 6. 当前执行队列

Phase 0 和 Phase 1 已按下列顺序完成。后续仍保持一次只推进一个主阶段：

1. `P0-01`：补齐 Model、Shell、Patch 的 manifest/grant/失败测试记录，并
   校准迁移账本。
2. `P0-02`：建立统一的 capability acceptance fixture 和 disabled-no-launch
   测试模板。
3. `P1-01`：设计并冻结 tool-call/tool-result/approval/cancel 协议。
4. `P1-02`：把 fixture model、Shell、Patch 串成最小自动工具闭环。
5. `P1-03`：增加文件搜索和文件查看插件，验证只读 grant。
6. `P1-04`：完成模型多轮工具上限、取消、失败回退和 CLI 可观察输出。

下一执行队列：

1. `P2-04` 已完成：MCP 调用取消、HTTP transport 和更细粒度 network/secret grant。
2. `P3-01` 已完成：建立独立 Web Gateway facade、健康/inventory/session list
   projection 和有界 `events.mux`/`events.host` 内存 carrier。
3. `P3-02` 已完成：实现独立 HTTP/SSE carrier，固定 `/api/<method>` 路由和
   `client-request`/`server-response` body bound。
4. `P3-03` 已完成：把 session history/prompt/approval RPC 和事件接入 Web Host，
   并保持 CLI 与 Web 共用同一个 Host facade。
5. `P3-04` 已完成：`yunxi-next web` 嵌入固定 dsh Web bundle，覆盖启动、聊天、
   会话恢复、插件状态、Host 事件和审批。
6. `P3-05` 已完成：新增 `yunxi-settings`，接入 `settings.describe/update/replace/mutate`
   和 dsh Plugins 能力开关；写入只决定下一次 Host 组合，不热卸载运行中插件。
7. `P4-01` 已完成：冻结多智能体协议、grant 子集、预算、父子图、持久化和重启恢复。
8. `P4-02` 基线已完成：接入 Host 审批、独立子 Model 进程及
   `agent.spawn/list/message/interrupt` 模型工具。
9. `P4-03` 进行中：增加后台并行、执行中即时取消、Web 分支视图和受限子工具授权。

### 执行记录：2026-08-23

| 项目 | 状态 | 证据 |
| --- | --- | --- |
| `P0-01` manifest/grant 契约与迁移账本 | 已完成 | `yunxi-protocol` manifest handshake、宿主 required-grant 校验、Model/Shell/Patch 声明和账本说明 |
| `P0-02` acceptance fixture 与 disabled-no-launch | 已完成 | `yunxi-plugin-host/tests/process_runtime.rs` 覆盖 manifest 缺失、坏帧、崩溃、超时；CLI fixture 明确关闭全部可选能力 |
| Phase 0 质量门槛 | 已完成 | `cargo fmt --all -- --check`、严格 Clippy、workspace 全量测试通过 |
| `P1-01` tool-call/tool-result/approval/cancel 协议 | 已完成 | `yunxi-protocol/src/tool_calls.rs` 冻结版本、关联 id、grant、取消、大小和轮次上限，并有 wire round-trip/坏值测试 |
| `P1-02` 最小自动工具闭环 | 已完成 | Model 可选 tool catalog/call wire、CLI Host approval continuation、Shell/Patch result 回送；17 个 CLI 集成测试覆盖批准、拒绝、取消、超时、插件拒绝和轮次恢复 |
| `P1-03` 只读文件搜索/查看 | 已完成 | `yunxi-tool-files` 独立进程、`tool.files@1` typed search/read、required `WorkspaceRead` manifest grant；CLI E2E 覆盖工具暴露、搜索、查看、无审批/无写权限，executor 覆盖路径边界和读取上限 |
| `P1-04` 多轮上限、取消、失败回退与可观察输出 | 已完成 | 8 轮/8 调用上限、`/cancel` 无副作用、超时/拒绝回送模型并恢复后续回合；CLI warning 对工具失败去重可见，18 个 CLI 集成测试全部通过 |
| `P2-01` MCP stdio 注册、握手、发现和失败边界 | 已完成 | `yunxi-tool-mcp` 有界 JSON-RPC stdio、显式环境白名单、请求 ID/超时/坏帧/崩溃测试；CLI disabled、发现和 MCP 崩溃隔离测试通过 |
| `P2-02` MCP 工具目录投影、调用和状态快照 | 已完成 | `mcp.<server>.<tool>` 动态目录、Host approval、typed call/list/status、工具失败回送模型；MCP 退出不影响模型路由 |
| `P2-03` 受限 Skills 元数据和动态工具声明 | 已完成 | `yunxi-tool-skills` 独立进程、`tool.skills@1:list/context/status`、workspace 内路径限制、环境清理、禁用过滤和 `skill.<id>.<tool>` metadata-only 投影；上下文崩溃不影响模型，声明调用无审批且无副作用 |
| `P2-04` MCP 取消、HTTP transport 和细粒度授权 | 已完成 | `yunxi-tool-mcp` 支持 opt-in HTTP/HTTPS JSON/SSE、`Mcp-Session-Id`、超时后的 `notifications/cancelled`、Host-issued exact scheme/host/port network scope、Secret reference grant、header/diagnostic redaction；HTTP fixture 8 项、CLI chat stack 24 项通过 |
| `P3-01` Web Gateway facade 和基础 projection | 已完成 | 新增 `yunxi-web-gateway`，支持 `health.status`、`pluginInventory/list`、`session.list`、结构化 unsupported/invalid failure、独立 mux/host 有界事件队列；`yunxi-cli::WebHost` 刷新同一 CLI Host 的状态、inventory 和 storage session projection；Gateway 5 项、CLI chat stack 24 项通过 |
| `P3-02` 独立 HTTP/SSE carrier | 已完成 | `yunxi-web-gateway` 新增有界 HTTP/1.1 parser、固定 `/api` unary 路由、JSON content-type/size/framing 校验、`events.mux`/`events.host` SSE carrier、`ShutdownToken` 和 TCP serving helper；9 项 carrier 集成测试覆盖 malformed envelope、channel isolation、wire bytes 和 loopback TCP；Gateway 与 CLI 定向 Clippy 通过 |
| `P3-03` WebHost session RPC 与审批事件 | 已完成 | `WebHost` 接入 `session.create/history/prompt`、`POST /api/respond`、审批 rpcId 回显校验、session/event 与 approval requested/resolved 事件；carrier 测试覆盖 session 路由、receipt、坏响应和既有 HTTP 边界 |
| `P3-04` dsh Web 服务入口与共享 Host 生命周期 | 已完成 | `yunxi-next web` 默认 loopback 绑定、`--bind` 参数、标准输入 EOF 优雅关闭；固定上游 commit 的 dsh shell、字体、语言包和 42 个客户端 bundle 由 Rust executable 嵌入；YunXi adapter 只替换 bounded SSE carrier，并保留真实 session、history、approval 和 inventory 路径 |
| `P3-05` 持久化能力设置 | 已完成 | `yunxi-settings` 提供 64 KiB 上限、严格 schema、revision fence、备份恢复和同目录原子替换；WebHost 只允许 `yunxi-capabilities` 的 13 个已知布尔字段，成功写入发送 Host event；dsh Plugins 页显示 restart-scoped switch，Model 保持只读；真实 Host 重启测试验证 disabled-no-launch、无路由和聊天存活 |

### 执行记录：2026-08-28

| 项目 | 状态 | 证据 |
| --- | --- | --- |
| `P4-01` 多智能体协议与协调进程 | 已完成 | 新增 `tool.multi-agent@1`、`AgentDelegationGrant`、父子 grant 子集与固定预算；`yunxi-multi-agent` 在 workspace 内持久化图、转录和有界事件，使用备份可恢复原子替换；进程测试覆盖重启恢复且持久化数据不含 Provider credential |
| `P4-02` Host 工具闭环与子模型隔离 | 基线已完成 | `agent.spawn/list/message/interrupt` 进入有界工具目录；spawn/message 经过既有审批；每个子回合使用单独 Model plugin 进程和独立转录，子失败写入对应分支并保持父模型可继续；禁用矩阵验证不启动、不注册、不暴露工具 |
| `P4-03` Web 子会话投影切片 | 部分完成 | 新增只读 `inspect` 协议操作；WebHost 接入 dsh `subagent.list/history`，投影 one-shot 子 Agent 的目录、状态、父子 lineage 和分页历史；进程级测试覆盖审批、隔离子模型、`session.list` 来源字段和历史读取 |

### 执行记录：2026-09-05

| 项目 | 状态 | 证据 |
| --- | --- | --- |
| `P4-03` Web 子 Agent 目录与历史 | 已完成首个切片 | `AgentInspectRequest/Result` 与协调器 `inspect` 仅返回受限 transcript；`subagent.list` 支持根及已知子父级，`subagent.history` 输出 dsh `HistoryEntry` 分页；只读重启恢复、HTTP 路由和 `yunxi-cli/tests/web_host.rs` 进程回归均通过 |

本批次已改变 Model 的可选工具运行时行为，但没有把 manifest 声明误当作
实际 Secret broker 或操作系统沙箱。Phase 1 的文字 Action Path 已完成
第一版；`/shell` 和 `/patch` 手动入口继续作为诊断和回退路径。MCP stdio/HTTP
和 Skills 只读接入已形成 Phase 2 基线：HTTP 默认关闭，必须由 Host 提供精确
network scope；Secret grant 只携带 reference，不能替代 Secret broker。HTTP
transport 超时会尽力发送远端 cancellation notification，但这不等同于远端
服务一定已经停止执行。所有新路径仍必须保留现有 Host grant 和故障隔离边界。

Phase 3 的本地单 Host 基线已完成：固定 dsh 浏览器产品、CLI WebHost、session
prompt/history/approval RPC、独立 HTTP/SSE carrier、插件 inventory 和持久化能力
开关均通过同一 Host 路径。设置只包含可选能力布尔值，不能写入 Provider Secret、
可执行路径或任意插件配置；运行中不热卸载，重启后 disabled 插件不启动且没有路由。

Phase 4 已形成第一版隔离基线：协调状态和子模型都位于受监督的独立进程，
Agent grant 不能超过父级，单个子模型 API 失败只结束对应分支。当前仍按同步回合
执行，不具备后台并行和执行中即时取消，因此不能视为多智能体阶段全部完成。

这不代表生产 Web 服务已经完成。当前没有认证和非 loopback 监听策略，WebHost
仍是文本内容、单实例活动会话、完整响应和轮询式事件，不宣称 token streaming、
多会话并发或真正多客户端 fan-out。继续推进 Phase 4 时仍应保持旧版 fallback、
文字 CLI 和 trusted kernel 的独立验证。
