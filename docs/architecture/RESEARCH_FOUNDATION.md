# Monday research foundation：代码能力与启用边界

本分支在现有 monorepo 内增加 `hft-research-platform`，将通用数据、研究谱系和计算生命周期与 CI 分离。它是可评审的基础代码，尚未完成生产切换。既有 Campaign、治理、采集和交易代码继续保留；本分支没有安装迁移、启用 backend、创建云资源或运行真实研究。

## 现状和职责

现有 CEX 科学入口仍是 `mission campaign-freeze → campaign-finalize → dispatch submit → campaign-execute`（`alpha-harness`）。Prediction Markets 保留独立的事件结算 evaluator；两者共享 Monday 的行情、风险和执行边界。`apps/live`、`tools/collector`、数据库和持续控制服务不能整体迁入临时 Sandbox。原生模型训练现为 CPU Burn ndarray，不能假设 AgentSandbox 支持 GPU、内存恢复或长期行情连接。

```mermaid
flowchart LR
  CI[CI: 源码验证 / scoped build / Clippy / 镜像 / release admission]
  BUILD[不可变 BuildArtifact: 二进制摘要 + OCI digest]
  CTRL[可信 Rust research service]
  PG[(PG: 实验 / Run / task / result / outbox)]
  CH[(CH: 持久 normalized hot data / SQL features / labels)]
  OSS[(OSS 经身份 broker / artifact gateway)]
  SESSION[AgentSandbox Session / 现有 Coding Agent]
  JOB[固定 Kubernetes 或 ACS Job: prepare / train / backtest]
  LIVE[常驻采集 / live / risk / OMS / execution]
  CI --> BUILD
  BUILD --> CTRL
  CTRL --> PG
  CTRL --> JOB
  JOB --> CH
  JOB --> OSS
  CTRL -->|独立字节回读 / 终态| OSS
  SESSION -.受控 research.submit/status/artifacts.-> CTRL
  PG -.显式 subscription / completion intent.-> SESSION
  CH -->|版本化只读 block| JOB
```

实线已有 Rust 实现或明确的外部存储接口；Session transport、原生治理导入器、OSS gateway 的生产部署、现有科学 worker 的新输入适配和灰度启用仍待接入。图没有把这些外部接口画成已部署资源。

| 负载 | 安放判断 | 当前启用条件 |
|---|---|---|
| live、risk、OMS、执行、长连接行情采集 | 保留常驻服务或专用主机 | 既有 runtime/collector Gate；新平台无订单依赖 |
| PG / CH / 对象存储 | 常驻持久服务 | 分别控制事务、热数据和原始归档/产物 |
| prepare / train / backtest | 固定命令 Job，允许 CPU ACS 弹性算力 | exact release + 科学 grant/预算 + backend acceptance + 输入 manifest |
| 研究对话与代码变异 | 有持久状态的 AgentSandbox Session | native state、workspace、审批、受控工具和恢复验收 |
| CI / CD | 验证、构建、发布证据与 admission | 不负责研究资源租期、启动、准入、停止、回执桥 |
| 运维 | 保留独立流程 | 真实资源、网络、RAM、发布或切换仍由单独授权和 Gate 控制 |

## Build → Run → Attempt，不在训练 Pod cold build

`src/build.rs` 定义 `BuildSpec` 和 `BuildArtifact`。Build 身份包含源码 commit 与 source manifest、Cargo.lock、工具链 distribution/compiler manifest、target、精确包/二进制/features/default-features、profile 和 profile bytes、Rust 编译参数、原生编译器/链接器/库环境、builder image。它不包含数据窗口、研究参数、seed、task ID 或 attempt。

Run 是固定科学调用：Experiment、BuildArtifact、配置摘要、命令、数据 manifest、seed、evaluator、evaluation protocol、fit identity。Run 同时验证其源码和镜像与引用的 Build 一致。Task 必须引用已注册 Run；同一 Run 最多一个计算 Task，Attempt 只重试原请求，不能变更产物或参数。多个参数 Run 可以引用同一 Build。

