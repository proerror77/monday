# 🎯 LOB-Native HFT系统架构蓝图

## 📊 核心范式转变：从OHLCV到订单簿级别

### ❌ 传统OHLCV方法的根本局限

```python
# 传统方法的问题
traditional_approach = {
    "数据粒度": "OHLCV蜡烛图（有损下采样）",
    "特征空间": "技术指标（SMA, RSI, MACD）",
    "时间分辨率": "分钟级/秒级",
    "预测目标": "价格方向（粗糙）",
    "信息损失": "巨大 - 丢失所有微观市场结构"
}
```

### ✅ LOB-Native方法的革命性优势

```python
# LOB-Native的核心优势
lob_native_approach = {
    "数据粒度": "事件级消息 (add, modify, cancel, execute)",
    "特征空间": "队列/流量变量 (OFI, QI, 深度斜率)",
    "时间分辨率": "纳秒级时间戳",
    "预测目标": "microprice漂移，执行P&L",
    "信息优势": "完整的微观市场结构信息"
}
```

---

## 1. 数据工程架构 (Event-Level Ingestion)

### 1.1. 数据源与重构

```python
@dataclass
class LOBMessage:
    """LOB事件消息结构"""
    timestamp: int  # 纳秒级时间戳
    message_type: str  # add, modify, cancel, execute
    order_id: int
    side: str  # bid/ask
    price: Decimal
    size: int
    level: Optional[int] = None

@dataclass
class LOBSnapshot:
    """LOB快照结构"""
    timestamp: int
    bids: List[Tuple[Decimal, int]]  # (price, size) pairs
    asks: List[Tuple[Decimal, int]]
    mid_price: Decimal
    microprice: Decimal  # 向较薄队列倾斜的mid
    spread: Decimal
```

### 1.2. 数据源接入

```python
class LOBDataIngestion:
    """LOB数据接入系统"""

    def __init__(self):
        self.feeds = {
            "NASDAQ_ITCH": ITCHParser(),
            "LOBSTER": LOBSTERReconstructor(),
            "TotalView": TotalViewDecoder()
        }

    def ingest_itch_feed(self, binary_stream: bytes) -> Iterator[LOBMessage]:
        """解码ITCH二进制流，重建完整深度和订单生命周期"""
        for message in self.feeds["NASDAQ_ITCH"].decode(binary_stream):
            # 纳秒级时间戳对齐
            aligned_message = self.align_timestamps(message)
            yield aligned_message

    def build_information_driven_bars(self, messages: List[LOBMessage]) -> List[Bar]:
        """构建信息驱动的bar（tick/volume/dollar bars）"""
        # 使用日均成交额的1/50作为起始阈值
        threshold = self.calculate_daily_volume() / 50
        return self.create_dollar_bars(messages, threshold)
```

### 1.3. 时钟系统

```python
class EventTimeClock:
    """事件时间时钟系统"""

    def __init__(self):
        self.event_time = True  # 保持数据在事件时间
        self.bar_types = ["tick", "volume", "dollar"]

    def regularize_sampling(self, events: List[LOBMessage]) -> List[RegularizedEvent]:
        """规范化采样，减少异方差性"""
        return self.create_information_bars(events)

    def enforce_monotone_sequence(self, events: List[LOBMessage]) -> List[LOBMessage]:
        """强制单调价格梯度，调和取消不匹配"""
        cleaned_events = []
        for event in events:
            if self.validate_price_ladder(event):
                cleaned_events.append(event)
        return cleaned_events
```

---

## 2. 微观结构感知标签系统

### 2.1. 短期预测目标

```python
class MicrostructureLabeler:
    """微观结构感知的标签生成器"""

    def create_microprice_labels(self, lob_snapshots: List[LOBSnapshot],
                                horizon_events: int = 100) -> pd.Series:
        """
        生成microprice变化标签
        microprice向较薄队列倾斜，是标准的HFT估计器
        """
        labels = []
        for i in range(len(lob_snapshots) - horizon_events):
            current_microprice = lob_snapshots[i].microprice
            future_microprice = lob_snapshots[i + horizon_events].microprice

            # 计算microprice漂移
            drift = (future_microprice - current_microprice) / current_microprice
            labels.append(drift)

        return pd.Series(labels)

    def create_triple_barrier_labels(self, prices: pd.Series,
                                   tp_threshold: float = 0.001,
                                   sl_threshold: float = 0.001,
                                   time_threshold: int = 1000) -> pd.Series:
        """
        三重屏障标签，与meta-labeling配对
        使用Purged K-Fold + Embargo验证以消除重叠泄漏
        """
        labels = []
        for i in range(len(prices)):
            entry_price = prices.iloc[i]

            # 寻找退出条件
            for j in range(i+1, min(i+time_threshold, len(prices))):
                current_price = prices.iloc[j]
                return_pct = (current_price - entry_price) / entry_price

                if return_pct >= tp_threshold:
                    labels.append(1)  # 止盈
                    break
                elif return_pct <= -sl_threshold:
                    labels.append(-1)  # 止损
                    break
            else:
                labels.append(0)  # 时间退出

        return pd.Series(labels)
```

