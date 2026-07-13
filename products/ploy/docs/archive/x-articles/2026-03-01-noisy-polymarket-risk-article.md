# 内文

```text
@noisyb0y1Most traders lose from greed, not bad trades - I tested 3 strategies over 300 trades to prove it568529KI analyzed 300+ traders on Polymarket with win rate above 65%. Most of them are sitting in drawdowns from $50,000 to $300,000. Win rate 65%+. Minus hundreds of thousands. How is that even possible?The answer is in one number - and you're ignoring it on every single trade.| Before we start, bookmark this and drop a follow
| Posting daily alpha on Polymarket and moreThis is a breakdown of why smart traders with right predictions are systematically blowing their deposits - and the math that explains it.Every section builds on the previous one. Skip one - the rest won't make sense. Read in order, and by the end, you'll have working code for every layer of the system.Phase 1: You're right 7 out of 10 times. Still in the red. Here's the number that explains it.February 2025, a market drops on Polymarket"Will BTC close above $100,000 by end of month?"

YES is trading at $0.63 - market gives 63% chance. But you're confident the real probability is closer to 74%. Edge is there, the difference is real, you buy in for $500.BTC closes above $100k. Contract goes to $1.00 and you make $185 on the trade.Now imagine you trade like this for a whole month - ten similar markets where you see edge, and seven out of ten times you turn out to be right.

70% win rate - seems like a great result that should give stable profit.But if you put the same $500 on every trade regardless of signal quality, the math is working against you.On three losing trades, you lose $1,500. On seven winning trades you make $1,295. End of month - minus $205 at 70% win rate.One reason - position size was the same, even though the trades were very different in edge quality. That's exactly what's systematically killing traders with right predictions.Phase 2: What your prediction is actually worthBefore getting into formulas, gotta understand the basic math.Every Polymarket contract is a coin with a known bias. If YES is trading at $0.61 - market says: 61% probability.  If you think the real probability is 72% you have edge = 11%.But edge by itself means nothing. The question is: how much to put on that edge?Most traders bet on feeling. Or "how much i can afford to lose". Or "how confident i feel". That's not a strategy.Expected Value positive EV - right to enter the trade. but EV doesn't tell you how much to bet. that's what Kelly is for.pythonp_win = 0.74           # your probability estimate
p_loss = 1 - p_win     # = 0.26
profit = (1.00 - 0.63) * 100   # $37 if you win
loss = 0.63 * 100               # $63 if you lose

EV = (p_win * profit) - (p_loss * loss)
EV = (0.74 * 37) - (0.26 * 63)
EV = 27.38 - 16.38
EV = +$11.00Phase 3: Kelly Criterion - the formula everyone knows and nobody uses rightMost traders have heard of Kelly Criterion. Few actually understand what it does.Kelly doesn't maximize profit from one trade. It maximizes long-term deposit growth across hundreds of trades. That's a fundamental difference.Kelly Criterion
f* - fraction of deposit to bet
p - your win probability estimate
q - loss probability (1 - p)
b - win-to-stake ratiopythondef kelly_criterion(p_win, contract_price):
    q = 1 - p_win
    b = (1.0 - contract_price) / contract_price
    f_star = (p_win * b - q) / b
    return max(0, f_star)

p_win = 0.74
price = 0.63

f = kelly_criterion(p_win, price)
print(f"Kelly size: {f:.1%} of deposit")
print(f"Stake from $1000: ${1000 * f:.0f}")Result:Kelly size: 21.3% of deposit
Stake from $1000: $213 not $500. math says $213.Phase 4: Why full Kelly = bankruptcyThis is where most people stop. They see 21.3% - they bet 21.3%.That's a mistake.Full Kelly assumes your estimate is perfectly accurate. That your 74% is exactly 74%, not somewhere between 67% and 81%.But your estimate is never accurate. You're aggregating data, reading news, watching the market. All of that gives you a range, not a precise point.The less confident you are in your estimate — the less you should bet. That's what Empirical Kelly is for.Empirical Kelly
CV_edge - coefficient of variation of your edge
σ_edge - how spread out your estimates are
μ_edge - average edgepythonimport numpy as np

def empirical_kelly(p_win_estimates, contract_price):
    estimates = np.array(p_win_estimates)
    edges = estimates - contract_price
    mu_edge = np.mean(edges)
    sigma_edge = np.std(edges)
    cv_edge = sigma_edge / mu_edge if mu_edge > 0 else 1.0
    p_mean = np.mean(estimates)
    f_kelly = kelly_criterion(p_mean, contract_price)
    f_empirical = f_kelly * (1 - cv_edge)
    return max(0, f_empirical), {
        'f_kelly': f_kelly,
        'uncertainty': sigma_edge
    }

estimates = [0.72, 0.74, 0.70, 0.77, 0.73]
price = 0.63

f_emp, details = empirical_kelly(estimates, price)
print(f"full Kelly:      {details['f_kelly']:.1%}")
print(f"uncertainty:     ±{details['uncertainty']:.1%}")
print(f"empirical Kelly: {f_emp:.1%}")
print(f"stake from $1000: ${1000 * f_emp:.0f}")Result:full Kelly: 21.3%
uncertainty: ±2.4%
empirical Kelly: 15.8%
stake from $1000: $158Phase 5: Max Drawdown - the line you can't crossEven with the right Kelly a bad streak can destroy your deposit.W_t - deposit peak up to moment t
P_t - current deposit
MDD - maximum drawdown over the entire periodpythonimport numpy as np

def empirical_kelly(p_win_estimates, contract_price):
    estimates = np.array(p_win_estimates)
    edges = estimates - contract_price
    mu_edge = np.mean(edges)
    sigma_edge = np.std(edges)
    cv_edge = sigma_edge / mu_edge if mu_edge > 0 else 1.0
    p_mean = np.mean(estimates)
    f_kelly = kelly_criterion(p_mean, contract_price)
    f_empirical = f_kelly * (1 - cv_edge)
    return max(0, f_empirical), {
        'f_kelly': f_kelly,
        'uncertainty': sigma_edge
    }

estimates = [0.72, 0.74, 0.70, 0.77, 0.73]
price = 0.63

f_emp, details = empirical_kelly(estimates, price)
print(f"full Kelly:      {details['f_kelly']:.1%}")
print(f"uncertainty:     ±{details['uncertainty']:.1%}")
print(f"empirical Kelly: {f_emp:.1%}")
print(f"stake from $1000: ${1000 * f_emp:.0f}")Typical result:Full Kelly - 18% chance that at some point you'll lose half your deposit. Quarter Kelly - 0.2%.Most quant traders use Half or Quarter Kelly in real trading. Not because they're scared. Because math.Phase 6: Fees - the hidden loss eating your edgeThere are two enemies on Polymarket that most people ignore: fees and spread. Not all markets have this, but the popular ones like 5/15-minute BTC, ETH, SOL markets - do. Together they can turn positive edge into negative.Fee Formula Fee is maximum at p=0.50 and minimum when p is close to 0 or 1. Meaning the most active contracts - are the most expensive to trade.pythondef simulate_kelly_drawdown(p_win, price, deposit=1000,
                             n_trades=100, n_simulations=10_000):
    results = {}
    for kelly_fraction in [1.0, 0.5, 0.25]:
        f = kelly_criterion(p_win, price) * kelly_fraction
        max_drawdowns = []
        for _ in range(n_simulations):
            portfolio = deposit
            peak = deposit
            max_dd = 0
            for _ in range(n_trades):
                stake = portfolio * f
                if np.random.random() < p_win:
                    portfolio += stake * (1 - price) / price
                else:
                    portfolio -= stake
                if portfolio > peak:
                    peak = portfolio
                dd = (peak - portfolio) / peak
                max_dd = max(max_dd, dd)
            max_drawdowns.append(max_dd)
        results[kelly_fraction] = {
            'median_mdd': np.median(max_drawdowns),
            'p95_mdd': np.percentile(max_drawdowns, 95),
            'ruin_rate': np.mean(np.array(max_drawdowns) > 0.5)
        }
    return resultsAll the formulas we covered above work together as one chain.You get a signal -> collect estimates from different sources -> calculate Empirical Kelly -> account for fees -> get a specific number.One table that explains everythingAll three traders were trading the same BTC contract with the same win rate. The only difference was position size - and it determined who's in the red, who's surviving, and who's growing steadily.
```

# 分析

1. 核心观点是：交易长期盈亏不由胜率单独决定，关键在仓位大小（position sizing）和风险控制。
2. 文章结构是正确方向：先用 EV 判断是否有正期望，再用 Kelly 决定下注比例，再加入不确定性、回撤和费用。
3. 文中有明显数值不一致：在价格 0.63、投入 $500 的前提下，盈利写成 $185 与标准结算不一致。按常见二元合约计法，盈利约为 $293.65。
4. Kelly 示例也可能不一致：若 p=0.74、price=0.63，按文中公式直接算出来接近 29.8%，不是文中给的 21.3%，推测作者混用了参数。
5. Phase 5/6 的代码段存在拼接错位：回撤模拟与手续费讨论混在一起，手续费模型没有完整落地公式。
6. 可执行做法：
   - 先做概率校准（避免高估 p）；
   - 用 half/quarter Kelly 做保守仓位；
   - 把手续费和点差直接扣进 EV；
   - 设定组合级别最大回撤阈值并硬性降仓。
