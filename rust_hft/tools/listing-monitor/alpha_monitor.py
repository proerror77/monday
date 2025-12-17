#!/usr/bin/env python3
"""Binance Alpha 新幣監控"""

import requests
import time
import logging

logging.basicConfig(level=logging.INFO, format='%(asctime)s - %(levelname)s - %(message)s')
log = logging.getLogger(__name__)

ALPHA_API = "https://www.binance.com/bapi/defi/v1/public/wallet-direct/buw/wallet/cex/alpha/all/token/list"
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


def fetch_alpha_tokens() -> dict:
    """獲取 Alpha 代幣列表"""
    resp = requests.get(ALPHA_API, headers={"User-Agent": "listing-monitor/1.0"}, timeout=30)
    resp.raise_for_status()
    data = resp.json()
    tokens = data.get("data", [])
    return {t.get("symbol", ""): t for t in tokens if t.get("symbol")}


def main():
    log.info("啟動 Binance Alpha 監控")
    known = {}
    first_run = True

    while True:
        try:
            current = fetch_alpha_tokens()
            new_tokens = [t for sym, t in current.items() if sym not in known]

            if new_tokens and not first_run:
                log.info(f"發現 {len(new_tokens)} 個新 Alpha 代幣")
                content = "\n".join([
                    f"**{t.get('symbol')}** - {t.get('name', 'N/A')}\nChain: {t.get('chainId', 'N/A')}"
                    for t in new_tokens
                ])
                send_feishu(f"Binance Alpha 新幣 ({len(new_tokens)})", content)

            if first_run:
                log.info(f"初始載入: {len(current)} 個 Alpha 代幣")

            known = current
            first_run = False

        except Exception as e:
            log.error(f"獲取失敗: {e}")

        time.sleep(POLL_INTERVAL)


if __name__ == "__main__":
    main()