### 2.2. 执行P&L目标

```python
class ExecutionPnLLabeler:
    """执行P&L标签生成器"""

    def calculate_execution_pnl(self, entry_tick: LOBSnapshot,
                               execution_sequence: List[LOBMessage],
                               costs: Dict[str, float]) -> float:
        """
        计算考虑成本/延迟的执行P&L
        """
        total_pnl = 0.0

        for message in execution_sequence:
            if message.message_type == "execute":
                # 计算执行价格 vs 入场价格的差异
                price_diff = message.price - entry_tick.mid_price

                # 扣除交易成本
                fees = costs.get("fees", 0.0001)
                spread_cost = entry_tick.spread / 2
                impact_cost = self.estimate_market_impact(message)

                net_pnl = price_diff - fees - spread_cost - impact_cost
                total_pnl += net_pnl

        return total_pnl
```

---

## 3. LOB-Native AutoAlpha系统

### 3.1. LOB领域特定语言 (DSL)

```python
class LOBOperatorSpace:
    """LOB操作符空间定义"""

    def __init__(self):
        # 叶节点：基础LOB特征
        self.leaves = [
            "bid_px[i]", "ask_px[i]", "bid_sz[i]", "ask_sz[i]",  # i=1..K级别
            "add_rate", "cancel_rate", "mkt_rate"  # 窗口W内的消息率
        ]

        # 一元操作符
        self.unary_ops = [
            "zscore", "ema", "std", "ts_rank", "diff", "clip", "log1p"
        ]

        # 二元操作符
        self.binary_ops = [
            "+", "-", "*", "/", "min", "max", "corr", "cov"
        ]

        # LOB特定组合器
        self.lob_combinators = {
            "queue_imbalance": self.qi_operator,
            "order_flow_imbalance": self.ofi_operator,
            "depth_slope": self.depth_slope_operator,
            "resilience": self.resilience_operator,
            "cancel_limit_ratio": self.cl_ratio_operator
        }

    def qi_operator(self, k: int) -> str:
        """
        队列不平衡算子
        QI_k = (Σ_{i<=k} bid_sz[i] - Σ_{i<=k} ask_sz[i]) / (Σ_{i<=k} bid_sz[i] + Σ_{i<=k} ask_sz[i])
        """
        return f"qi_level_{k}"

    def ofi_operator(self, window: int) -> str:
        """订单流不平衡算子"""
        return f"ofi_window_{window}"

    def depth_slope_operator(self, levels: int) -> str:
        """深度斜率算子"""
        return f"depth_slope_{levels}"
```

### 3.2. 符号回归 + 质量多样性搜索

```python
class LOBAlphaSearchEngine:
    """LOB Alpha搜索引擎"""

    def __init__(self):
        self.operator_space = LOBOperatorSpace()
        self.complexity_penalty = 0.001
        self.turnover_penalty = 0.01
        self.cost_penalty = 0.005

    def symbolic_regression_search(self, lob_data: List[LOBSnapshot],
                                 labels: pd.Series) -> List[AlphaExpression]:
        """
        符号回归 + 遗传编程搜索
        使用MAP-Elites保持多样化高质量alpha组合
        """
        # 初始化MAP-Elites网格
        map_elites = MAPElitesGrid(
            dimensions=["complexity", "half_life", "turnover"],
            bins_per_dim=10
        )

        population = self.initialize_population()

        for generation in range(self.max_generations):
            for individual in population:
                # 评估alpha表达式
                alpha_values = self.evaluate_expression(individual, lob_data)

                # 计算适应度（IC/IR + 稳定性 + 成本感知可交易性）
                ic = self.calculate_ic(alpha_values, labels)
                ir = self.calculate_ir(alpha_values, labels)
                stability = self.calculate_stability(alpha_values)
                tradability = self.calculate_cost_aware_tradability(alpha_values)

                fitness = ic * ir * stability * tradability
                fitness -= self.complexity_penalty * individual.complexity
                fitness -= self.turnover_penalty * individual.turnover

                # 更新MAP-Elites网格
                map_elites.update(individual, fitness)

            # 质量多样性选择
            population = map_elites.select_diverse_population()

        return map_elites.get_best_alphas()

    def create_lob_expression(self) -> AlphaExpression:
        """创建LOB特定的alpha表达式"""
        # 示例：rank(qi_1) / ema(ofi_10, 20) + depth_slope_5
        operators = random.sample(self.operator_space.lob_combinators.keys(), 2)
        expression = self.combine_operators(operators)
        return AlphaExpression(expression)
```

