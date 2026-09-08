# YunXi Next 开发路线图

> 版本：v1.3
> 工作分支：`codex/complete-plugin-runtime`（发布提交以最终门禁记录为准）
> 制定日期：2026-08-22；更新日期：2026-09-08
> 状态：Rust Cordis 元内核、Agent spine、进程插件、CLI/TUI/Web、管理面、
> 本地 Multi-agent、Voice sidecar、Weixin iLink 和安全迁移已形成发布候选；
> 真实设备/账号/媒体、远程多机和 OS 沙箱仍属于外部或平台验收

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
| Cordis core / runtime | 基础版已接入 | `Context`、`Service`、`inject`、`Event`、`Effect`、Fiber 生命周期，以及静态插件注册、开关和失败隔离；动态包生命周期由 Plugin Host 承担，任意 `cdylib`/WASM 仍未开放。 |
| Agent spine | 默认执行链已接入 | 独立 Rust loop、bounded session/context/model/tool seams、cancellation、budget 和 fail-closed approval continuation 已接入 CLI/Web；`ChatSession` 保留应用生命周期、持久化和 Web projection，不再承担第二套模型工具循环。 |
| Kernel / Plugin Host | 已接入 | 进程监督、握手、能力路由、崩溃隔离、兄弟插件存活，以及每次启用最多 3 次的 refresh 驱动自动恢复。 |
| Model Chat | 基础版已接入 | OpenAI/DeepSeek 兼容 Chat Completions；有界 SSE 流式适配已接入 CLI REPL/TUI、`run --jsonl` 和 WebHost 的增量事件路径。 |
| Context / Persona | 基础版已接入 | `AGENTS.md`、Persona profile 和 `soul.txt` 的受限读取与组合。 |
| Memory | 本地产品路径已接入 | 召回、规则提取、隐私过滤、去重、待审核写入及 status/list/show/search/approve/reject/delete/clear/on/off 均通过独立进程 Host facade；无风险默认开启。 |
| Storage | 基础版已接入 | 会话 append/list/load/resume 和旧记录只读投影。 |
| Companion | 本地产品路径已接入 | 确定性语气/跟进决策及 status/check/history/clear/on/off 均通过独立进程 Host facade；无风险默认开启。 |
| Mailbox / Scheduler | 基础版已接入 | 加密信箱和受限主动计划；跟随 Companion 默认关闭。 |
| Shell / Patch | 基础版已接入 | 独立进程、Host 审批、workspace grant、超时和输出限制；默认关闭。 |
| File search / view | 基础版已接入 | 独立只读进程、`tool.files@1`、workspace read grant、路径和内容大小限制；默认关闭。 |
| MCP stdio/HTTP bridge | 基础版已接入 | 独立二层进程、MCP initialize/list/call/cancel、JSON/SSE、session id、动态工具投影、Host approval 和崩溃恢复；默认关闭。HTTP 需要显式 network scope，Secret 只通过 reference grant 发放。 |
| Skills | 本地产品路径已接入 | 独立发现/上下文、禁用过滤、惰性 metadata-only 声明，以及显式启用、固定 allowlist、Host 审批、ActionGrant、取消和隔离进程的可执行 action；默认关闭。 |
| Multi-agent | 本地产品路径已接入 | 独立协调进程、父子图、预算、持久化、精确子工具 grants、独立子模型、Web 并行 continuable worker、模型选择、流事件、即时中断、会话重挂恢复和兄弟故障隔离；默认关闭，远程多机不在本地范围。 |
| Composition | 基础版已接入 | Profile、Bundle、Layer 和插件 inventory 已有；settings crate 的 15 个内置可选键全部进入 CLI/Web 启动组合，动态插件目录支持受约束的包发现/重载；任意第三方 bundle 配置仍未开放。 |
| Voice / Weixin | 本地适配边界已接入 | 同一 Host 进程插件可在 loopback 与显式 Voice JSONL sidecar / Weixin HTTPS iLink 间选择；Device/Network/Secret grants、doctor/login/serve/send/reply、取消、重启、加密存储和失败回退均有自动测试；真实硬件/账号/媒体仍需人工证据。 |
| Web | 本地产品路径已接入 | 固定 dsh Web 客户端、实时多会话、取消/审批、持久 SSE cursor、Memory/Persona/Relationship/Mailbox、Multi-agent、Voice/Weixin 和 Settings > Plugins 开关共用同一 Host；仍是 loopback 单机未认证服务。 |

