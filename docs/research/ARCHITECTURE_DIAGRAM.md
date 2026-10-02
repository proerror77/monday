# Monday Research ACK 架构图

## 整体架构

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                          Monday Research System                              │
└─────────────────────────────────────────────────────────────────────────────┘

┌─────────────────────┐
│  Binance Public     │
│  WebSocket Feed     │
│  (Spot + USD-M)     │
└──────────┬──────────┘
           │
           ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│  ECS Collector (monday-trade-data-26)                                       │
│  ┌─────────────────────────────────────────────────────────────────┐       │
│  │  binance-lob-archiver                                           │       │
│  │  • Capture LOB snapshots + deltas                               │       │
│  │  • Write manifest.json + tape.jsonl.zst                         │       │
│  │  • Sync segments.parquet (NEW)                                  │       │
│  └─────────────────────────────────────────────────────────────────┘       │
│  Cost: ~CNY 483/month (包年)                                               │
└──────────┬──────────────────────────────────────────────────────────────────┘
           │
           ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│  OSS Storage (oss-ap-northeast-1-internal.aliyuncs.com)                     │
│  ┌────────────────────┐  ┌────────────────────┐  ┌────────────────────┐   │
│  │  lake/raw/         │  │  lake/output/      │  │  research/         │   │
│  │  • tape.jsonl.zst  │  │  • segments.parquet│  │  • campaigns/      │   │
│  │  • manifest.json   │  │    (DuckDB Index)  │  │  • results/        │   │
│  │  • _SUCCESS        │  │  • features/       │  │  • ledgers/        │   │
│  │                    │  │  • materialized/   │  │                    │   │
│  │  ~500GB           │  │  ~50GB            │  │  ~10GB            │   │
│  └────────────────────┘  └────────────────────┘  └────────────────────┘   │
│  Cost: ~CNY 100/month                                                       │
└──────────┬──────────────────────────────────────────────────────────────────┘
           │
           ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│  ACK Cluster (monday-research-apne1, Tokyo)                                 │