### 3.3. 约束与质量控制

```python
class LOBAlphaConstraints:
    """LOB Alpha约束系统"""

    def __init__(self):
        self.type_safety = True  # 事件 vs 快照操作
        self.max_complexity = 50  # 节点/参数数量
        self.max_turnover = 10.0  # 日换手率限制
        self.min_novelty = 0.1  # 与基准特征的最小相关性

    def validate_expression(self, expression: AlphaExpression) -> bool:
        """验证表达式约束"""
        checks = [
            self.check_type_safety(expression),
            self.check_complexity_limit(expression),
            self.check_turnover_penalty(expression),
            self.check_novelty_score(expression)
        ]
        return all(checks)

    def check_type_safety(self, expr: AlphaExpression) -> bool:
        """确保事件和快照操作不混用"""
        event_ops = expr.get_event_operations()
        snapshot_ops = expr.get_snapshot_operations()
        return not (event_ops and snapshot_ops)

    def penalize_complexity(self, expr: AlphaExpression) -> float:
        """复杂度惩罚"""
        node_count = expr.count_nodes()
        param_count = expr.count_parameters()
        return self.complexity_penalty * (node_count + param_count)
```

---

## 4. DeepLOB表示学习

### 4.1. CNN+LSTM架构

```python
class DeepLOBModel(nn.Module):
    """
    DeepLOB风格的CNN+LSTM架构
    将订单簿视为2D张量（级别×特征）随时间变化
    在FI-2010数据集上验证的mid-price移动基准
    """

    def __init__(self, num_levels: int = 10, num_features: int = 4,
                 conv_filters: List[int] = [32, 32, 32],
                 lstm_hidden: int = 64):
        super().__init__()

        # CNN层处理LOB空间结构
        self.conv_layers = nn.ModuleList([
            nn.Conv2d(1, conv_filters[0], kernel_size=(1, 2), stride=(1, 2)),
            nn.Conv2d(conv_filters[0], conv_filters[1], kernel_size=(4, 1)),
            nn.Conv2d(conv_filters[1], conv_filters[2], kernel_size=(4, 1))
        ])

        # LSTM/TCN处理时间依赖
        self.lstm = nn.LSTM(
            input_size=conv_filters[-1] * self._calc_conv_output_size(),
            hidden_size=lstm_hidden,
            batch_first=True
        )

        # 成本感知损失的分类头
        self.classifier = nn.Linear(lstm_hidden, 3)  # up/stay/down

    def forward(self, lob_tensor: torch.Tensor) -> torch.Tensor:
        """
        Args:
            lob_tensor: [batch, time, levels, features] LOB张量
        Returns:
            price_direction_probs: [batch, 3] 价格方向概率
        """
        batch_size, seq_len = lob_tensor.shape[:2]

        # 重塑为CNN输入: [batch*time, 1, levels, features]
        x = lob_tensor.view(-1, 1, *lob_tensor.shape[2:])

        # CNN特征提取
        for conv in self.conv_layers:
            x = F.relu(conv(x))

        # 展平并重塑为序列: [batch, time, features]
        x = x.view(batch_size, seq_len, -1)

        # LSTM时序建模
        lstm_out, _ = self.lstm(x)

        # 最后时间步的预测
        final_output = lstm_out[:, -1, :]

        return self.classifier(final_output)

    def cost_aware_loss(self, predictions: torch.Tensor,
                       targets: torch.Tensor,
                       spread_costs: torch.Tensor) -> torch.Tensor:
        """
        成本感知损失函数
        根据spread/impact调整类权重
        """
        # 计算基础交叉熵损失
        base_loss = F.cross_entropy(predictions, targets, reduction='none')

        # 根据成本调整权重
        cost_weights = 1.0 / (1.0 + spread_costs)
        weighted_loss = base_loss * cost_weights

        return weighted_loss.mean()
```

### 4.2. Transformer架构