当前组合策略：Model/Agent spine 必开；Context、Persona、Memory、Companion、
Storage 属于无外部副作用的安全默认；Mailbox、Scheduler、Shell、Patch、Files、
MCP、Skills、Multi-agent、Voice、Weixin 默认关闭。Web 当前写入
`YUNXI_NEXT_HOME\settings.json`；WebHost 在写入后重建当前 Host，独立 CLI
在下一次 Host 启动时应用；`settings.plugins`
的显式值优先于旧 capability 环境/文件设置，后者作为兼容性回退入口。
Voice/Weixin 开关会建立对应的隔离进程和路由；开启即获得该插件声明的 Host
grants，关闭即停止进程并撤销路由。只有显式 sidecar/iLink 配置才选择外部适配器，
其本地可用性仍不等于真实设备或账号已完成生产验收。

### 当前完成边界

本基线已经完成：Rust Cordis 核心原语、默认 Agent spine、进程级插件隔离、
内置能力的启停与故障恢复、受约束的动态 Rust 可执行包发现/依赖排序/重载/卸载、
CLI/TUI/Web 管理命令、插件开关、Voice/Weixin 可替换适配边界。上述能力均有
针对协议边界、坏帧、崩溃、超时、禁用和兄弟存活的测试。

仍未由仓库自动完成的是：真实麦克风/扬声器/语音 Provider、真实 Weixin
账号与媒体字节、OS keychain/HSM、操作系统级 CPU/内存/句柄/文件/网络沙箱、
远程多机调度，以及认证后的非 loopback 多客户端 Web。上述外部边界不能由
fixture、库层接口、`production_ready` 字段或静态 inventory 视为已验收。

### 发布验收门禁

每次发布候选必须从 `D:\YunXi Next` 执行
`scripts\acceptance-audit.ps1`。门禁只构建并安装 `yunxi-next.exe` 到唯一的
私有临时目录，先后读取旧版 `D:\Apps\YunXi Agent\bin\yunxi.exe` 的保护性
SHA-256，并对 `D:\YunXi Agent` 做只读指纹核对。它不得启动旧版，不得写旧版
源码或二进制，不得修改 PATH、注册表、Windows 服务或当前 checkout。

自动证据必须包括 PowerShell 语法/安全检查、`cargo fmt --check`、严格
Clippy、workspace tests、release build/install、CLI/Web/ConPTY smoke，以及
插件坏帧、崩溃、超时、三次重启耗尽、禁用/重新启用、动态替换、失败替换
恢复、卸载归属和 migration rollback 的精确测试结果。`-DryRun` 只验证范围
和旧版 SHA-256；任何跳过项都不能被记录为发布通过。

真实 Voice 设备/sidecar 和 Weixin 账号/网络/媒体链路只允许列入人工联调
记录。fixture、loopback、静态 inventory 和 `production_ready` 字段不能代替
这些外部证据。

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
`yunxi-multi-agent` 独立进程持久化父子图、转录、预算和有界事件。协调进程重启会
先把旧的 Running 分支标成可恢复状态；Web 根会话重新挂载时，Host 会用新的精确
grant 重建可续写 worker，已取消或已失败的分支不会被错误重启。`P4-02` 已把
`agent.spawn/list/message/interrupt` 接入模型工具目录，spawn/message 复用 Host
approval，每次子回合启动单独的 Model plugin 进程，且不向子进程发放父会话之外的
工具目录或 Provider credential 协议字段。

**当前进展：** `P4-03` 已完成 WebHost 的受限 continuable worker 切片：dsh
`subagent.list` / `subagent.history` 能读取真实父子图、子会话状态和有界 transcript，
`subagent.prompt` 可续写，`subagent.interrupt` 可中断当前 Web child worker，多个
worker 受 Host 上限约束并通过 mux 发送子模型增量和状态事件。`session.list` 会携带
`origin: subagent` 与 `parentSessionId`。查询使用只读 grant，协调器重启后的运行中
分支会在只读视图中降级为失败，不阻断历史查看。

**当前边界：** CLI 模型工具仍以有界 one-shot 子回合为主；Web 已支持独立的
continuable worker、精确的 file/patch 工具 grant、模型选择、并行执行、增量事件、
即时中断和根会话重挂恢复。当前自动证据覆盖有界双 worker 的并行与兄弟隔离，尚不
声称无限并发或远程多机调度。Web worker 的取消会立即撤销本地任务并关闭对应
provider/process；远端 HTTP 服务本身是否在网络边界外停止，仍取决于 provider 的
协作取消语义。

**退出门槛：** 子 Agent 不能继承超出父任务的 grant；子 Agent 崩溃、超时或
取消只结束对应分支；主会话、Kernel 和同级 Agent 仍可继续工作。