│                                                                              │
│  ┌────────────────────────────────────────────────────────────────────┐    │
│  │  System Pool (固定)                                                 │    │
│  │  • 1 node: ecs.u1-c1m2.large (2C4G Spot)                           │    │
│  │  • CoreDNS, kube-proxy, etc.                                       │    │
│  │  Cost: ~CNY 82-87/month                                            │    │
│  └────────────────────────────────────────────────────────────────────┘    │
│                                                                              │
│  ┌────────────────────────────────────────────────────────────────────┐    │
│  │  Worker Pool (autoscale 0-4)                                       │    │
│  │  • nodeSelector: workload=backtest                                 │    │
│  │  • ecs.u1-c1m4.xlarge (4C16G Spot)                                 │    │
│  │  • Scale to zero when idle                                         │    │
│  │  Cost: CNY 0.342/hour per node                                     │    │
│  └────────┬───────────────────────────────────────────────────────────┘    │
│           │                                                                  │
│  ┌────────┴──────────────────────────────────────────────────────────┐    │
│  │  Namespace: monday-research                                        │    │
│  │                                                                     │    │
│  │  ┌──────────────────────────────────────────────────────────┐    │    │
│  │  │  研究开发层 (Development Layer)                           │    │    │
│  │  │  ┌────────────────────────────────────────────────┐      │    │    │
│  │  │  │  StatefulSet: research-devpod                  │      │    │    │
│  │  │  │  • Image: rust:1.98.1-bookworm                 │      │    │    │
│  │  │  │  • Init: git-sync (拉取代码)                   │      │    │    │
│  │  │  │  • Resources: 2-4C, 4-8Gi                     │      │    │    │
│  │  │  │  • Replicas: 1-3 (多用户)                     │      │    │    │
│  │  │  │                                                 │      │    │    │
│  │  │  │  Volumes:                                      │      │    │    │
│  │  │  │  • workspace (20Gi Block PVC)                 │      │    │    │
│  │  │  │  • cargo-cache (10Gi Block PVC)               │      │    │    │
│  │  │  │  • raw (OSS CSI, ReadOnly)                    │      │    │    │
│  │  │  │  • output (OSS CSI, ReadWrite)                │      │    │    │
│  │  │  │                                                 │      │    │    │
│  │  │  │  Features:                                     │      │    │    │
│  │  │  │  ✓ Git pull 更新代码 (10秒)                   │      │    │    │
│  │  │  │  ✓ 增量编译 (<1分钟)                          │      │    │    │
│  │  │  │  ✓ 状态持久化                                  │      │    │    │
│  │  │  │  ✓ DuckDB 预装                                 │      │    │    │
│  │  │  └────────────────────────────────────────────────┘      │    │    │
│  │  │                                                            │    │    │
│  │  │  ┌────────────────────────────────────────────────┐      │    │    │
│  │  │  │  Job: kaniko-build                             │      │    │    │
│  │  │  │  • 从 GitHub 拉取代码                          │      │    │    │
│  │  │  │  • 原生 amd64 编译 (无 QEMU)                  │      │    │    │
│  │  │  │  • 推送到 ACR                                  │      │    │    │
│  │  │  │  • BuildKit 缓存                               │      │    │    │
│  │  │  │  • 时间: 30-60分钟 (首次)                     │      │    │    │
│  │  │  └────────────────────────────────────────────────┘      │    │    │
│  │  └──────────────────────────────────────────────────────────┘    │    │
│  │                                                                     │    │
│  │  ┌──────────────────────────────────────────────────────────┐    │    │
│  │  │  数据准备层 (Preparation Layer)                          │    │    │
│  │  │  ┌────────────────────────────────────────────────┐      │    │    │
│  │  │  │  Job: segment-index-backfill                   │      │    │    │
│  │  │  │  • 扫描历史 manifests                          │      │    │    │
│  │  │  │  • 生成 segments.parquet 索引                  │      │    │    │
│  │  │  │  • 一次性运行                                  │      │    │    │
│  │  │  └────────────────────────────────────────────────┘      │    │    │
│  │  │                                                            │    │    │
│  │  │  ┌────────────────────────────────────────────────┐      │    │    │
│  │  │  │  Job: cex-materialization                      │      │    │    │
│  │  │  │  • 使用 DuckDB 索引 (NEW)                     │      │    │    │
│  │  │  │  • 选择段: 20小时 → 10分钟 (120x)             │      │    │    │
│  │  │  │  • Slice → PIT → Replay                        │      │    │    │
│  │  │  │  • 输出: features.parquet                      │      │    │    │
│  │  │  └────────────────────────────────────────────────┘      │    │    │
│  │  └──────────────────────────────────────────────────────────┘    │    │
│  │                                                                     │    │
│  │  ┌──────────────────────────────────────────────────────────┐    │    │
│  │  │  研究执行层 (Execution Layer)                            │    │    │
│  │  │  ┌────────────────────────────────────────────────┐      │    │    │
│  │  │  │  Job: campaign-execute (pre-holdout)           │      │    │    │
│  │  │  │  • 因子筛选 + Ridge/CART/MLP                   │      │    │    │
│  │  │  │  • 多轮 walk-forward 验证                      │      │    │    │
│  │  │  │  • 成本感知回放                                │      │    │    │
│  │  │  │  • 输出: campaign-result.json                  │      │    │    │
│  │  │  └────────────────────────────────────────────────┘      │    │    │
│  │  │                                                            │    │    │
│  │  │  ┌────────────────────────────────────────────────┐      │    │    │
│  │  │  │  Job: campaign-cycle-controller                │      │    │    │
│  │  │  │  • 读取结果                                    │      │    │    │
│  │  │  │  • 生成学习计划                                │      │    │    │
│  │  │  │  • 迭代优化 (bounded)                          │      │    │    │
│  │  │  │  • DuckDB ledger                               │      │    │    │
│  │  │  └────────────────────────────────────────────────┘      │    │    │
│  │  └──────────────────────────────────────────────────────────┘    │    │
│  └─────────────────────────────────────────────────────────────────┘    │
│                                                                              │
│  Total Cost (idle): ~CNY 202-207/month                                      │
│  Worker Cost: CNY 0.342/hour (按需)                                        │
└─────────────────────────────────────────────────────────────────────────────┘
```

## 数据流详解

```
┌────────────────────────────────────────────────────────────────────────┐
│  Phase 1: 数据采集 (Continuous)                                        │
└────────────────────────────────────────────────────────────────────────┘

Binance WebSocket
  │
  ▼
binance-lob-archiver (ECS)
  ├─ 1小时滚动窗口
  ├─ 写 manifest.json (元数据)
  ├─ 写 tape.jsonl.zst (压缩的 LOB 事件)
  ├─ 写 _SUCCESS (完成标记)
  └─ 写 segments.parquet (索引, NEW)
  │
  ▼
OSS lake/raw/
  venue=binance_usdm/dataset=lob/date=2026-09-01/hour=12/
    ├─ manifest.json
    ├─ tape.jsonl.zst
    └─ _SUCCESS

