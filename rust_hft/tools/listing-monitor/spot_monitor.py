#!/usr/bin/env python3
"""Binance USDT 現貨新幣監控"""

import requests
import time
import logging

logging.basicConfig(level=logging.INFO, format='%(asctime)s - %(levelname)s - %(message)s')
log = logging.getLogger(__name__)

EXCHANGE_INFO_API = "https://api.binance.com/api/v3/exchangeInfo"
FEISHU_WEBHOOK = "https://open.feishu.cn/open-apis/bot/v2/hook/2ad78ad0-0325-48e8-b0ed-d813774c810d"
POLL_INTERVAL = 60


def send_feishu(title: str, content: str):
    """發送飛書通知"""
    try:
        requests.post(FEISHU_WEBHOOK, json={
            "msg_type": "interactive",
            "card": {
                "header": {"title": {"tag": "plain_text", "content": title}, "template": "red"},
                "elements": [{"tag": "markdown", "content": content}]
            }
        }, timeout=10)
    except Exception as e:
        log.error(f"飛書通知失敗: {e}")


def fetch_usdt_symbols() -> dict:
    """獲取 USDT 現貨交易對"""
    resp = requests.get(EXCHANGE_INFO_API, headers={"User-Agent": "listing-monitor/1.0"}, timeout=30)
    resp.raise_for_status()
    data = resp.json()
    symbols = [s for s in data.get("symbols", []) if s.get("status") == "TRADING" and s.get("quoteAsset") == "USDT"]
    return {s["symbol"]: s for s in symbols}


def main():
    log.info("啟動 Binance USDT 現貨監控")
    known = {}
    first_run = True

    while True:
        try:
            current = fetch_usdt_symbols()
            new_symbols = [s for sym, s in current.items() if sym not in known]
            delisted = [sym for sym in known if sym not in current]

            if new_symbols and not first_run:
                log.info(f"發現 {len(new_symbols)} 個新交易對")
                content = "\n".join([
                    f"**{s['symbol']}**\nBase: {s.get('baseAsset')} | Quote: USDT"
                    for s in new_symbols
                ])
                send_feishu(f"Binance 現貨新幣 ({len(new_symbols)})", content)

            if delisted and not first_run:
                log.warning(f"下架: {delisted}")
                send_feishu(f"Binance 現貨下架 ({len(delisted)})", "\n".join([f"**{s}** - 已下架" for s in delisted]))

            if first_run:
                log.info(f"初始載入: {len(current)} 個 USDT 交易對")

            known = current
            first_run = False

        except Exception as e:
            log.error(f"獲取失敗: {e}")

        time.sleep(POLL_INTERVAL)


if __name__ == "__main__":
    main()
