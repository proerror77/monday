# 20+商品 × 15分钟数据库压力测试报告 (兼容版)

**测试时间**: Tue Jul 22 09:47:05 CST 2025
**结果目录**: ./test_results/compatible_db_test_20250722_093005
**系统架构**: arm64
**总记录数**: 309492

## 📊 测试概况

### 测试配置
- **测试时长**: 15 分钟
- **目标商品**: 25 个热门交易对
- **数据通道**: OrderBook5, Trades, Ticker
- **数据库**: ClickHouse (hft.market_data_15min)
- **批量大小**: 500 记录/批次
- **并发写入**: 4 个写入器
- **服务模式**: 兼容版 (仅ClickHouse + Redis)
- **优化级别**: 标准Release优化 (无AVX2/FMA)

### 系统信息
```
=== 兼容版数据库测试系统信息 ===
测试开始时间: Tue Jul 22 09:31:55 CST 2025
主机名: sonics-MacBook-Air.local
操作系统: Darwin sonics-MacBook-Air.local 24.5.0 Darwin Kernel Version 24.5.0: Tue Apr 22 19:54:33 PDT 2025; root:xnu-11417.121.6~2/RELEASE_ARM64_T8122 arm64
系统架构: arm64
Rust 版本: rustc 1.88.0 (6b00bc388 2025-06-23)
Cargo 版本: cargo 1.88.0 (873a06493 2025-05-10)
ClickHouse 版本: 23.8.16.16

=== CPU 信息 (macOS) ===
Apple M3
CPU 核心数: 8

=== 内存信息 (macOS) ===
Mach Virtual Memory Statistics: (page size of 16384 bytes)
```

### 测试结果
#### 性能指标
```
[2m2025-07-22T01:46:56.514754Z[0m [32m INFO[0m [2mcomprehensive_db_stress_test[0m[2m:[0m 🔗 连接状态:
[2m2025-07-22T01:46:56.514759Z[0m [32m INFO[0m [2mcomprehensive_db_stress_test[0m[2m:[0m    活跃连接: 25
[2m2025-07-22T01:46:56.514782Z[0m [32m INFO[0m [2mcomprehensive_db_stress_test[0m[2m:[0m    失败连接: 0
[2m2025-07-22T01:46:56.514787Z[0m [32m INFO[0m [2mcomprehensive_db_stress_test[0m[2m:[0m    重连次数: 0
[2m2025-07-22T01:46:56.514790Z[0m [32m INFO[0m [2mcomprehensive_db_stress_test[0m[2m:[0m 📡 数据流性能:
[2m2025-07-22T01:46:56.514794Z[0m [32m INFO[0m [2mcomprehensive_db_stress_test[0m[2m:[0m    接收消息: 77373 (86.0 msg/s)
[2m2025-07-22T01:46:56.514800Z[0m [32m INFO[0m [2mcomprehensive_db_stress_test[0m[2m:[0m    写入记录: 309492 (343.9 rec/s)
[2m2025-07-22T01:46:56.514804Z[0m [32m INFO[0m [2mcomprehensive_db_stress_test[0m[2m:[0m    写入批次: 892
[2m2025-07-22T01:46:56.514807Z[0m [32m INFO[0m [2mcomprehensive_db_stress_test[0m[2m:[0m    数据吞吐: 40.14 MB (0.04 MB/s)
[2m2025-07-22T01:46:56.514811Z[0m [32m INFO[0m [2mcomprehensive_db_stress_test[0m[2m:[0m ⚡ 延迟性能:
[2m2025-07-22T01:46:56.514814Z[0m [32m INFO[0m [2mcomprehensive_db_stress_test[0m[2m:[0m    处理延迟: 4.9μs 平均, 0μs 最小, 13343μs 最大
[2m2025-07-22T01:46:56.514818Z[0m [32m INFO[0m [2mcomprehensive_db_stress_test[0m[2m:[0m    数据库延迟: 12519.8μs 平均
[2m2025-07-22T01:46:56.514822Z[0m [32m INFO[0m [2mcomprehensive_db_stress_test[0m[2m:[0m 🗄️  数据库性能:
[2m2025-07-22T01:46:56.514825Z[0m [32m INFO[0m [2mcomprehensive_db_stress_test[0m[2m:[0m    写入成功: 892
[2m2025-07-22T01:46:56.514828Z[0m [32m INFO[0m [2mcomprehensive_db_stress_test[0m[2m:[0m    写入错误: 0
```

#### 自动化分析
```
```

### 数据验证结果

- **总记录数**: 309492

#### 按商品分布
```
BTCUSDT	31384	1753147916853544	1753148335750124
SOLUSDT	19684	1753147918291387	1753148347175040
ETHUSDT	17044	1753147917093021	1753148303809288
DOGEUSDT	15464	1753147918610607	1753148308675986
XRPUSDT	14656	1753147917777121	1753148319944529
ADAUSDT	14288	1753147918022888	1753148317122264
LINKUSDT	14164	1753147919205368	1753148337693834
WAVESUSDT	13832	1753147924020957	1753148368754378
XLMUSDT	13468	1753147920521620	1753148369151689
SHIBUSDT	13260	1753147922772152	1753148347181136
```

#### 按通道分布
```
OrderBook5	149080
Ticker	120448
Trades	39964
```

#### 数据库存储信息
```
18.40 MiB	155.25 MiB	73
```

---
**兼容版测试报告生成时间**: Tue Jul 22 09:47:05 CST 2025