OSS lake/output/metadata/
    └─ segments.parquet (全局索引)

┌────────────────────────────────────────────────────────────────────────┐
│  Phase 2: 索引维护 (Once + Continuous)                                 │
└────────────────────────────────────────────────────────────────────────┘

历史数据回填 (一次性):
  segment-index-backfill Job
    │ 扫描 OSS lake/raw/
    │ 读取所有 manifest.json
    │ 生成 segments.parquet
    ▼
  OSS lake/output/metadata/segments.parquet

实时更新 (持续):
  binance-lob-archiver
    │ 每小时段完成时
    │ append 到 segments.parquet
    ▼
  索引自动更新

┌────────────────────────────────────────────────────────────────────────┐
│  Phase 3: 研究准备 (On-demand)                                         │
└────────────────────────────────────────────────────────────────────────┘

Campaign 请求
  ├─ 时间窗口: 2026-09-01 00:00 ~ 2026-09-07 23:59
  ├─ Symbol: BTCUSDT
  └─ Horizon: 5s
  │
  ▼
cex-materialization Job
  │
  ├─ Step 1: 选择段 (NEW: 使用 DuckDB 索引)
  │   SELECT * FROM segments.parquet
  │   WHERE symbol = 'BTCUSDT'
  │     AND date BETWEEN '2026-09-01' AND '2026-09-07'
  │     AND replay_safe = true
  │   ORDER BY start_received_at_ns
  │   执行时间: 20小时 → <1秒 (120x 加速)
  │
  ├─ Step 2: Slice (可选并行)
  │   binance-market-tape-slicer
  │   • 提取单个 symbol
  │   • 验证序列连续性
  │   • 输出: sliced segments
  │
  ├─ Step 3: PIT Materialization
  │   lob-pit-materializer
  │   • 重放 LOB 事件
  │   • 计算 point-in-time 特征
  │   • 生成标签 (5s horizon)
  │   • 输出: features.parquet
  │
  └─ Step 4: Replay Verification
      binance-replay-parquet-materializer
      • 验证可重放性
      • 生成 campaign-inputs.json
      • 记录 SHA256 fingerprint
  │
  ▼
OSS research/campaigns/<campaign-id>/
  ├─ features.parquet
  ├─ materialization.parquet
  ├─ replay.parquet
  └─ campaign-inputs.json

┌────────────────────────────────────────────────────────────────────────┐
│  Phase 4: 模型训练与评估 (Campaign Execution)                          │
└────────────────────────────────────────────────────────────────────────┘

campaign-execute Job (--pre-holdout)
  │
  ├─ Round 1 (seed=7)
  │   ├─ Load features.parquet
  │   ├─ Factor Screening
  │   │   • 连续因子: IC 筛选
  │   │   • L2 聚合: 交易信号
  │   │   • 候选池: 22 slots
  │   ├─ Model Training
  │   │   • Ridge Regression
  │   │   • CART
  │   │   • MLP (optional)
  │   │   • Walk-forward validation
  │   ├─ Model Selection
  │   │   • IC/ICIR
  │   │   • Sharpe Ratio
  │   │   • Cost-aware replay
  │   └─ Output: mission-result.json
  │
  ├─ Round 2 (seed=11)
  │   └─ (same as Round 1)
  │
  └─ Aggregate Results
      • 选择最佳模型
      • 失败分类
      • 输出: campaign-result.json
  │
  ▼
OSS research/campaigns/<campaign-id>/
  ├─ round-0-mission.json
  ├─ round-0-results.zip
  ├─ round-1-mission.json
  ├─ round-1-results.zip
  └─ campaign-result.json

┌────────────────────────────────────────────────────────────────────────┐
│  Phase 5: 学习与迭代 (Optional Follow-up)                              │
└────────────────────────────────────────────────────────────────────────┘

campaign-cycle-controller Job (ACK readback)
  │
  ├─ 读取 campaign-result.json
  │
  ├─ 结果分析
  │   ├─ no_candidate → 学习
  │   ├─ capacity_breach → 调整
  │   ├─ negative_ic → 终止
  │   └─ selected → 准备 final eval
  │
  ├─ 生成学习计划 (如果 no_candidate)
  │   mission campaign-learn
  │     --result campaign-result.json
  │     --output next-research-plan.json
  │   • 失败分类
  │   • 生成假设
  │   • 调整策略
  │
  └─ 创建子 Campaign (bounded)
      mission campaign-freeze
        --research-plan next-research-plan.json
      → 回到 Phase 4
  │
  ▼
