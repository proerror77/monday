# 20+商品 × 15分钟完整数据库压力测试

## 📋 测试概览

这是一个完整的高频交易数据库压力测试套件，用于评估系统在真实市场数据负载下的性能表现。

### 🎯 测试目标

- **25个热门交易对**的实时数据收集
- **15分钟持续运行**的数据库写入测试
- **真实WebSocket数据**从Bitget交易所获取
- **ClickHouse数据库**的高频写入性能评估
- **自动化性能分析**和优化建议生成

### 📊 测试规模

| 项目 | 规模 |
|------|------|
| 交易对数量 | 25个 (BTC, ETH, BNB, XRP, ADA...) |
| 数据通道 | 3个 (OrderBook5, Trades, Ticker) |
| 测试时长 | 15分钟 |
| 预计数据量 | 1,000,000+ 条记录 |
| 并发连接 | 25个WebSocket连接 |
| 数据库写入 | 4个并发写入器 |

## 🚀 快速开始

### 方法1：一键执行完整测试套件

```bash
# 一键执行所有步骤（推荐）
./scripts/run_full_test_suite.sh
```

这将自动完成：
1. 启动测试环境 (ClickHouse, Redis, Prometheus, Grafana)
2. 编译高性能Release版本
3. 运行完整数据库压力测试
4. 数据验证和报告生成

### 方法2：分步执行

```bash
# 1. 启动测试环境
./scripts/start_test_environment.sh

# 2. 编译项目
cargo build --release --example comprehensive_db_stress_test

# 3. 运行测试
./scripts/run_comprehensive_db_test.sh
```

### 方法3：直接运行测试程序

```bash
# 确保ClickHouse运行中
docker-compose up -d clickhouse

# 编译并运行
cargo run --release --example comprehensive_db_stress_test
```

## 🔧 环境要求

### 系统要求

- **内存**: 最少4GB可用内存
- **磁盘**: 最少10GB可用空间
- **网络**: 稳定的互联网连接
- **CPU**: 4核心或以上（推荐）

### 软件依赖

- **Rust**: 1.70+
- **Docker**: 最新版本
- **Docker Compose**: v2.0+

### 服务端口

| 服务 | 端口 | 说明 |
|------|------|------|
| ClickHouse HTTP | 8123 | 数据库查询接口 |
| ClickHouse TCP | 9000 | 原生客户端接口 |
| Redis | 6379 | 缓存服务 |
| Prometheus | 9090 | 监控数据收集 |
| Grafana | 3000 | 可视化仪表板 |

## 📈 性能监控

### 实时监控指标

测试期间会实时显示：

- **连接状态**: 活跃连接数、失败连接数、重连次数
- **数据流性能**: 消息接收率、数据库写入率、吞吐量
- **延迟性能**: 处理延迟、数据库写入延迟
- **队列状态**: 当前队列长度、最大队列长度
- **按商品统计**: 各交易对的消息数量和延迟

### 监控报告间隔

- **实时输出**: 每30秒输出详细统计
- **进度报告**: 每1000条消息报告进度
- **最终报告**: 测试结束时生成完整分析

## 📊 结果分析

### 自动生成报告

测试完成后会自动生成：

1. **综合性能报告** (`comprehensive_test_report.md`)
2. **详细测试日志** (`stress_test.log`)
3. **数据统计分析** (`data_by_symbol.txt`, `data_by_channel.txt`)
4. **系统信息记录** (`system_info.txt`)

### 性能评级标准

| 等级 | 消息处理率 | 写入成功率 | 错误率 |
|------|------------|------------|--------|
| A+ 优秀 | >50,000 msg/s | >95% | <0.1% |
| A 良好 | >20,000 msg/s | >90% | <1% |
| B 中等 | >5,000 msg/s | >80% | <5% |
| C 需优化 | <5,000 msg/s | <80% | >5% |

### 自动化优化建议

系统会根据测试结果自动提供：

- **吞吐量优化**: 网络、解析、并发调优建议
- **写入性能优化**: 批量大小、写入器数量调整
- **错误处理优化**: 重试机制、连接池配置
- **系统级优化**: 内存、CPU、磁盘I/O优化

## 🛠️ 配置调优

### 测试参数配置

编辑 `config/test/comprehensive_test.yaml`:

```yaml
test_config:
  test_duration_minutes: 15        # 测试时长
  target_symbols_count: 25         # 商品数量
  batch_config:
    batch_size: 500                # 批量大小
    writer_count: 4                # 写入器数量
  performance_thresholds:
    min_message_rate_per_sec: 5000 # 最低消息率
    max_error_rate_percent: 1.0    # 最大错误率
```

### 高性能优化选项

```bash
# 编译时启用所有优化
RUSTFLAGS="-C target-cpu=native -C target-feature=+avx2,+fma" \
cargo build --release

# 运行时优化
export RUST_LOG=info
export RUST_BACKTRACE=1
```

## 🐛 问题排查

### 常见问题

1. **ClickHouse连接失败**
   ```bash
   # 检查服务状态
   docker-compose ps clickhouse
   
   # 重启服务
   docker-compose restart clickhouse
   ```

2. **内存不足**
   ```bash
   # 检查内存使用
   docker stats
   
   # 减少并发数或批量大小
   ```

3. **网络连接问题**
   ```bash
   # 测试网络连接
   curl -I https://ws.bitget.com
   
   # 检查防火墙设置
   ```

### 日志分析

```bash
# 查看详细日志
tail -f test_results/*/stress_test.log

# 查看错误信息
grep ERROR test_results/*/stress_test.log

# 查看性能统计
grep "详细性能统计" test_results/*/stress_test.log -A 20
```

## 📱 服务访问

测试运行期间，可以访问以下服务：

- **Grafana仪表板**: http://localhost:3000 (admin/admin)
- **Prometheus监控**: http://localhost:9090
- **ClickHouse查询**: http://localhost:8123

### ClickHouse查询示例

```sql
-- 查看总记录数
SELECT count() FROM hft.market_data_15min;

-- 按商品统计
SELECT symbol, count() as records 
FROM hft.market_data_15min 
GROUP BY symbol 
ORDER BY records DESC;

-- 查看最新数据
SELECT * FROM hft.market_data_15min 
ORDER BY timestamp DESC 
LIMIT 10;
```

## 🎯 下一步优化

基于测试结果，可以考虑：

1. **扩展测试规模**: 增加更多商品或延长测试时间
2. **优化系统参数**: 根据建议调整配置
3. **集成机器学习**: 添加实时模型推理
4. **监控告警**: 设置性能阈值告警
5. **高可用部署**: 实现多实例负载均衡

---

**注意**: 这是一个真实的高频交易数据测试，会产生大量网络流量和数据库写入。请确保在合适的网络环境下运行。