```python
class LOBTransformer(nn.Module):
    """用于更长上下文的Transformer架构"""

    def __init__(self, num_levels: int = 10, num_features: int = 4,
                 d_model: int = 128, nhead: int = 8, num_layers: int = 6):
        super().__init__()

        # LOB特征嵌入
        self.lob_embedding = nn.Linear(num_levels * num_features, d_model)

        # 位置编码
        self.pos_encoding = PositionalEncoding(d_model)

        # Transformer编码器
        encoder_layer = nn.TransformerEncoderLayer(
            d_model=d_model,
            nhead=nhead,
            dim_feedforward=d_model * 4,
            dropout=0.1
        )
        self.transformer = nn.TransformerEncoder(encoder_layer, num_layers)

        # 输出层
        self.output_layer = nn.Linear(d_model, 1)  # 回归目标

    def forward(self, lob_sequence: torch.Tensor) -> torch.Tensor:
        """
        Args:
            lob_sequence: [batch, seq_len, levels, features]
        Returns:
            price_predictions: [batch, seq_len, 1]
        """
        batch_size, seq_len = lob_sequence.shape[:2]

        # 展平LOB特征
        x = lob_sequence.view(batch_size, seq_len, -1)

        # 特征嵌入
        x = self.lob_embedding(x)

        # 位置编码
        x = self.pos_encoding(x)

        # Transformer编码
        # 转置为[seq_len, batch, d_model]
        x = x.transpose(0, 1)
        x = self.transformer(x)
        x = x.transpose(0, 1)

        # 输出预测
        predictions = self.output_layer(x)

        return predictions
```

---

## 5. 强化学习系统 (两个产品)

### 5.1. 市场制造 (Market Making)

```python
class MarketMakingEnv(gym.Env):
    """市场制造环境"""

    def __init__(self, lob_simulator):
        self.lob_simulator = lob_simulator
        self.inventory_limit = 100
        self.max_spread = 0.01

        # 状态空间：多层订单簿 + 队列位置代理 + 库存 + 波动率/冲击估计
        self.observation_space = spaces.Box(
            low=-np.inf, high=np.inf,
            shape=(50,),  # 10级别*4特征 + 库存 + 其他状态
            dtype=np.float32
        )

        # 动作空间：报价放置（价格偏移，大小）和取消
        self.action_space = spaces.Box(
            low=np.array([-0.01, 0, -0.01, 0, 0]),  # [bid_offset, bid_size, ask_offset, ask_size, cancel]
            high=np.array([0.01, 100, 0.01, 100, 1]),
            dtype=np.float32
        )

    def step(self, action):
        """执行市场制造动作"""
        bid_offset, bid_size, ask_offset, ask_size, cancel_flag = action

        # 执行报价动作
        if cancel_flag > 0.5:
            self.cancel_all_orders()
        else:
            self.place_quotes(bid_offset, bid_size, ask_offset, ask_size)

        # 模拟市场演进
        next_state, fills, adverse_selection = self.lob_simulator.step()

        # 计算奖励：价差捕获 - 库存风险 - 逆选择 - 成本
        spread_capture = self.calculate_spread_capture(fills)
        inventory_risk = self.calculate_inventory_risk()
        costs = self.calculate_transaction_costs(fills)

        reward = spread_capture - inventory_risk - adverse_selection - costs

        # 检查终止条件
        done = abs(self.inventory) > self.inventory_limit

        return next_state, reward, done, {}

class AvellanedaStoikovBaseline:
    """Avellaneda-Stoikov基准策略"""

    def __init__(self, risk_aversion: float = 0.1):
        self.gamma = risk_aversion  # 风险厌恶参数

    def optimal_spread(self, volatility: float, inventory: float,
                      remaining_time: float) -> Tuple[float, float]:
        """计算最优bid-ask价差"""
        reservation_price = self.calculate_reservation_price(inventory, remaining_time)
        optimal_spread = self.gamma * volatility * remaining_time

        bid_price = reservation_price - optimal_spread / 2
        ask_price = reservation_price + optimal_spread / 2

        return bid_price, ask_price
```

### 5.2. 执行优化 (Execution)

```python
class ExecutionEnv(gym.Env):
    """执行优化环境"""

    def __init__(self, target_volume: int, time_horizon: int):
        self.target_volume = target_volume
        self.time_horizon = time_horizon
        self.remaining_volume = target_volume
        self.remaining_time = time_horizon

        # 状态：剩余数量 + 剩余时间 + 订单簿/流状态 + 成交统计
        self.observation_space = spaces.Box(
            low=-np.inf, high=np.inf,
            shape=(30,),  # LOB状态 + 执行状态
            dtype=np.float32
        )

        # 动作：现在切片vs等待 + limit vs market + 价格偏移
        self.action_space = spaces.Box(
            low=np.array([0, 0, -0.01]),  # [slice_ratio, order_type, price_offset]
            high=np.array([1, 1, 0.01]),
            dtype=np.float32
        )

    def step(self, action):
        slice_ratio, order_type, price_offset = action

        # 确定订单大小
        order_size = int(self.remaining_volume * slice_ratio)

        # 执行订单
        if order_type > 0.5:  # market order
            fill_price, fill_size = self.execute_market_order(order_size)
        else:  # limit order
            fill_price, fill_size = self.execute_limit_order(order_size, price_offset)

        # 更新状态
        self.remaining_volume -= fill_size
        self.remaining_time -= 1

        # 计算奖励：实施短缺 vs Almgren-Chriss基准
        implementation_shortfall = self.calculate_shortfall(fill_price, fill_size)
        benchmark_cost = self.almgren_chriss_benchmark()

        reward = benchmark_cost - implementation_shortfall

        done = self.remaining_volume <= 0 or self.remaining_time <= 0

        return self.get_state(), reward, done, {}

class AlmgrenChrissBaseline:
    """Almgren-Chriss执行基准"""

    def __init__(self, risk_aversion: float = 1e-6):
        self.lambda_risk = risk_aversion

    def optimal_trajectory(self, total_volume: int, time_horizon: int,
                         volatility: float, impact_params: Dict) -> List[int]:
        """计算最优执行轨迹"""
        # Almgren-Chriss闭式解
        kappa = impact_params['temporary_impact']
        eta = impact_params['permanent_impact']

        # 计算最优参数
        k = np.sqrt(self.lambda_risk * volatility**2 / kappa)

        # 生成最优执行时间表
        trajectory = []
        for t in range(time_horizon):
            remaining_time = time_horizon - t
            optimal_rate = k * remaining_time / time_horizon
            trajectory.append(int(total_volume * optimal_rate))

        return trajectory
```