最多 3 代迭代，或找到候选模型

┌────────────────────────────────────────────────────────────────────────┐
│  Phase 6: 最终评估 (Final Evaluation, 独立窗口)                        │
└────────────────────────────────────────────────────────────────────────┘

campaign-execute Job (--final-evaluation)
  │
  ├─ 冻结模型权重 (不重新训练)
  ├─ 独立选择窗口评估
  ├─ Sealed holdout 测试
  └─ 生成晋升 lineage
  │
  ▼
OSS research/campaigns/<campaign-id>/
  └─ sealed-holdout-result.json
      • promotion_ready → 可晋升
      • holdout_rejected → 终止
```

## 开发工作流

```
┌────────────────────────────────────────────────────────────────────────┐
│  开发者工作流 (使用 DevPod)                                            │
└────────────────────────────────────────────────────────────────────────┘

┌───────────────┐
│ Local Machine │
└───────┬───────┘
        │
        │ 1. 编辑代码
        ▼
    git push origin feat/my-experiment
        │
        │ 2. 在集群拉取 (10秒)
        ▼
┌─────────────────────────────────────────────────┐
│  research-devpod-0 (ACK)                        │
│                                                  │
│  cd /workspace                                  │
│  git checkout feat/my-experiment                │
│  git pull                                       │
│                                                  │
│  ✓ 代码已更新 (10秒)                           │
└───────────────┬─────────────────────────────────┘
                │
                │ 3. 增量编译 (<1分钟)
                ▼
            cargo build -p hft-collector --release
                │
                │ ✓ 编译完成
                ▼
┌─────────────────────────────────────────────────┐
│  测试                                            │
│                                                  │
│  ./target/release/segment-index-cli \\          │
│    --index /lake/output/metadata/segments.parquet │
│    info                                         │
│                                                  │
│  ✓ 功能验证                                     │
└───────────────┬─────────────────────────────────┘
                │
                │ 4. 准备生产镜像
                ▼
┌─────────────────────────────────────────────────┐
│  kaniko-build Job                                │
│                                                  │
│  • 拉取代码                                      │
│  • 原生 amd64 编译                              │
│  • 推送到 ACR                                   │
│                                                  │
│  ✓ 镜像构建完成 (30-60分钟)                    │
└───────────────┬─────────────────────────────────┘
                │
                │ 5. 生产运行
                ▼
┌─────────────────────────────────────────────────┐
│  Production Jobs                                 │
│                                                  │
│  • cex-materialization                          │
│  • campaign-execute                             │
│  • campaign-cycle-controller                    │
│                                                  │
│  ✓ 使用不可变镜像                               │
└─────────────────────────────────────────────────┘

总耗时对比:
  传统方式: 代码修改 → 重建镜像 (60-90分钟) → 测试
  DevPod 方式: 代码修改 → git pull (10秒) → 增量编译 (<1分钟) → 测试
  
  加速: 60-90x
```

## 成本分解

```
┌────────────────────────────────────────────────────────────────────────┐
│  月度成本明细 (CNY)                                                     │
└────────────────────────────────────────────────────────────────────────┘

固定成本（空闲状态）:
  ├─ ECS Collector (包年)           CNY 483
  ├─ ACK System Node (2C4G Spot)    CNY 82-87
  ├─ OSS Storage (~500GB)           CNY 100
  ├─ Block PVC (workspace 20Gi)     CNY 10
  ├─ Block PVC (cargo-cache 10Gi)   CNY 5
  ├─ Block PVC (ledger 10Gi)        CNY 5
  └─ Network/Misc                    CNY 20
  ─────────────────────────────────────────
  Total Fixed                        CNY 705-710

变动成本（按需）:
  ├─ Worker Node (4C16G Spot)       CNY 0.342/hour
  │   • 开发使用 (8h/天 × 20天)   CNY 54.72
  │   • Campaign 运行 (100 hours)  CNY 34.20
  │
  └─ 数据传输 (OSS → ACK)          ~CNY 10
  ─────────────────────────────────────────
  Total Variable (typical)           CNY 98.92

月度总计 (typical):
  CNY 705-710 (fixed) + CNY 98.92 (variable) = CNY 803.92-808.92

对比之前无索引:
  准备时间: 20小时/Campaign × 10 Campaigns = 200 hours
  成本: 200 × CNY 0.342 = CNY 68.40
  
  现在有索引:
  准备时间: 10分钟/Campaign × 10 Campaigns = 1.67 hours
  成本: 1.67 × CNY 0.342 = CNY 0.57
  
  节省: CNY 67.83/月 (仅准备阶段)