当前本地阶段已通过 grant 子集、可执行子工具、失败分支、Web worker 中断、并行模型
选择、根会话恢复和同级存活测试；远程调度与非 loopback 多租户服务仍明确排除在本地
发布范围之外。

### Phase 5：Voice 和 Weixin Channel

**目标：** 把语音和微信做成可替换的媒体/渠道适配器，文字聊天始终保持
独立可用。

**当前基线：** `yunxi-voice` 和 `yunxi-weixin` 已接入同一 Plugin Host 路径，
并完成确定性 loopback 与显式外部适配器的本地边界。两者继续复用现有 Plugin Host、
grant、审批和故障隔离边界；真实设备、账号和媒体仍只由人工联调确认。

**实现包：** `yunxi-voice`、`yunxi-weixin`。

**交付物：**

- `voice.transcribe@1`：有界音频、partial/final transcript、取消、背压。
- `voice.synthesize@1`：有界文本、音频分块、取消、背压。
- 麦克风、扬声器、设备和语音 Runtime URL 均由 Host grants 控制；默认不
  持久化原始音频。
- 微信 login/status/doctor/serve/pair/session/logout 和文字收发。
- 微信语音输入复用 Voice plugin 和同一个 YunXi session；渠道不嵌入第二套
  Agent runtime。

本地 Host 路径在没有外部配置时使用 bounded loopback；Voice 在显式配置
`YUNXI_VOICE_SIDECAR_PROGRAM` 后切换到独立 JSONL sidecar，并只发放 `Device`
grant；Weixin 在显式 production 配置完整时切换到 HTTPS iLink、加密 SecretStore
和非阻塞 long-poll，否则使用确定性 loopback/fallback。内置 fixture 仍只用于
协议回归，不能替代真实设备、账号、签名、媒体和网络证据。

**退出门槛：** 设备拒绝、服务超时、坏音频、插件崩溃、取消和背压都有可见
回退；微信网关或 Voice 失败时，CLI 文本聊天仍能继续。

### Phase 6：旧版管理和交互 parity

**目标：** 在核心能力稳定后补齐旧版 YunXi 的用户可见管理面。

**交付物：**

- Memory provider extraction、搜索、详情、删除、清空、批量审核和旧邮箱/旧
  状态迁移。
- Session show/history/rollout/graph/fork/archive/pin 等命令和完整旧事件投影。
- Persona、Companion、Controls 的持久化开关、历史、审计和清理命令。
- Model 的完整旧版输出 parity、`--json`、结构化输出以及必要的 `--cwd`、
  provider/model、approval/sandbox 配置兼容层；CLI/Web 的本地增量 streaming 和
  `--jsonl` 事件路径已属于当前基线，不再作为未实现能力记录。
- TUI 已实现首版 parity：由本地 Host facade 驱动流式回合、取消、后端切换，
  并提供旧版常用的会话、模型、provider、插件、Memory、Persona、Companion、
  Controls、Voice 和迁移管理入口；仍需后续补齐语音设备交互与完整 token 计费
  展示，不能据此宣称与旧版全部功能等价。

**退出门槛：** 旧版数据仍只读可回退；新命令不会覆盖旧文件；结构化输出有
固定 fixture；streaming 和交互取消不会留下插件进程或半写状态。

## 4. 明确暂缓

以下内容不进入上述主线，除非后续单独批准产品设计和安全边界：

- Desktop app、Cloud Tasks、SDK packaging。
- update、doctor、completion、marketplace。
- 完整 app-server、remote-control 和云端任务面。
- `yunxi-agent-eval` 运行时能力；它只作为开发期 acceptance fixtures。
- 操作系统级文件、网络、CPU、内存沙箱。当前 workspace grant 不能被
  误称为系统沙箱；Plugin Host 的三次有界恢复不等于系统级资源隔离。

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
   和 dsh Plugins 能力开关；内置开关通过 Host 重建应用，动态插件目录在刷新边界
   重扫并支持受控替换/卸载，不宣称单插件原地热卸载。
7. `P4-01` 已完成：冻结多智能体协议、grant 子集、预算、父子图、持久化和重启恢复。
8. `P4-02` 基线已完成：接入 Host 审批、独立子 Model 进程及
   `agent.spawn/list/message/interrupt` 模型工具。
9. `P4-03` 已完成本地 Web worker 闭环：后台 worker、增量事件、bounded interrupt、
     `subagent.list/history/prompt/interrupt`、精确子工具 grant、独立模型选择、并行
     执行和根会话重挂恢复均已接入；远程调度和无限并发不属于本地发布范围。
