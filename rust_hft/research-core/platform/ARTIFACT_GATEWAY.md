# 受控产物网关

`research-artifact-gateway` 提供已有 `ArtifactGateway` 和 `Writer` 的持久存储出口。
它不创建云资源，不签发 capability，不启用 PG authority，也不改变科学任务。

用 `gateway` feature 构建。配置示例：

```json
{
  "bind": "127.0.0.1:8091",
  "root": "/var/lib/monday-artifacts",
  "capabilities_file": "/run/monday-broker/capabilities.json",
  "max_object_bytes": 536870912
}
```

存储根目录必须是独占、持久、canonical 的目录。只有网关 UID 能写入。
网关持有文件锁；第二个 writer 会拒绝启动。配置、PG 凭据和 broker 投影必须在存储根目录之外。
`MONDAY_RESEARCH_DATABASE_URL` 使用独立 gateway 角色。服务不自动运行迁移。

部署方先由 authority owner 离线安装 `sql/artifact_gateway.sql`。
gateway 角色只需要相关状态表的 SELECT，以及该函数的 EXECUTE。
函数以受控 owner 权限取得短期行锁，不能更新任务、准入、撤销或 authority。
网关不需要这些表的 INSERT、UPDATE 或 DELETE 权限。

broker 原子替换权限为 `0600` 的 capability 投影文件。它是一个数组。
每个条目包含 `token_sha256`、UTC Unix 毫秒的 `expires_ms` 和 `access`。
剩余有效期必须在 24 小时内。API 不接受 caller 提供的新 scope 或延长有效期。
broker 负责签发、续期、取消及每次 Attempt 的凭据注入；本模块不代替 broker。

| role | 范围 |
| --- | --- |
| `reader` | 指定的只读前缀；前缀必须以 `/` 结束 |
| `publisher` | 仅固定 `research/sources/<40位commit>/` 或 `research/builds/<64位Build ID>/` |
| `attempt_writer` | `tenant`、`task_id`、`attempt`、`fence`；实际前缀从 PG 读取 |

AttemptWriter 在读取及上传前后必须仍属于 Running 任务，且 lease、总 deadline 和 admission 有效。
最终发布持有 authority/task/admission 锁，取消、撤销、pause 和接管与发布有确定顺序。
终态结果回读使用独立 Reader；旧 worker 不能利用只读回执延续写权限。

HTTP 接口只有 `GET /research/<key>` 和 `PUT /research/<key>`。
PUT 必须携带 `If-None-Match: *`。文件先流式写入私有临时文件，限额并同步，最后原子 link。
同名文件不能覆盖；重复 PUT 返回 412，已有 Writer 独立回读并比较实际字节。
不完整上传不会成为可见对象。进程强制退出留下的私有临时文件不能通过接口读取，清理由独立存储保留合同处理。
路径逐段用 directory FD/NOFOLLOW 打开；拒绝符号链接、隐藏文件、空路径段和父目录跳转。
一次只处理一个上传，上传总时间上限 30 秒，单对象上限 512 MiB。

监听地址只允许 loopback。部署方必须在同一隔离环境内提供私有 TLS ingress，再将 HTTPS 端点交给现有客户端。
网关不会把 bearer capability 发往 OSS 原生 endpoint。
TLS ingress、broker、持久卷、远端身份和真实数据验收仍属于部署阶段。
