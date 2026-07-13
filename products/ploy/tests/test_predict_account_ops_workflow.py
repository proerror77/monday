import pathlib
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]


class PredictAccountOpsWorkflowContracts(unittest.TestCase):
    def test_protected_workflow_binds_one_operation_to_exact_release_and_plan(self):
        workflow = (
            ROOT / ".github" / "workflows" / "approve-predict-account-op.yml"
        ).read_text()
        for required in (
            "environment: ploy-trade-live",
            '[[ "${GITHUB_REF_NAME}" == "main" ]]',
            '[[ "$(git rev-parse origin/main)" == "${DEPLOY_SHA}" ]]',
            '[[ "$(readlink -f "${root}/current")" == "${root}/releases/${deploy_sha}" ]]',
            "I APPROVE THIS PREDICT ACCOUNT OPERATION",
            "root:root:600",
            '${root}/data/account-ops/predict-order-plan.json',
            '${root}/data/account-ops/predict-redeem-plan.json',
            'PLOY_PREDICT_APPROVAL_WRITE_ENABLED=true "${root}/bin/ploy-predict-account-ops"',
            'PLOY_PREDICT_ACCOUNT_OPS_WRITE_ENABLED=true "${root}/bin/ploy-predict-account-ops"',
            'PLOY_PREDICT_RECONCILE_WRITE_ENABLED=true "${root}/bin/ploy-predict-account-ops"',
            "Revalidate current main after human approval",
            'git merge-base --is-ancestor "${DEPLOY_SHA}" origin/main',
            "fetch-depth: 0",
            "require_unique_false PLOY_PREDICT_RECONCILE_WRITE_ENABLED",
            "StrictHostKeyChecking yes",
        ):
            self.assertIn(required, workflow)
        self.assertGreaterEqual(workflow.count("fetch-depth: 0"), 2)
        self.assertNotIn("git fetch --no-tags --depth=1 origin main", workflow)
        self.assertNotIn('upsert_env PLOY_PREDICT_ACCOUNT_OPS_WRITE_ENABLED "true"', workflow)
        self.assertNotIn('upsert_env PLOY_PREDICT_APPROVAL_WRITE_ENABLED "true"', workflow)
        self.assertNotIn('upsert_env PLOY_PREDICT_RECONCILE_WRITE_ENABLED "true"', workflow)

    def test_trade_deploy_packages_adapter_but_resets_persistent_write_gates(self):
        workflow = (ROOT / ".github" / "workflows" / "deploy-trade.yml").read_text()
        for required in (
            "tools/predict-fun-account-ops/package-lock.json",
            "npm audit --omit=dev --audit-level=high",
            'upsert_env PLOY_PREDICT_ACCOUNT_OPS_WRITE_ENABLED "false"',
            'upsert_env PLOY_PREDICT_APPROVAL_WRITE_ENABLED "false"',
            'upsert_env PLOY_PREDICT_RECONCILE_WRITE_ENABLED "false"',
            'ploy-predict-account-ops',
            "Predict account operation is locked; reconcile it before deploying a new release",
        ):
            self.assertIn(required, workflow)
        self.assertGreaterEqual(workflow.count("npm test"), 2)
        self.assertGreaterEqual(workflow.count("npm audit --omit=dev --audit-level=high"), 2)


if __name__ == "__main__":
    unittest.main()
