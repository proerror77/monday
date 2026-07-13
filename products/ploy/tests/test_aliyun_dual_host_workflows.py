import os
import pathlib
import subprocess
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]


class AliyunDualHostWorkflowContracts(unittest.TestCase):
    def test_trade_service_has_exact_guardrails_and_immutable_entrypoint(self):
        service = (ROOT / "deployment" / "ployd-trade.service").read_text()
        for required in (
            "ExecStart=/opt/ploy/current/bin/ployd",
            "Restart=always",
            "RestartSec=5",
            "MemoryHigh=1280M",
            "MemoryMax=1536M",
            "OOMPolicy=kill",
        ):
            self.assertIn(required, service)

    def test_postflight_checks_runtime_guardrails_and_paused_live_state(self):
        script = (ROOT / "scripts" / "verify_trade_host_postflight.sh").read_text()
        for required in (
            "RestartUSec 5s",
            "MemoryHigh 1342177280",
            "MemoryMax 1610612736",
            "OOMPolicy kill",
            "Rust build process is running on the trade host",
            "desired=Paused",
            "observed=Paused",
            "venue:venue:polymarket:healthy",
            "release.json",
        ):
            self.assertIn(required, script)

    def test_tango_bundle_has_no_live_authority(self):
        workflow = (ROOT / ".github" / "workflows" / "deploy-tango-1-1.yml").read_text()
        self.assertNotIn("02-pm5d.live.toml dist/", workflow)
        self.assertNotIn("02-pm5d-threelayer.live.toml dist/", workflow)
        self.assertNotIn(
            "cp scripts/drills/pm5d_threelayer_live_gate.sh dist/", workflow
        )
        self.assertIn("! -name 'pm5d_threelayer_live_gate.sh'", workflow)
        self.assertIn("research host bundle contains a live deployment", workflow)
        self.assertIn('-p ployctl', workflow)
        self.assertIn('dist/bin/ployctl', workflow)

    def test_tango_ssh_deploy_uses_shared_generated_remote_script(self):
        workflow = (ROOT / ".github" / "workflows" / "deploy-tango-1-1.yml").read_text()
        self.assertIn(
            "deploy_tango_cloud_assist.py --print-remote-script", workflow
        )
        self.assertNotIn("ssh tango-1-1 /bin/bash <<EOF", workflow)

        run_block_sizes = []
        lines = workflow.splitlines()
        for index, line in enumerate(lines):
            if line.strip() != "run: |":
                continue
            indent = len(line) - len(line.lstrip())
            block = []
            for candidate in lines[index + 1 :]:
                candidate_indent = len(candidate) - len(candidate.lstrip())
                if candidate.strip() and candidate_indent <= indent:
                    break
                block.append(candidate)
            run_block_sizes.append(len("\n".join(block).encode()))
        self.assertLess(max(run_block_sizes), 21_000)

        env = os.environ.copy()
        env.update(
            {
                "DEPLOY_ROOT": "/opt/ploy",
                "DEPLOY_BUNDLE_NAME": "ploy-test.tar.gz",
                "GITHUB_SHA": "a" * 40,
                "DEPLOY_OSS_REGION": "ap-northeast-1",
                "ALIYUN_OSS_BUCKET": "test-bucket",
                "DEPLOY_OSS_PREFIX": "deploy/tango-1-1",
                "ALIYUN_OSS_ACCESS_KEY_ID": "test-id",
                "ALIYUN_OSS_ACCESS_KEY_SECRET": "test-secret",
                "DEPLOY_MIGRATIONS": "",
            }
        )
        generated = subprocess.run(
            [
                "python3",
                str(ROOT / "scripts" / "ci" / "deploy_tango_cloud_assist.py"),
                "--print-remote-script",
            ],
            env=env,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(generated.returncode, 0, generated.stderr)
        syntax = subprocess.run(
            ["bash", "-n"],
            input=generated.stdout,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(syntax.returncode, 0, syntax.stderr)

    def test_trade_bundle_contains_full_control_plane_and_paused_gate(self):
        workflow = (ROOT / ".github" / "workflows" / "deploy-trade.yml").read_text()
        for required in (
            "-p new-ployd",
            "-p ployctl",
            "-p new-ploy-runner",
            "pm5d.threelayer.live.json",
            "pm5d_threelayer_live_gate.sh",
            "verify_trade_host_postflight.sh",
            "releases/${GITHUB_SHA}",
            "desired_state"):
            self.assertIn(required, workflow)
        self.assertIn('default: false', workflow)

    def test_live_resume_is_separate_evidence_bound_human_approval(self):
        workflow = (ROOT / ".github" / "workflows" / "approve-live-trade.yml").read_text()
        for required in (
            "environment: ploy-trade-live",
            "runtime_replay_run_id",
            "parity_run_id",
            "validate_live_promotion_evidence.py",
            "I APPROVE LIVE RISK AT 5 USD MAX EXPOSURE",
            "actions/runs/${run_id}",
            "head_sha",
            "deployments resume pm5d.threelayer.live",
            "deployments pause pm5d.threelayer.live",
            "verify_trade_host_postflight.sh",
            "--expected-config-sha256",
            "approval-provenance.json",
            "pending.json",
        ):
            self.assertIn(required, workflow)

        tango = (ROOT / ".github" / "workflows" / "deploy-tango-1-1.yml").read_text()
        trade = (ROOT / ".github" / "workflows" / "deploy-trade.yml").read_text()
        gate = (ROOT / "scripts" / "drills" / "pm5d_threelayer_live_gate.sh").read_text()
        self.assertNotIn("deployments resume pm5d.threelayer.live", tango)
        self.assertNotIn("deployments resume pm5d.threelayer.live", trade)
        self.assertNotIn("deployments resume", gate)

        daemon = (ROOT / "crates" / "ploy-daemon-host" / "src" / "runtime.rs").read_text()
        self.assertIn("ensure_live_resume_approved", daemon)
        self.assertIn("live_config_sha256", daemon)
        self.assertIn("expires_at > Utc::now()", daemon)
        self.assertIn("chrono::Duration::minutes(20)", daemon)
        self.assertIn("io::ErrorKind::PermissionDenied", daemon)
        self.assertIn("live resume requires PLOY_LIVE_APPROVAL_FILE", daemon)

        self.assertIn('import re', gate)

        cloud_assist = (
            ROOT / "scripts" / "ci" / "deploy_tango_cloud_assist.py"
        ).read_text()
        self.assertIn("--print-remote-script", tango)
        self.assertIn('record["desired_state"] = "paused"', cloud_assist)
        self.assertIn('record["observed_state"] = "paused"', cloud_assist)
        self.assertIn("-iname '*live*.toml'", cloud_assist)

        self.assertIn("group: ploy-trade-host", workflow)
        self.assertIn("group: ploy-trade-host", trade)


if __name__ == "__main__":
    unittest.main()