计算 Pod 直接调用 `/usr/local/bin/<已发布二进制>`；PG Run admission 拒绝 `cargo` 或任意 shell 作为直接入口。reconciler 每次 launch（包括 retry）读取已注册 Build，并从受控 artifact gateway 流式核验二进制大小/摘要，再检查租期、截止时间和撤销状态。缺失或错误产物不能触发 launch。OCI digest 与内含二进制的绑定来自独立的 trusted release verifier；通用 Agent API 无权出具这个证明。

`.github/scripts/build-research-release.sh` 使用精确 `-p` / `--bin` / `--features`，没有默认 `--workspace` / `--all-features`。共享发布 bundle 兼顾已有 CEX、Prediction 和新控制二进制；不同科学变异的 BuildSpec 可以缩小到实际科学 crate/binary。Cargo 依赖图重建受影响 crate，链接仍有成本。本分支没有编译耗时基准，不承诺加速倍数。

发布保留原 `release` profile（opt-level 3、thin LTO、codegen-units 1）。另提供显式 `research` profile（opt-level 2、无 LTO、16 codegen units）；它的身份与 release 分开，不能把不同优化产物当同一科学执行。`researchctl plan-build BUILD` 仅输出经过校验的 scoped Cargo 参数，不执行编译或配置云端 builder。

CI 的 `capture-research-build-inputs.sh` 将实际编译器/标准库、原生软件版本、编译环境、profile、lock 和 scoped 配方指纹纳入缓存键，并将这些输入保存在 release manifest。Cargo cache 与可执行产物分开：缓存只影响后续编译效率，命中缓存仍必须执行 build、二进制摘要验证和 image smoke。每个 runner 有自己的可写 target，禁止多租户共享可写 target；readonly prepared-data mount 不能被当作 compiler cache。

生产构建调度、变异 workspace 的源码归档/签名导入和 release verifier 向 PG 的自动投影尚未实现。现有 CI bundle 能构建一次并被 smoke/发布复用；新 PG 合同能让多个 Run/Attempt 复用已导入的同一产物。不能据此声称已有自动 Agent 变异 → Build → 科学执行闭环。

## 数据：CH 数值准备、版本化出口、bounded 共享

`src/data.rs` 的 `DataViewSpec` 绑定 venue、instrument、market、depth、排序后的 source SHA、normalizer、SQL recipe、feature names、时间窗、lookback、多个 horizon、容差、split 和 fitting cutoff。没有写死某个资产、100 档或某组 horizon。

默认 SQL recipe 为 mid/spread/depth imbalance。PreparationPlan 也可携带经过原生 admission 审核的不可变 recipe_sql，绑定 exact SQL digest、固定 features/labels 两个插入目标和输出 schema；算法变化形成新的数据身份，可以复用同一 prepare Build。参数、horizons、数据窗或已审核 SQL recipe 变化无需编译 Rust。这个入口不是向 Agent 开放的任意 SQL 执行器，也不是 SQL parser/sandbox；科学 grant 和 CH 的独立权限边界仍必须接入。改动 Rust 的解码/计算实现或打包默认值本身才需新 Build。

`sql/clickhouse.sql` 使用持久 MergeTree normalized books/events 与 prepared features/labels；`sql/prepare.sql` 一次计算共享特征并用 AVAILABLE 时钟 + horizon 做 temporal ASOF join。同 segment、标签容差、split end 和 fitting cutoff 约束阻止跨 gap/会话/分割取未来数据。相同未来时间的 tie 使用 availability、ordinal、source 做确定选择。CH 不是 OSS 的临时 `s3()` 扫描器。

`src/clickhouse.rs` 使用参数绑定的固定 SQL，输出有界 RowBinary；Training / Features / Replay 是三个 typed 出口，不产生 JSONL spool。每次 preparation 有独立 physical generation，由 PG lease/fence 和 DataView advisory lock 准入。旧 attempt 不能覆盖已发布 generation。`research-prepare` worker 在每个 CH 阶段前复查 lease/deadline/撤销，将内容寻址 block 上传后最后写 receipt。