---

## 6. 验证纪律 (关键的防止过拟合)

### 6.1. Purged K-Fold + Embargo

```python
class PurgedKFold:
    """净化K折交叉验证，防止重叠泄漏"""

    def __init__(self, n_splits: int = 5, embargo_days: int = 1):
        self.n_splits = n_splits
        self.embargo_days = embargo_days

    def split(self, timestamps: pd.DatetimeIndex,
             labels: pd.Series) -> Iterator[Tuple[np.ndarray, np.ndarray]]:
        """
        生成净化的训练/验证分割
        确保验证集前后都有embargo期间
        """
        fold_size = len(timestamps) // self.n_splits

        for i in range(self.n_splits):
            # 验证集索引
            val_start = i * fold_size
            val_end = (i + 1) * fold_size
            val_indices = np.arange(val_start, val_end)

            # 训练集索引（排除embargo）
            embargo_before = val_start - self.embargo_days
            embargo_after = val_end + self.embargo_days

            train_indices = np.concatenate([
                np.arange(0, max(0, embargo_before)),
                np.arange(min(len(timestamps), embargo_after), len(timestamps))
            ])

            yield train_indices, val_indices

class CombinatorialPurgedCV:
    """组合净化交叉验证 (CPCV)"""

    def __init__(self, n_splits: int = 5, n_test_groups: int = 2):
        self.n_splits = n_splits
        self.n_test_groups = n_test_groups

    def split(self, timestamps: pd.DatetimeIndex) -> Iterator[Tuple[np.ndarray, np.ndarray]]:
        """
        生成所有可能的训练/测试组合
        用于多配置扫描时的严格验证
        """
        from itertools import combinations

        # 将数据分成组
        group_size = len(timestamps) // self.n_splits
        groups = [np.arange(i * group_size, (i + 1) * group_size)
                 for i in range(self.n_splits)]

        # 生成所有可能的测试组合
        for test_groups in combinations(range(self.n_splits), self.n_test_groups):
            test_indices = np.concatenate([groups[i] for i in test_groups])
            train_indices = np.concatenate([groups[i] for i in range(self.n_splits)
                                          if i not in test_groups])
            yield train_indices, test_indices
```

### 6.2. 成本模型与模拟器

```python
class CostModel:
    """交易成本模型"""

    def __init__(self):
        self.fee_rate = 0.0001  # 手续费率
        self.spread_multiplier = 0.5  # 价差成本倍数

    def calculate_total_cost(self, trades: List[Trade],
                           market_impact: MarketImpactModel) -> float:
        """
        计算总成本 = 手续费 + 价差 + 临时/永久冲击
        """
        total_cost = 0.0

        for trade in trades:
            # 手续费
            fees = trade.volume * trade.price * self.fee_rate

            # 价差成本
            spread_cost = trade.volume * trade.spread * self.spread_multiplier

            # 市场冲击
            temporary_impact = market_impact.temporary_impact(trade)
            permanent_impact = market_impact.permanent_impact(trade)

            total_cost += fees + spread_cost + temporary_impact + permanent_impact

        return total_cost

class LOBSimulator:
    """高保真LOB模拟器"""

    def __init__(self, historical_data: List[LOBSnapshot]):
        self.historical_data = historical_data
        self.queue_priority_model = QueuePriorityModel()
        self.hawkes_process = HawkesOrderFlow()

    def simulate_execution(self, orders: List[Order]) -> List[Fill]:
        """
        模拟订单执行，考虑队列优先级和取消
        """
        fills = []

        for order in orders:
            # 模拟队列位置
            queue_position = self.queue_priority_model.estimate_position(order)

            # 模拟到达流和执行概率
            arrival_rate = self.hawkes_process.intensity(order.timestamp)
            execution_prob = self.calculate_execution_probability(
                order, queue_position, arrival_rate
            )

            if np.random.random() < execution_prob:
                fill = Fill(
                    timestamp=order.timestamp,
                    price=order.price,
                    size=order.size,
                    latency=self.simulate_latency()
                )
                fills.append(fill)

        return fills
```