```

## 性能指标

```
┌────────────────────────────────────────────────────────────────────────┐
│  关键性能指标                                                          │
└────────────────────────────────────────────────────────────────────────┘

数据采集:
  ├─ Collector 延迟          < 100ms (p99)
  ├─ 段完成时间              ~1 hour (滚动)
  └─ 索引更新                < 1 second

准备阶段 (385小时数据):
  ├─ 段选择 (旧)             20 hours (文件扫描)
  ├─ 段选择 (新)             < 10 minutes (DuckDB 索引)
  ├─ 加速比                  120x
  ├─ PIT 物化                2-4 hours
  └─ 总准备时间              ~3 hours (vs 22 hours)

Campaign 执行:
  ├─ 因子筛选                ~10 minutes
  ├─ Ridge 训练              ~5 minutes
  ├─ CART 训练               ~10 minutes
  ├─ MLP 训练 (optional)     ~30-120 minutes
  ├─ Walk-forward 验证       ~20 minutes
  └─ 总执行时间              ~1-3 hours (per round)

开发迭代:
  ├─ 代码切换 (DevPod)       10 seconds (git pull)
  ├─ 增量编译                < 1 minute
  ├─ 完整编译 (首次)         25-40 minutes
  ├─ 镜像构建 (Kaniko)       30-60 minutes (首次)
  └─ 镜像构建 (缓存)         10-20 minutes

端到端时延:
  ├─ 数据采集 → 可用         ~1 hour (段完成)
  ├─ Campaign 请求 → 结果    ~4-6 hours (准备+执行)
  └─ 开发 → 生产             < 1 hour (DevPod → Kaniko)
```

## 存储分布

```
┌────────────────────────────────────────────────────────────────────────┐
│  存储使用详情                                                          │
└────────────────────────────────────────────────────────────────────────┘

OSS lake/raw/ (~500GB):
  venue=binance_usdm/
    dataset=lob/
      date=2026-09-01/
        hour=00/
          ├─ manifest.json              ~5KB
          ├─ tape.jsonl.zst             ~150MB (compressed)
          └─ _SUCCESS                   ~100B
        hour=01/
          ├─ ...
        ...
      date=2026-09-02/
        ...

OSS lake/output/ (~50GB):
  metadata/
    └─ segments.parquet                 ~50MB (全局索引)
  campaigns/
    campaign-<id>/
      ├─ features.parquet               ~500MB
      ├─ materialization.parquet        ~200MB
      ├─ replay.parquet                 ~100MB
      ├─ campaign-inputs.json           ~10KB
      ├─ campaign-result.json           ~50KB
      └─ rounds/
          ├─ round-0-results.zip        ~20MB
          └─ round-1-results.zip        ~20MB

Block PVC (workspace, 20Gi):
  /workspace/
    ├─ .git/                            ~500MB
    ├─ rust_hft/                        ~200MB (source)
    └─ target/                          ~15GB (build artifacts)

Block PVC (cargo-cache, 10Gi):
  /cargo-cache/
    ├─ registry/                        ~8GB (crates)
    └─ git/                             ~1GB (git deps)

Block PVC (ledger, 10Gi):
  /campaign-root/
    ├─ ledger.duckdb                    ~100MB
    ├─ cycles/                          ~1GB (checkpoints)
    └─ inputs/                          ~2GB (cached receipts)
```

---

## 架构优势总结

✅ **快速迭代**: 代码修改 → 测试，从 60-90 分钟降到 < 1 分钟
✅ **成本优化**: 准备阶段成本降低 99% (CNY 6.84 → CNY 0.06)
✅ **状态持久化**: PVC 保存 workspace 和 cache，重启无损
✅ **多用户支持**: StatefulSet 可扩展到多个独立开发环境
✅ **原生编译**: Kaniko 在集群内原生 amd64 编译，避免 QEMU
✅ **数据就近**: OSS CSI 直接挂载，无需本地下载
✅ **可审计**: 不可变镜像 + 内容哈希 + DuckDB ledger

---

## 下一步优化

1. **自动扩缩容**: 根据 CPU 使用率自动扩展 Worker 节点
2. **成本优化**: 闲时缩容 DevPod，节省 ~CNY 246/月
3. **监控仪表板**: Grafana + Prometheus 实时监控
4. **CI/CD 集成**: GitHub Actions → Kaniko → ACR 自动化
5. **多区域复制**: 支持其他区域的研究需求