`src/prepared.rs` 使用带版本、大小上限的 bincode；本地文件和对象读取核验 manifest/block digest。Transport 只能提供字节，不能自行替换解码结果；一次性 acquired buffers 在解码后释放。`VerifiedCache` 对缺页进行读/解码/时钟/split 验证，cache hit 仍执行当前 view 的合同检查。一个 bounded batch 可复用 readonly `Arc`，模型、试验、optimizer/checkpoint 状态各自独立。batch owner 还要计入外部持有 Arc 的内存，不能声称 LRU 本身限制了所有引用的总驻留量。

`apps/backtest::engine::replay_shared_target_positions` 已消费这些 shared typed 输入，复用现有 IOC target-position engine，每个试验新建状态。它拒绝错误 manifest、market、instrument、多 gap segment 和 split 外决策；availability ns 向上取整到 us，避免提前看数据。这里没有新增被动排队成交或 live 交易声明。

现有 collector → 新 normalized CH tables 的独立验证/ingestion 接口、新 trainer 的 shared-input 接入、科学 CLI 的 Run/config 下载和最终 manifest 写入、原生 evaluation/Campaign settlement 的投影仍未完成。不能把 schema、prepare worker 或 tiny replay fixture 当作真实生产数据、训练或终态科学成果。

## PG 单权威、任务和终态

`sql/postgres.sql` 是离线迁移，安装后 authority 为 `paused`、backend 为 disabled。连接服务不会迁移或启用。提交必须具有旧 writer 停止证明和迁移回读证明；新任务/结果不存在 DuckDB fallback。旧 CEX DuckDB 科学实现仍存在，是待迁移的旧入口，不能与新 PG authority 双活。

trusted native governance verifier 必须预先导入 exact TaskSpec 的科学 grant、预算/资源 reservation 和 release-admission receipts。服务与 Agent 没有 issuance/revocation 写权限。当前只实现投影合同与读取校验，尚未接入原生签名/grant verifier、预算扣账或 closed-family evaluator。Holdout 在通用 submit 中始终拒绝。SHA 引用本身不等于已验证签名或已扣预算。

claim 使用 PG 事务、行锁、revision/fence 和全局 quota；Launching/Running/Stopping 都占并发，过期但未确认停止的资源仍占位。先提交 durable claim 再调用 provider；重新连接用同名资源 GET + UID + 标签/完整 annotations readback，不能因超时另造 resource identity。总 timeout 在首次 claim 设定并跨 retry 保留，queued retry 也不能重置截止时间。当前实现短事务认领，但 provider reconciliation 仍持有 task/global authority 锁；这会串行化 I/O，属于明确的吞吐限制，扩容前需拆为基于 revision 的 outbox reconciliation。

取消/超时/重试经过 Stopping。provider foreground delete 绑定 UID，资源不存在且对应 task/attempt/fence 的 Pod 列表为空后才能确认 process-tree stop。TTL、Job 消失或 Session turn interrupt 不是科学 cancel 成功。receipt 必须绑定 task、attempt、fence、输入、source、image、fit，实际 artifacts 和 checkpoint 经独立字节验证；只有停止确认后 PG 才落终态 result，Prepare 同事务发布 view。checkpoint 对当前 attempt 单调，retry 保留已验证 checkpoint，旧 fence 和晚到结果拒绝。

Backend profile 绑定 exact cluster/namespace/service account、架构、CPU/内存/scratch、接受证明及可选 readonly prepared PVC / worker config secret。GPU 显式拒绝。worker service account token 不自动挂载；控制平面 token 与 worker 凭据分开。新接口使用 `agents.kruise.io/v1alpha1` CRD 模板，但没有假定官方 Rust SDK、E2B 完整日志事件 API、memory snapshot 或 provider command reconnect 已被验证。

ArtifactGateway/Writer 是 HTTPS、无 redirect、大小有界的 scoped gateway 合同，不是向 OSS 原生 endpoint 直接发送 bearer token。gateway、identity broker、每个 attempt 的输出前缀/短期凭据、只读源码/数据范围与 ACR pulls 仍需要独立部署和权限验收。当前代码不会创建这些资源或 RAM 权限。

