# 20+商品 × 15分钟数据库压力测试报告 (兼容版)

**测试时间**: Mon Jul 21 23:24:17 CST 2025
**结果目录**: ./test_results/compatible_db_test_20250721_232229
**系统架构**: arm64
**总记录数**: Code: 60. DB::Exception: Table hft.market_data_15min does not exist. (UNKNOWN_TABLE) (version 23.8.16.16 (official build))

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
测试开始时间: Mon Jul 21 23:24:16 CST 2025
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
```

#### 自动化分析
```
```

### 数据验证结果

- **总记录数**: Code: 60. DB::Exception: Table hft.market_data_15min does not exist. (UNKNOWN_TABLE) (version 23.8.16.16 (official build))

---
**兼容版测试报告生成时间**: Mon Jul 21 23:24:17 CST 2025
