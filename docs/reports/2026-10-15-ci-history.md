# Monday CI 历史分析：2026-10-15

## 结论

本次实施安全流水线中 `cargo-audit` 和 `cargo-machete` 的预编译安装。
两者保留已观察到的工具版本、检查命令、检查范围和失败条件。
按每次替代安装占用 10 秒估算，一周历史负载下合计节省 **12.27 runner 分钟/天**。
这是当前可明确量化、没有其他任务占用、且无需修改测试或生产行为的最高收益候选。
此数字是估算，不是上线后的实测收益。

## 数据和口径

- 仓库：`proerror77/monday`；分析基线：`08b98142e75588dd37a77a98b2bfe3486f5d3618`。
- 窗口：2026-10-08 00:00:00 至 2026-10-15 00:00:00 UTC，左闭右开，共 7 天。
  对应北京时间 2026-10-08 08:00 至 2026-10-15 08:00。
- 按天分页读取 GitHub Actions API，避免宽时间查询的运行数量上限截断。
  共 1,842 次 workflow run；503 次 skipped 不分配工作。
  其余 1,339 次均读取 jobs，使用 `filter=all` 包含保留的历史 attempts。
- runner 分钟为每个已分配 job 的 `completed_at - started_at` 之和除以 60。
  并行 job 相加；排队时间不计；这不是账单分钟，也不是 PR 等待时间。
- 候选安装成本只统计成功完成的安装步骤；失败或取消中的不完整步骤不推算收益。
- 日均收益公式为 `(历史成功安装总秒数 - 安装次数 × 替代安装秒数) / 60 / 7`。
  已有缓存命中的零秒安装也纳入计算，允许单次收益为负，避免夸大收益。
- 一周活动频率不是未来负载保证；本窗口内代码、缓存和 CI 分工均有变化。

## 可量化候选，按预计节省分钟/天排序

下表统一用每次替代安装 10 秒作规划预算。除本次实施项外，替代路径尚未实测。
各候选独立计量，不能把同一安装的多个优化重复相加。

| 排名 | 优化 | 一周安装次数 | 原安装总秒数 | 原安装中位秒数 | 预计节省 runner 分钟/天 | 处理 |
| --- | --- | ---: | ---: | ---: | ---: | --- |
| 1 | 安全工具预编译安装：audit + machete | 154 + 154 | 2,407 + 5,828 | 1 / 41 | **12.27** | 本次实施；同版本、固定 installer SHA、校验下载内容 |
| 2 | `cargo-license` 固定版本的独立二进制缓存 | 154 | 4,279 | 30 | 6.52 | 待验证缓存命中率；现有 installer 没有该工具的直接 manifest |
| 3 | SQLx CLI 避免重复源码安装 | 57 | 2,669 | 62 | 5.00 | 待验证预编译 CLI 的 PostgreSQL/TLS 功能等价性；涉及其他 PR 所有权 |
| 4 | actionlint 使用同版本预编译工具 | 193 | 2,298 | 12 | 0.88 | 收益小；涉及已在修改的 prediction 工作流 |

第一项内部：machete 为 10.21 分钟/天，audit 为 2.06 分钟/天。
audit 的历史缓存命中很多；不能把最近一次 182 秒的冷安装乘以所有运行次数。
若两个新安装各用 5 秒，合计估算为 15.94 分钟/天；各用 20 秒则为 4.94 分钟/天。
实际值须从本次完整安全运行及后续同类运行读取。

## 较大的成本池，暂不宣称可节省量

| 成本池 | 历史成功步骤运行分钟/天 | 为什么不能直接算成收益 |
| --- | ---: | --- |
| Research immutable release build | 333.44 | 现有 PR #1430、#1410 正在处理 recipe/native dependency/linker；需同输入构建对照 |
| Loop nextest shards | 326.48 | 分片可能缩短等待，但增加 runner 总分钟；PR #1368 已有独立任务 |
| Collector release shell contracts | 140.87 | PR #1371 已做对照，但其最慢耗时目标未达到；不能覆盖其 draft 边界 |
| Docker smoke image builds | 114.10 | 需逐项证明缓存及已有镜像覆盖，不能删掉验收或重复计算主构建缓存收益 |

取消 job 另占 142.82 runner 分钟/天，但取消前完成的验证不能全部视为浪费。
不修改 main 的精确源码验证和发布串接，也不把排队时间算入该数字。
采集器定时监控的一些运行长时间轮询后失败；其运行时间不是可直接删除的 CI。
例如 [run 38749780041](https://github.com/proerror77/monday/actions/runs/38749780041)
耗时约 11.7 分钟，但缺少足以判断根因的响应证据；本次不缩短监控预算或修改远端服务。

## 实施证据和验证方法

- [安全基线 run 38530458592](https://github.com/proerror77/monday/actions/runs/38530458592)：
  [audit job 115740993281](https://github.com/proerror77/monday/actions/runs/38530458592/job/115740993281)
  中安装 `cargo-audit 0.22.2` 用 182 秒，审计用 7 秒。
  [machete job 115740993611](https://github.com/proerror77/monday/actions/runs/38530458592/job/115740993611)
  中安装 `cargo-machete 0.9.2` 用 31 秒。
- 本次使用仓库已有的 `taiki-e/install-action`，固定为
  `1168f6810a3cb30a3514bf5756a3b7f0422c6df8`，工具分别固定为 `0.22.2` 和 `0.9.2`。
  installer 对这两个版本均有 Linux x86_64 manifest 和 SHA-256；开启 checksum，禁止安装 fallback。
- audit advisory 数据库仍由原审计命令读取最新数据。固定的是执行工具，不是漏洞库。
  后续升级工具版本需同时更新固定的 installer manifest 与工具版本。
- 删除 audit job 不再需要的源码编译缓存。其余 job、检查命令、权限、选择逻辑、报告和 aggregate gate 不变。
- 本地：actionlint、审计 summary 合同测试、完整 CI scope selector 测试、`git diff --check`。
  另外以 YAML 结构对照确认变更只涉及两个 job 的安装步骤及废弃缓存。
- 普通 workflow-only PR 的 selector 不执行 audit/machete。因此必须在同一 PR head
  手动触发 `security-enabled.yml`，确认真实下载、版本、审计及 aggregate gate；
  再核对 current-head 的三个分支保护检查及合并身份。

## 可复查的数据身份

运行清单的 SHA-256：
`231f6a3e860f4127d55f80ebcd79b3e39d8eb0fe1f55b98eb5ae55c9ec273d06`。

按运行、job、成功步骤展开的证据清单 SHA-256：
`3a78aee449cc2d51b54c5ba3da382f55cf57030591f9c03d094ef3e8ff6d3ee8`。

复查入口：逐日读取 `GET /repos/proerror77/monday/actions/runs?per_page=100&created=<UTC日范围>`，
跟随分页；对每个非 skipped run 读取
`GET /repos/proerror77/monday/actions/runs/<id>/jobs?per_page=100&filter=all`，跟随分页。
根据上述时间窗口、步骤名和公式重算；GitHub 日志或历史 attempt 到期后，数据可能不再完整。

本次只改变 CI 工具安装。生产服务、交易、风控及采集器配置不变。
回滚可还原该工作流安装差异。合并仍可能触发现有自动发布流程；发布不代表部署。