---

## 7. 生产架构 (Rust + Python)

### 7.1. Rust核心组件

```rust
// LOB引擎 (Rust)
use polars::prelude::*;
use rayon::prelude::*;

pub struct LOBEngine {
    levels: Vec<PriceLevel>,
    message_queue: RingBuffer<LOBMessage>,
    feature_evaluator: FeatureEvaluator,
}

impl LOBEngine {
    pub fn process_message(&mut self, message: LOBMessage) -> Option<LOBSnapshot> {
        // 更新订单簿状态
        self.update_book_state(&message);

        // 计算实时特征
        let features = self.feature_evaluator.compute_features(&self.levels);

        // 返回快照
        Some(LOBSnapshot {
            timestamp: message.timestamp,
            bid_levels: self.get_bid_levels(),
            ask_levels: self.get_ask_levels(),
            features,
        })
    }

    pub fn evaluate_alpha_expression(&self, expression: &str) -> Result<f64, String> {
        // DSL表达式评估器
        self.feature_evaluator.evaluate_expression(expression)
    }
}

// ITCH解析器
pub struct ITCHParser {
    buffer: Vec<u8>,
    message_types: HashMap<u8, MessageType>,
}

impl ITCHParser {
    pub fn parse_feed(&mut self, data: &[u8]) -> Vec<LOBMessage> {
        let mut messages = Vec::new();
        let mut offset = 0;

        while offset < data.len() {
            match self.parse_message(&data[offset..]) {
                Ok((message, consumed)) => {
                    messages.push(message);
                    offset += consumed;
                }
                Err(_) => break,
            }
        }

        messages
    }
}
```

### 7.2. Python-Rust桥接

```python
# Python接口 (pyo3)
import hft_rust_core  # Rust编译的Python扩展

class ProductionLOBSystem:
    """生产级LOB系统"""

    def __init__(self):
        # Rust核心组件
        self.lob_engine = hft_rust_core.LOBEngine()
        self.itch_parser = hft_rust_core.ITCHParser()
        self.feature_evaluator = hft_rust_core.FeatureEvaluator()

        # Python ML组件
        self.alpha_searcher = LOBAlphaSearchEngine()
        self.deep_models = {"deeplob": DeepLOBModel(), "transformer": LOBTransformer()}
        self.rl_agents = {"mm": MarketMakingAgent(), "exec": ExecutionAgent()}

    def real_time_inference(self, itch_message: bytes) -> Dict[str, float]:
        """实时推理管道"""
        # 1. 解析消息 (Rust, <10μs)
        lob_message = self.itch_parser.parse_single(itch_message)

        # 2. 更新订单簿 (Rust, <50μs)
        snapshot = self.lob_engine.process_message(lob_message)

        # 3. 计算Alpha因子 (Rust, <100μs)
        alpha_values = {}
        for alpha_expr in self.active_alphas:
            value = self.feature_evaluator.evaluate(alpha_expr, snapshot)
            alpha_values[alpha_expr] = value

        # 4. 深度学习推理 (Python+GPU, <200μs预算)
        dl_predictions = {}
        for model_name, model in self.deep_models.items():
            with torch.no_grad():
                pred = model(self.prepare_tensor(snapshot))
                dl_predictions[model_name] = pred.item()

        return {**alpha_values, **dl_predictions}

    def deploy_alpha(self, alpha_expression: str) -> bool:
        """部署新的Alpha表达式到生产系统"""
        # 验证表达式
        if not self.validate_expression(alpha_expression):
            return False

        # 编译到Rust评估器
        success = self.feature_evaluator.add_expression(alpha_expression)

        if success:
            self.active_alphas.append(alpha_expression)
            self.log_alpha_deployment(alpha_expression)

        return success
```

### 7.3. 延迟优化

