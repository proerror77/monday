# Paused persistent research foundation assets

这些资产补齐持久 PG、CH、artifact gateway 和 Session 的部署包装。它们是
离线代码与模板。仓库没有应用生产迁移、创建资源、修改 IAM/network、启动模型、
运行科学任务或启用 backend。参考 [research foundation](../../../../docs/architecture/RESEARCH_FOUNDATION.md)。

## 离线准备

1. 从经过验证的发行产物取得 control 程序和所选原生 Codex executable。
   `Dockerfile` 只安装预构建程序。Session 镜像还必须独立安装摘要匹配的 native
   Codex，不能使用 launcher 脚本。镜像验收、签名和 pull 权限是独立发布合同。
2. 在仓库外复制 `inventory.example.json`，填入实际镜像 digest、现有 Retain
   StorageClass、现有私有 broker PVC、有限容量、cluster/API IP，以及既有
   Experiment、Session policy 和 native executable 身份。示例刻意不能直接渲染。
3. 运行 `researchctl render-foundation INVENTORY /absolute/new/output-directory`。
   此命令无 PG/provider 连接。它只写新目录，拒绝覆盖已有目录。配置、ConfigMap、
   StatefulSet、ClusterIP 和 NetworkPolicy 都以可评审文件输出；所有副本固定为 0。
4. 在部署 Gate 中独立核验 images、预算/保留期、存储与 TLS/RBAC。
   渲染结果没有 `kubectl apply`、StorageClass、Namespace、Secret 或 IAM 创建操作。

PVC 在缩容和 StatefulSet 删除后保留。PG/CH/objects/native-state/delivery/workspace
不会使用科学 Job 的临时盘。外部 broker claim 必须支持控制器与 Session 的并发只读挂载，
并由独立原生 broker writer 维护。模板没有为 broker 创造网络、账号、角色或发行权。

## PG 与 CH 的离线安装

用独立 schema owner 按顺序安装 platform `sql/postgres.sql`、`verified_build_release.sql`、
`session_deliveries.sql`、`artifact_gateway.sql`、`native_admission.sql`，再安装 `postgres/roles.sql`。
Native admission 迁移最后替换 artifact permit，保留 fence、lease、deadline、撤销与暂停检查，并加原生有效期和租户检查。
`authority.mode` 必须仍为 `paused`；所有 backend 仍为 disabled。
NOLOGIN 角色没有密码，也没有互相继承。经过独立审查的 login identity 才能获得对应角色。

Submitter 只提交已准入请求。Reconciler 管 task/result/outbox。
Session host 管 native Session 与 completion delivery。Gateway 只有 SELECT 与
`artifact_write_permit` 的 EXECUTE。Prepare worker 没有 PG 写权限。
Native admission 与 verified release import 使用独立身份，Agent 不取得这些角色。
只有 native admission importer 可以插入 `native_admission_imports`；控制器、Submitter、Session 和 Gateway 只读。
Prepare worker 只读取 request、tenant 和 expiry 三列，供 lease gate 校验。它不读取 trust document。
只授予不可变 key 列或固定 true singleton 的 UPDATE 来支持 row lock；这些权限不能
改 authority mode、proof、budget 或 admission 文档。没有 application authority-owner 凭据。

PG 网络入口强制 TLS + SCRAM；连接 URL 必须指定 `sslmode=verify-full` 与受控 CA。
bootstrap password 仅用于空 PG data 初始化，不安装 research schema 或激活服务。
PG 的 paused 恢复应先确认 active task/process 状态，而非直接改 writer。

CH 模板持久保存 foundation `research` 数据库；不是旧 `monday_analytics` 的替换部署。
使用 platform `sql/clickhouse.sql` 的离线 schema。专用 schema owner 及 normalized
数据导入身份须独立安装和验收；准备账号只 SELECT normalized 与 SELECT/INSERT prepared。
它没有 CREATE、ALTER、DELETE 或 raw archive 权限。Reader 为 readonly。
CH 只开 TLS 8443/9440，并要求由私有 CA 签发的 client certificate。客户端仍需专用账号。
它关闭明文端口与 query 日志；科学 bulk 数据继续留在 ACK/OSS。
TCP readiness probe 只说明端口可连接，不能证明 TLS、migration、数据完整性或 scientific terminal。

## 私有 TLS 和 scoped broker

控制 Pod 的两个 Rust listener 仅监听 `127.0.0.1:7081/7082`。
同 Pod 的 nginx 在 8443/8444 提供 TLS 1.2/1.3 + 必需 client certificate；不记录请求
或 bearer。入口只有 POST `/research` 与 GET/PUT `/research/`，保留对象不可覆盖 header。
私有 CA 不关闭 hostname 验证；显式 CA 替换 public roots。客户端拒绝 redirect 和环境 proxy。

