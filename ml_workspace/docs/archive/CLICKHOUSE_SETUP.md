# ClickHouse 連接設置指南

## ✅ AWS ClickHouse Cloud 配置 (推薦)

您的 AWS ClickHouse Cloud 實例已配置：

```python
# AWS ClickHouse Cloud 連接配置
CLICKHOUSE_CONFIG = {
    'host': 'https://ivigyu08to.ap-northeast-1.aws.clickhouse.cloud:8443',
    'username': 'default',
    'password': 'sIiFK.4ygf.9R',
    'database': 'hft',
    'secure': True  # AWS ClickHouse Cloud 需要 SSL
}
```

## 立即測試 AWS 連接

### 步驟1: 測試 AWS ClickHouse Cloud 連接

1. **運行連接測試**
   ```bash
   cd /Users/proerror/Documents/monday/ml_workspace
   python test_aws_clickhouse.py
   ```

2. **如果連接成功，測試 CLI 工具**
   ```bash
   # 查看特徵列表
   python cli_tcn_gru.py features --verbose
   
   # 使用真實數據訓練（如果有數據）
   python cli_tcn_gru.py train --symbol WLFIUSDT --use_taker --epochs 5
   ```


### 步驟2: 檢查特徵數據

AWS ClickHouse 中的35特徵表可能命名為：
- `features_37` (包含35個特徵 + mid_price + exchange_ts)
- `mlofi_features`
- `order_book_features`

檢查特徵數據：
```bash
python -c "
from utils.clickhouse_client import ClickHouseClient
client = ClickHouseClient(
    host='https://ivigyu08to.ap-northeast-1.aws.clickhouse.cloud:8443',
    username='default',
    password='sIiFK.4ygf.9R',
    database='hft'
)
tables = client.show_tables()
print('所有表：', tables)
"
```

## 如果連接成功，你可以使用以下 CLI 工具：

### 1. 查看特徵系統
```bash
python cli_tcn_gru.py features --verbose
```

### 2. 訓練模型（使用真實數據）
```bash
# Taker策略
python cli_tcn_gru.py train --symbol WLFIUSDT --use_taker --epochs 5

# Maker策略  
python cli_tcn_gru.py train --symbol WLFIUSDT --use_maker --epochs 5
```


## 故障排除

### 如果 AWS ClickHouse 連接失敗：

1. **檢查網路連接**
   ```bash
   curl -I https://ivigyu08to.ap-northeast-1.aws.clickhouse.cloud:8443
   ```

2. **檢查憑證**
   - 用戶名：default
   - 密碼：sIiFK.4ygf.9R
   - 數據庫：hft

3. **如果找不到35特徵數據**
   - 需要先设置并填充特征表
   - 或者联系数据工程师确保特征数据可用

## 下一步

✅ **立即可用的工具：**
- `test_aws_clickhouse.py` - 測試連接
- `cli_tcn_gru.py` - TCN-GRU 無監督訓練（需要真實數據）

📊 **訓練流程：**
1. 測試連接 → 檢查數據 → 確認35特徵表可用
2. 使用 `cli_tcn_gru.py` 進行無監督訓練
3. 如缺少特徵數據，需要先运行特征工程流程