```python
class LatencyOptimizer:
    """延迟优化器"""

    def __init__(self):
        self.latency_budget = 200_000  # 200μs预算（纳秒）
        self.gc_strategy = "deterministic"  # 确定性GC行为

    def optimize_inference_pipeline(self):
        """优化推理管道延迟"""
        optimizations = {
            "message_parsing": "10μs (Rust ITCH解析器)",
            "lob_update": "50μs (无锁环形缓冲区)",
            "feature_evaluation": "100μs (SIMD优化的DSL)",
            "ml_inference": "40μs (TensorRT优化模型)",
            "total_budget": "200μs (包含余量)"
        }
        return optimizations

    def setup_deterministic_gc(self):
        """设置确定性GC行为"""
        # Python侧的GC优化
        import gc
        gc.disable()  # 禁用自动GC

        # 定期手动GC（在非关键路径）
        self.schedule_manual_gc()

    def implement_kill_switches(self):
        """实现紧急停机开关"""
        kill_switches = {
            "latency_breach": "延迟超过阈值时停止交易",
            "pnl_drawdown": "回撤超过限制时平仓",
            "data_anomaly": "数据异常时暂停算法"
        }
        return kill_switches
```

---

## 8. LOB特定特征菜单 (启动包)

### 8.1. 队列与流量特征

```python
class LOBFeatureLibrary:
    """LOB特征库"""

    def calculate_queue_imbalance(self, bid_levels: List[PriceLevel],
                                ask_levels: List[PriceLevel], k: int = 5) -> float:
        """
        队列不平衡 QI@1..K
        QI_k = (Σ_{i<=k} bid_sz[i] - Σ_{i<=k} ask_sz[i]) / (Σ_{i<=k} bid_sz[i] + Σ_{i<=k} ask_sz[i])
        """
        bid_volume = sum(level.size for level in bid_levels[:k])
        ask_volume = sum(level.size for level in ask_levels[:k])

        total_volume = bid_volume + ask_volume
        if total_volume == 0:
            return 0.0

        return (bid_volume - ask_volume) / total_volume

    def calculate_order_flow_imbalance(self, messages: List[LOBMessage],
                                     window: int = 100) -> float:
        """
        订单流不平衡 OFI
        基于窗口内的add/cancel/execute消息
        """
        recent_messages = messages[-window:]

        buy_flow = sum(msg.size for msg in recent_messages
                      if msg.side == 'bid' and msg.message_type in ['add', 'execute'])
        sell_flow = sum(msg.size for msg in recent_messages
                       if msg.side == 'ask' and msg.message_type in ['add', 'execute'])

        total_flow = buy_flow + sell_flow
        if total_flow == 0:
            return 0.0

        return (buy_flow - sell_flow) / total_flow

    def calculate_depth_slope(self, levels: List[PriceLevel], side: str) -> float:
        """
        深度斜率 - 衡量订单簿厚度变化
        """
        if len(levels) < 2:
            return 0.0

        # 计算价格-数量的线性斜率
        prices = [level.price for level in levels]
        volumes = [level.size for level in levels]

        return np.polyfit(prices, volumes, 1)[0]  # 斜率

    def calculate_resilience(self, messages: List[LOBMessage],
                           snapshots: List[LOBSnapshot], window: int = 50) -> float:
        """
        弹性 - 订单簿补充速度
        """
        # 计算执行后的补充速度
        execution_times = []
        refill_times = []

        for i, msg in enumerate(messages[-window:]):
            if msg.message_type == 'execute':
                execution_time = msg.timestamp

                # 寻找补充时间
                for j, snapshot in enumerate(snapshots[i:]):
                    if self.is_refilled(snapshot, msg):
                        refill_time = snapshot.timestamp - execution_time
                        refill_times.append(refill_time)
                        break

        return np.mean(refill_times) if refill_times else 0.0

    def calculate_microprice(self, bid_price: float, ask_price: float,
                           bid_size: float, ask_size: float) -> float:
        """
        Microprice - 向较薄队列倾斜的中间价
        标准HFT估计器
        """
        total_size = bid_size + ask_size
        if total_size == 0:
            return (bid_price + ask_price) / 2

        # 按大小加权，向较薄一侧倾斜
        weight_bid = ask_size / total_size
        weight_ask = bid_size / total_size

        return bid_price * weight_bid + ask_price * weight_ask
```

### 8.2. 价格状态特征