## Session：借鉴 OpenResearch，保留 Monday 科学权威

参考固定版本 [OpenResearch f4cec9f, v0.2.15](https://github.com/alphaXiv/OpenResearch/commit/f4cec9f010a64fccf51cd4653ba548df2e5fb648)（MIT），只参考合同，未复制其运行时代码或引入 AX 依赖。

其 [Codex harness](https://github.com/alphaXiv/OpenResearch/blob/f4cec9f010a64fccf51cd4653ba548df2e5fb648/src/local/harness/codex.rs) / [本地 adapter](https://github.com/alphaXiv/OpenResearch/blob/f4cec9f010a64fccf51cd4653ba548df2e5fb648/src/local/codex.rs) 提示需要长驻 app-server child、initialize/initialized、thread start/resume、turn start/steer/interrupt 和双向审批。PG thread ID 不能替代 native CODEX_HOME 状态。`research::SessionSnapshot` 因此绑定 workspace、transcript、code commit 和 native-state manifest；`coding_agent::admit_resume` 拒绝缺失或摘要错误的 native state。实际 child transport、workspace/native files 的持久化与恢复尚未实现。

OpenResearch 测试的 Codex 版本为 0.144.0；本机只读生成的 app-server schema 为已安装 0.159.2。本分支审批合同标注后者，并绑定原 RPC ID + process generation + command/file/user-input 类型；重启后相同 RPC ID 不能复用旧审批。当前只接受逐次 accept/decline/cancel，不授予持久 policy amendment。未启动 model/session，不能声称完整协议互通或版本兼容验收。

Plan mode 是 prompt，不能替代隔离或授权。未知或已接受的消息 delivery 必须 reconcile，不能盲重发；not_sent/rejected 可重试。Run 完成订阅独立于 Session 所有权，PG 在终态事务中按显式 subscription 写去重 completion intent；迟到订阅也读取终态。provider wake 的 delivery ledger/dispatcher 仍未实现，outbox intent 不等于消息已送达。

`agent_api.rs` 提供可选、默认关闭的 loopback bearer-capability API，只有 `research.submit/status/artifacts`，principal 来自服务配置。submit 只能引用预先批准的请求；不允许配置 evaluator、打开 holdout、签名、kubectl 或携带 PG/cluster 凭据。`researchctl tool` 是独立客户端。远端 Sandbox 到这个 loopback API 的受控 HTTPS/proxy/broker 连接尚未实现，不能声称 Session 已连通。

OpenResearch [chat delivery](https://github.com/alphaXiv/OpenResearch/blob/f4cec9f010a64fccf51cd4653ba548df2e5fb648/src/local/chat/mod.rs) 和 [Kubernetes jobs](https://github.com/alphaXiv/OpenResearch/blob/f4cec9f010a64fccf51cd4653ba548df2e5fb648/src/jobs/kubernetes.rs) 的可恢复 handle 不能代替 Monday 的 durable claim、幂等、UID/fence 和最终 manifest。没有照搬 kubectl cp bootstrap、namespace-wide secret 或共享可写 target。

远端认证不能概括成“全部无认证”：OpenResearch 的 [up_remote](https://github.com/alphaXiv/OpenResearch/blob/f4cec9f010a64fccf51cd4653ba548df2e5fb648/src/commands/up_remote.rs) / [up](https://github.com/alphaXiv/OpenResearch/blob/f4cec9f010a64fccf51cd4653ba548df2e5fb648/src/commands/up.rs) remote-host 路径使用 per-connection bearer token 和 health auth；普通 loopback up 不具备同等 application auth。它也不是 Monday 多租户 RBAC 的现成实现。

## CI/CD 与退役顺序

- `ci.yml` 保留 actual affected-package 选择和原域合同测试，新增 PG/CH ephemeral fixture job。Collector pagination 合同的存在/非 ignored 检查与完整 owning suite 结合，避免空 filter 冒充通过。
- Monorepo Rust Workspace 运行共享 strict Clippy，并上传经过选择的 stage outcomes；Security 校验 exact source/fork/base/checkout/run/attempt/numeric job、最新 producer 和 scoped command digest。weekly schedule 没有普通 producer，保留自己的 strict Clippy。
- Prediction CI 构建共享 release 一次，smoke 下载同一 artifact；原来的格式、研究/ml/db/三事件 sidecar 合同迁回 native CI。没有删去这些实际测试或用 selector 输出充当测试通过。
- ACR 保留三个 required checks、当前 main、exact Prediction producer 与 artifact 来源。软件读取校验 latest attempt/job 和实际 binary bytes，发布后拉取 OCI digest、核验 source label 和 contained executables。Campaign controller 镜像继续保留既有 Ops；不存在的 research-data-service Dockerfile 目标移除。
- ACR source-test 仍只有审核过的两个离线测试 profile、可信当前 main 和确定 source-test tag。native runner 构建/测试该镜像，不把测试生命周期塞回 ACK private executor。
- GHCR 已有 artifact/image reuse 继续保留，未重复重写。Monorepo/Prediction/Security/ACR 不再调用旧 private ACK receipt wait。历史 signed receipt/helper 测试作为审计记录保留；Mac signer、private executor 与资源策略脚本不再是这些 required checks 的完成路径。真实 Ops 和有偿资源控制未在本 PR 中启用或迁移。

## 验证、灰度与阻塞项

新服务直接依赖 SQLx core/PostgreSQL driver；不解析未用 MySQL/RSA/SQLite 驱动，也未新增 audit ignore。

本地验证：platform 数据/生命周期/Build/API/RPC 合同，shared typed replay 的 backtest owning tests，strict Clippy、fmt、selector/native CI evidence/release/source readback/workflow 合同。PG 和 CH daemon 未在 Mac 安装，真实 SQL/RowBinary/事务测试在 disposable CI services 执行；它们不是真实研究或云端 acceptance。精确本地命令、commit 和 CI 结果见 PR Validation。

最小真实 vertical slice 的依赖顺序：

1. Tokyo/ACK 基础设施独立 Gate：东京 Pro 资格/价格、ACS 容量、K8s/CRD/runtime 兼容、容器 command/log/取消和 UID 回读。当前东京底座未验收，禁止以本代码代替 Gate。
2. 专用 PG 角色、paused schema、native grant/预算/签名导入器、旧 writer quiescence 与一次迁移回读；默认保持 paused。
3. 规范化 source receipt 与 persistent CH ingestion/版本 SQL；最小经过批准的数据切片仅准备一个 view，重复请求复用同一逻辑 manifest；CH/OSS 内容独立回读。
4. Build 的源码归档、scoped 编译、contained binaries/OCI 验证和发布证明投影；两组参数 Run 引用同一 Build，强制基础设施 retry 仍保留同一 Build、总 deadline、fit/checkpoint provenance。
5. 接入一条真实原生科学 worker 的 typed input、固定 Run/config、native scientific terminal manifest / Campaign settlement；prepare 或退出码 0 不算科学成功。先 CPU Job，验收 stale receipt、取消/retry、controller 重启/幂等及实际 artifact 丢失。
6. 再接 Coding Agent app-server 的持久 Session、native-state恢复、审批与受控工具/完成 wake；Session turn interrupt 与 Job cancel 分别验证。
7. 单 tenant / concurrency=1 / 明确预算灰度，读回旧 writer 已停再启 PG，禁止双活。回滚先 pause、cancel/drain 并证明新资源全部停，再以当前恢复/迁移证明恢复旧 authority；不能同时启用旧/新 ledger。

旧 CI/private control 链的退役必须同时满足 exact-head required checks 已通过、共享发布证据可读、独立新资源生命周期/回读已真实验收、旧请求队列和 lease 已 drain、private signer 与旧 receipt 发布不再被消费，以及恢复路径验证。仅草稿 PR、unit tests 或 CRD Ready 不满足退役条件。

本次无需新的费用或安全授权即可评审代码。未来东京 Pro/ACS/Sandbox 资源、RAM/network/credential 和部署切换均须先提供具体 Gate/成本/权限/回滚结果，再在用户已限定的范围内申请下一阶段授权。本分支不会自动完成这些动作。