10. `P5-01` 已完成 Host Voice 适配边界：loopback、显式 JSONL sidecar、Device grant、
     设备/播放/保存/取消/背压/重启/文字降级均有本地测试；真实硬件和 provider 仍待人工。
11. `P5-02` 已完成 Host Weixin 适配边界：loopback、显式 HTTPS iLink、SecretStore、
     login/status/doctor/serve/pair/session/logout、long-poll、幂等队列和 Agent bridge
     均已接入；真实账号、网络签名、加密和媒体仍待人工。
12. `P6-01` 已完成首版：Agent spine 已成为 CLI/Web 默认模型工具编排路径；
      Cordis service discovery 和应用钩子仍继续收敛，包式动态插件装载已进入
      Plugin Host 基线。

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
| `P3-05` 持久化能力设置 | 已完成 | `yunxi-settings` 提供 64 KiB 上限、严格 schema、revision fence、备份恢复和同目录原子替换；WebHost 当前允许 `yunxi-capabilities` 的 15 个 launch-wired 布尔字段，成功写入后重建当前 Host 并发送 Host event；dsh Plugins 页显示 composition-scoped switch，Model 保持只读；真实 Host 重建测试验证 disabled-no-launch、无路由和聊天存活 |

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

### 历史执行记录：2026-09-06

| 项目 | 状态 | 证据 |
| --- | --- | --- |
| Cordis Rust foundation | 基线已完成 | `yunxi-cordis-core` 覆盖 scoped Context/Service、typed Event、Effect、Fiber 和 panic-contained callbacks；`yunxi-cordis-runtime` 覆盖 static registry、manifest policy、enable/disable、dependency resolution、local failure snapshots；两者均不负责进程/IPC。 |
| Agent spine approval slice | 默认路径已完成 | `yunxi-agent-spine` 覆盖 bounded session、model/context/tool seams、loop budgets、cancellation、`AwaitingApproval` continuation、approval decision validation 和 denied tool results；CLI 默认 Chat/Web turn 已通过 spine-backed Host adapter，兼容循环仅由显式环境变量启用。 |
| Voice process fixture | 当日历史基线 | 当日记录只覆盖 process fixture；后续记录已补齐 Host loopback/sidecar、Device grant 和本地失败回退。 |
| Weixin process fixture | 当日历史基线 | 当日记录只覆盖 process fixture；后续记录已补齐 Host loopback/iLink、SecretStore、poll worker 和管理边界。 |

### 执行记录：2026-09-08

| 项目 | 状态 | 证据 |
| --- | --- | --- |
| Web Multi-agent worker | 本地闭环已完成 | `web_host.rs` 覆盖持久 Running worker 在根会话重挂时自动恢复、两个 worker 并行使用独立模型、目标子 Agent 即时中断且兄弟仍可用；子工具目录按父级 grant 精确求交。 |
| Voice / Weixin Host boundary | 本地适配边界已完成 | Voice Host 覆盖 loopback、显式 sidecar、Device grant、音频文件适配和文字回退；Weixin Host 覆盖 loopback、显式 iLink、SecretStore、long-poll、幂等队列和管理生命周期。真实设备/账号仍按外部清单人工验收。 |
| Release gate correction | 定向通过 | `acceptance-audit.ps1` 的 ConPTY 使用空标准句柄并由独立线程排空 UTF-8 管道；定向审计已通过 release 安装、CLI/Web smoke、ConPTY 和旧版 SHA-256/树指纹保护。 |

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
可执行路径或任意插件配置；内置开关更新通过 Host 重建应用，动态包目录在刷新边界
重扫并支持生成代际安全的替换和卸载，不宣称运行中单插件原地热卸载。

Phase 4 当前本地闭环已完成：协调状态和子模型都位于受监督的独立进程，Agent grant
不能超过父级，单个子模型 API 失败只结束对应分支。WebHost 支持 continuable child
worker、增量事件、活动 worker 中断、精确 file/patch grant、模型选择、并行执行和
根会话重挂恢复；CLI 模型工具仍按同步 one-shot 子回合执行，远程多机不在本地范围。

Cordis runtime 当前是 trusted bootstrap，Agent spine 已成为 CLI/Web 的默认
模型工具编排入口；`ChatSession` 仍负责应用钩子和投影。这不代表生产 Web
服务已经完成。当前没有认证和非 loopback 监听策略，WebHost 虽支持受限独立会话、
子 worker 增量事件和活动中断，但不提供真正的多客户端 fan-out、远端调度或完整
dsh parity。继续推进后续阶段时仍应保持旧版 fallback、文字 CLI 和 trusted kernel
的独立验证。