```python
class PriceStateFeatures:
    """价格状态特征"""

    def detect_spread_regime(self, spreads: List[float], window: int = 100) -> str:
        """检测价差机制"""
        recent_spreads = spreads[-window:]
        avg_spread = np.mean(recent_spreads)
        std_spread = np.std(recent_spreads)

        current_spread = spreads[-1]

        if current_spread > avg_spread + 2 * std_spread:
            return "wide"
        elif current_spread < avg_spread - std_spread:
            return "tight"
        else:
            return "normal"

    def microprice_mid_distance(self, microprice: float, mid_price: float) -> float:
        """Microprice与中间价的距离"""
        return abs(microprice - mid_price) / mid_price

    def round_tick_distance(self, price: float, tick_size: float = 0.01) -> float:
        """到整数tick的距离"""
        return abs(price % tick_size - tick_size / 2) / tick_size

class EventRhythmFeatures:
    """事件节奏特征"""

    def calculate_hawkes_intensity(self, event_times: List[int],
                                 current_time: int, decay: float = 0.1) -> float:
        """
        Hawkes过程强度代理（自激发）
        """
        intensity = 0.0
        for event_time in event_times:
            if event_time < current_time:
                time_diff = current_time - event_time
                intensity += np.exp(-decay * time_diff)

        return intensity

    def detect_burst_patterns(self, messages: List[LOBMessage],
                            window: int = 100, threshold: float = 2.0) -> bool:
        """检测突发模式"""
        recent_messages = messages[-window:]

        # 计算消息间隔
        intervals = []
        for i in range(1, len(recent_messages)):
            interval = recent_messages[i].timestamp - recent_messages[i-1].timestamp
            intervals.append(interval)

        if not intervals:
            return False

        # 检测是否存在异常密集的消息
        mean_interval = np.mean(intervals)
        recent_interval = np.mean(intervals[-10:])  # 最近10个间隔

        return recent_interval < mean_interval / threshold
```

---

## 9. 实际可行的MVP (6-8周)

### 9.1. Phase 1: 数据基础设施 (2周)

```python
# Week 1-2: 数据摄取与存储
mvp_phase1 = {
    "数据源": "LOBSTER 1-2个交易所数据",
    "存储": "事件和快照存储系统",
    "时钟": "实现dollar bars",
    "验证": "数据完整性检查",
    "工具": "ClickHouse + Polars + Rust ITCH解析器"
}
```

### 9.2. Phase 2: 标签系统 (1周)

```python
# Week 3: 标签生成
mvp_phase2 = {
    "目标": "Microprice漂移（H事件内）+ 三重屏障",
    "验证": "Purged K-Fold设置",
    "质量": "标签统计和分布检查",
    "工具": "自定义标签生成器"
}
```

### 9.3. Phase 3: AutoAlpha v1 (2周)

```python
# Week 4-5: Alpha发现
mvp_phase3 = {
    "搜索": "SR/GP over LOB-DSL",
    "约束": "复杂度和成本惩罚",
    "存档": "质量多样性(QD)存档",
    "评估": "Out-of-fold IC/IR",
    "工具": "PySR + 自定义LOB操作符"
}
```

### 9.4. Phase 4: 深度学习基准 (1周)

```python
# Week 6: DeepLOB基准
mvp_phase4 = {
    "模型": "DeepLOB重新实现",
    "数据": "LOB张量预处理",
    "输出": "隐藏状态作为'meta-alphas'",
    "评估": "FI-2010风格的基准测试",
    "工具": "PyTorch + 自定义数据加载器"
}
```

### 9.5. Phase 5: RL沙盒 (1-2周)

```python
# Week 7-8: 强化学习
mvp_phase5 = {
    "环境": "ABIDES执行 或 JAX-LOB市场制造",
    "基准": "Avellaneda-Stoikov / Almgren-Chriss",
    "训练": "PPO/SAC基本实现",
    "评估": "模拟器内P&L对比",
    "工具": "Gym + Stable-Baselines3 + ABIDES"
}
```

---

## 10. 成功标准与度量

### 10.1. 预测层指标

```python
prediction_metrics = {
    "IC/IR": "Out-of-fold信息系数和比率",
    "PR-AUC": "精确度-召回曲线下面积",
    "Brier评分": "概率预测校准度",
    "分类精度": "平衡精度/F1（考虑类别不平衡）"
}
```

### 10.2. 交易层指标

```python
trading_metrics = {
    "成本调整P&L": "扣除所有交易成本后的净盈利",
    "夏普/Sortino比率": "风险调整收益",
    "成交率": "订单成交比例",
    "报价生存时间": "限价单平均存活时间",
    "库存VaR": "库存风险价值",
    "尾部损失": "压力窗口期间的最大损失"
}
```

### 10.3. 多样性指标

```python
diversity_metrics = {
    "Alpha相关性": "不同Alpha因子间的相关性矩阵",
    "MAP-Elites覆盖": "复杂度×半衰期×换手率的存档覆盖度",
    "新颖性分数": "与基准特征的最小相关性",
    "稳定性": "Alpha表现的时间稳定性"
}
```

---

## 🎯 立即行动计划

基于你提供的blueprint，我建议：

1. **现在就开始LOB数据获取** - LOBSTER或ITCH数据源
2. **废弃OHLCV方法** - 全面转向事件级数据
3. **优先实现OFI/QI特征** - 这些是最强的短期预测器
4. **建立成本感知模拟器** - 防止假边缘
5. **严格的验证纪律** - Purged K-Fold从第一天开始

这个LOB-native方法将把你的系统提升到真正的工业级HFT水平！

---

**状态**: 🟢 **LOB-Native架构蓝图完成 - 准备实施**
