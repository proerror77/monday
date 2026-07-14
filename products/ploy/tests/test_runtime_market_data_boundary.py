from pathlib import Path
import tomllib
import unittest


ROOT = Path(__file__).resolve().parents[1]
STRATEGY_DIR = ROOT / "config" / "strategies"


class RuntimeMarketDataBoundaryTests(unittest.TestCase):
    def test_predict_fun_collector_is_deployed_but_requires_host_api_key(self) -> None:
        runner = (ROOT / "crates/ploy-runner-host/src/lib.rs").read_text()
        service = (ROOT / "deployment/systemd/ploy-predict-fun-collector.service").read_text()
        workflow = (ROOT / ".github/workflows/deploy-tango-1-1.yml").read_text()
        deploy = (ROOT / "scripts/ci/deploy_tango_cloud_assist.py").read_text()

        self.assertIn("collect-predict-fun", runner)
        self.assertIn("ExecStart=/opt/ploy/bin/ploy-runner collect-predict-fun", service)
        self.assertIn("EnvironmentFile=/etc/ploy/predict-fun.env", service)
        self.assertIn("Restart=always", service)
        self.assertIn("RestartSec=5", service)
        self.assertIn("MemoryHigh=1280M", service)
        self.assertIn("MemoryMax=1536M", service)
        self.assertIn("OOMPolicy=kill", service)
        self.assertIn("050_predict_fun_market_data.sql", workflow)
        self.assertIn("ploy-predict-fun-collector.service", workflow)
        self.assertIn("PREDICT_FUN_API_KEY", deploy)
        self.assertIn("predict_fun_configured", deploy)
        self.assertIn("api-testnet\\\\.predict\\\\.fun", deploy)
        self.assertIn("systemctl disable --now ploy-predict-fun-collector.service", deploy)
        self.assertIn("Predict.fun collection pass complete", deploy)
        self.assertIn("predict_fun_orderbook_ticks WHERE received_at", deploy)

    def test_public_cex_collector_is_deployed_and_health_gated(self) -> None:
        collector = (ROOT / "crates/ploy-market-data/src/cex_collectors.rs").read_text()
        runner = (ROOT / "crates/ploy-runner-host/src/lib.rs").read_text()
        service = (ROOT / "deployment/systemd/ploy-cex-public-collector.service").read_text()
        workflow = (ROOT / ".github/workflows/deploy-tango-1-1.yml").read_text()
        deploy = (ROOT / "scripts/ci/deploy_tango_cloud_assist.py").read_text()
        health = (ROOT / ".github/workflows/healthcheck-tango-1-1.yml").read_text()
        audit = (ROOT / "scripts/audit_market_data_gaps.py").read_text()

        for endpoint in (
            "https://fapi.binance.com",
            "wss://fstream.binance.com/stream",
            "wss://ws.okx.com:8443/ws/v5/public",
            "wss://stream.bybit.com/v5/public/spot",
            "wss://advanced-trade-ws.coinbase.com",
            "wss://ws.kraken.com/v2",
        ):
            self.assertIn(endpoint, collector)
        self.assertIn("collect-cex-public", runner)
        self.assertIn("ExecStart=/opt/ploy/bin/ploy-runner collect-cex-public", service)
        self.assertIn("Restart=always", service)
        self.assertIn("OOMPolicy=kill", service)
        self.assertIn("049_cex_public_market_data.sql", workflow)
        self.assertIn("ploy-cex-public-collector.service", workflow)
        for text in (deploy, health):
            self.assertIn("ploy-cex-public-collector.service", text)
            self.assertIn("cex_public_market_ticks", text)
            for exchange in ("okx", "bybit", "coinbase", "kraken"):
                self.assertIn(exchange, text)
        self.assertIn('"cex-extended"', audit)
        self.assertIn("kind = 'liquidation'", health)
        self.assertIn(
            'source_id: "binance_liquidations"',
            (ROOT / "crates/ploy-daemon-host/src/reports.rs").read_text(),
        )
        self.assertIn('?streams={}', collector)

    def test_pm5d_runtime_configs_default_to_local_market_data(self) -> None:
        configs = sorted(STRATEGY_DIR.glob("02-pm5d*.toml"))
        self.assertGreaterEqual(len(configs), 1)

        for path in configs:
            config = tomllib.loads(path.read_text())
            mode = config.get("runtime", {}).get("mode", "")
            if mode not in {"dryrun", "live"}:
                continue
            source = config.get("runtime", {}).get("market_data_source", "local_db")
            self.assertEqual(
                source,
                "local_db",
                f"{path.name} must not opt strategy runners back into direct public feeds",
            )

    def test_live_runtime_direct_feeds_are_explicitly_gated(self) -> None:
        live_rs = ROOT / "crates" / "ploy-strategy-runtime" / "src" / "live.rs"
        text = live_rs.read_text()

        self.assertIn("market_data_source.uses_external_direct()", text)
        self.assertIn("market_data_source.uses_local_db()", text)
        self.assertIn("spawn_db_polymarket_feed", text)

        external_gate = text.index("market_data_source.uses_external_direct()")
        for direct_feed in (
            "spawn_spot_feed(",
            "spawn_chainlink_feed(",
            "spawn_pyth_reference_feed(",
            "spawn_market_scanner(",
            "spawn_sports_feed(",
        ):
            self.assertGreater(
                text.index(direct_feed),
                external_gate,
                f"{direct_feed} must stay behind explicit external-direct opt-in",
            )

    def test_central_market_discovery_service_owns_pm_catalog_refresh(self) -> None:
        runner_lib = ROOT / "crates" / "ploy-runner-host" / "src" / "lib.rs"
        runner_ops = ROOT / "crates" / "ploy-runner-host" / "src" / "ops.rs"
        scanner = ROOT / "crates" / "ploy-market-data" / "src" / "scanner.rs"
        service = ROOT / "deployment" / "systemd" / "ploy-market-discovery.service"
        workflow = ROOT / ".github" / "workflows" / "deploy-tango-1-1.yml"

        self.assertIn("collect-markets", runner_lib.read_text())
        self.assertIn("run_market_discovery_collector", runner_ops.read_text())

        scanner_text = scanner.read_text()
        self.assertIn("MarketDiscoveryCollectorConfig", scanner_text)
        self.assertIn("refresh_crypto_catalog", scanner_text)
        self.assertIn("persist_discovered_crypto_market", scanner_text)

        service_text = service.read_text()
        self.assertIn("ExecStart=/opt/ploy/bin/ploy-runner collect-markets", service_text)
        self.assertIn("Restart=always", service_text)
        self.assertIn("MemoryMax=768M", service_text)

        workflow_text = workflow.read_text()
        cloud_assist_text = (ROOT / "scripts" / "ci" / "deploy_tango_cloud_assist.py").read_text()
        self.assertIn("ploy-market-discovery.service", workflow_text)
        self.assertIn("deploy_tango_cloud_assist.py --print-remote-script", workflow_text)
        self.assertLess(
            cloud_assist_text.index("systemctl restart ploy-market-discovery.service"),
            cloud_assist_text.index("systemctl restart ploy-quote-collector.service"),
            "market discovery must refresh catalog before quote collector subscribes",
        )
        self.assertLess(
            cloud_assist_text.index("systemctl restart ploy-market-discovery.service"),
            cloud_assist_text.index("systemctl restart ploy-pm-trade-collector.service"),
            "market discovery must refresh catalog before trade collector polls",
        )
        for text, name in ((cloud_assist_text, "deploy_tango_cloud_assist.py"),):
            discovery_restart = text.index("systemctl restart ploy-market-discovery.service")
            catalog_wait = text.index(
                "pm_market_catalog has no active crypto markets after market-discovery restart"
            )
            metadata_wait = text.index(
                "pm_market_metadata has no active crypto markets after market-discovery restart"
            )
            trade_restart = text.index("systemctl restart ploy-pm-trade-collector.service")
            self.assertLess(
                discovery_restart,
                catalog_wait,
                f"{name} must wait for catalog after market discovery restart",
            )
            self.assertLess(
                catalog_wait,
                trade_restart,
                f"{name} must wait for catalog before trade collector restart",
            )
            self.assertLess(
                discovery_restart,
                metadata_wait,
                f"{name} must wait for metadata after market discovery restart",
            )
            self.assertLess(
                metadata_wait,
                trade_restart,
                f"{name} must wait for metadata before trade collector restart",
            )

    def test_pm_trade_deploy_health_uses_collector_poll_not_fresh_insert(self) -> None:
        workflow = ROOT / ".github" / "workflows" / "deploy-tango-1-1.yml"
        cloud_assist = ROOT / "scripts" / "ci" / "deploy_tango_cloud_assist.py"
        self.assertIn(
            "deploy_tango_cloud_assist.py --print-remote-script", workflow.read_text()
        )

        for path in (cloud_assist,):
            text = path.read_text()
            self.assertIn("wait_for_recent_log", text)
            self.assertIn("Polymarket trade collector poll complete", text)
            self.assertIn("pm trade collector did not complete a healthy poll after deploy", text)
            self.assertIn("pm trade collector failed after deploy", text)
            self.assertIn("120", text)
            self.assertIn("5", text)
            self.assertIn('local since="${{DEPLOY_STARTED_AT}}"', text)
            self.assertIn(
                '"pm trade collector did not complete a healthy poll after deploy" \\\\\n'
                "  120 \\\\\n"
                "  5",
                text,
            )
            self.assertNotIn(
                "clob_trade_ticks WHERE received_at >= NOW() - INTERVAL '5 minutes'",
                text,
                f"{path.name} must not require a fresh PM trade insert as collector health",
            )
            self.assertNotIn(
                "clob_trade_ticks is not receiving PM trade prints after deploy",
                text,
                f"{path.name} must not fail deploys when a healthy poll inserts no trades",
            )


if __name__ == "__main__":
    unittest.main()