init 容器把 Kubernetes Secret 的 TLS 及 controller credential projection 复制为
非 root UID 所有、0600 的 regular 文件，放在私有 tmpfs。TLS key 和 client identity
不能直接挂 symlink 或 group-readable 文件给 Rust client。Server certificate SAN 必须
覆盖实际 Service DNS；controller 的 Kubernetes CA 必须覆盖 inventory 的 API IP。
Gateway、controller、Session 和 worker 使用不同 PG/reader/writer 身份。
客户端 credential PEM 和 PG URL 不进入子进程环境、tool payload、PG 或日志。

原生 broker 在 `/run/broker/private` 0700 目录中以 atomic rename 发布 0600 regular 文件。
既有 broker PVC 根目录须满足 fsGroup 的 group/permission readback，且使用 OnRootMismatch，
避免 Kubernetes 递归修改内部私有目录与 key 权限。此项仍须真实 CSI/mount 验收。
`tools.json` 与 `artifacts.json` 初始可为 `[]`，所有请求被拒绝。
Broker 不可把文件放在 Agent workspace、native home 或 artifact store。
工具 API 每次请求重新读取 projection；删除、到期或移除 permission 即撤销后续请求。
Session host 每次工具调用重新读取 host-only token，续期不需要重启 child。

工具 capability 形状为 `token_sha256 / tenant / not_before_ms / expires_ms / permissions`。
最长有效区间为一小时。Permission 只能为 exact `research.submit` request + idempotency key，
或 exact Run 的 `research.status` / `research.artifacts`。没有 tenant-wide wildcard、cancel、
SQL、kubectl、签名、risk 或 resume RPC。租户从 broker principal 取得，不接受 payload tenant。
这只收窄工具访问；真正提交仍需 PG native scientific admission、未撤销 grant、资源预算、
verified release、旧 writer quiescence 与 migration readback。

Artifact capability 保留 Reader、exact Build/source Publisher 和 tenant/task/attempt/fence
AttemptWriter。Gateway 每次及发布前读取 broker 与 PG 撤销；最终发布持有 scoped permit。
任务停止仍需 provider process-tree 确认。Session interrupt 不取消科学任务。

## Session 停止与恢复

Session Pod 使用固定 native executable，host-only state 和只读 workspace。首次 `start`
必须有空 native/delivery 树；已有状态只能显式 `resume`。模型网络规则未开放，启动不会发 turn。
Typed stdin 来自受信 operator；没有公开 host-command endpoint。审批仍绑定 process generation。

正常 stdin EOF、`checkpoint`、`close`、SIGINT 或 SIGTERM 会先停止 child，再在 native 与
host delivery 两个 writer lock 持有期间捕获并原子保存 `/state/checkpoints/native.json`。
检查点 schema 2 绑定 provider digest、native thread、全部允许的 native 文件，以及 host
消息去重文件的 exact path/hash/size。保存后才释放 writer lock。强杀不会生成新检查点。

1. 先确认旧 Pod/child 已停止；Session interrupt 本身不是这一证明。
2. 同时保留/恢复 native、deliveries、checkpoint 与对应只读 workspace source。
   不得只恢复 PG thread ID，或把新 native 状态配旧 dedup 目录。存储 snapshot 是外部 Gate。
3. 独立回读检查点身份和 workspace source；不修改未确定的消息记录为 accepted。
4. 将 Session args 显式改为 `resume /config/session.json /state/checkpoints/native.json`。
   host 在取得两个 writer lock 后按检查点逐字节和 exact file coverage 校验，再启动 child。
   少文件、多文件、改字节或 provider digest 变化都失败，不会自动开始一个新 thread。
5. 原 process generation 的 pending approval 不可重放。Unknown completion delivery 只允许
   native item readback 对账，不重发。host receipt 仍由 PG 独立存储。

如果强杀后实际卷比已保存检查点更新，恢复会失败。应隔离现有卷并取最后一致的 stopped
snapshot，或由独立恢复合同对账后生成新的真实证据。不要删除新状态、重写去重文件、
伪造 snapshot 或重置 scientific deadline/fence 来通过恢复。

## 启用与回滚边界

私有 listener/manifest/角色/TLS 测试不是云部署或真实接受证明。启用仍需要旧 writer 已停、
native grants、迁移/存储/网络/provider readback；默认零副本与 paused authority 不改变。
这组代码不开放 sealed holdout、交易、风险修改或 runtime resume。

回滚顺序仍为 pause 新 authority，cancel/drain 科学任务，独立确认所有 process tree 停止，
再按已审查恢复合同决定旧 authority。保留 append-only ledger、artifact 和 Session 状态。
删除计算 Pod、缩容或 Session child stop 都不能替代这个研究控制合同。
