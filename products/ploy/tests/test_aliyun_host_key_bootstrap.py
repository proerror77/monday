import pathlib
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]


class AliyunHostKeyBootstrapContracts(unittest.TestCase):
    def test_host_key_attestation_uses_cloud_assistant_as_trust_root(self):
        workflow = (
            ROOT / ".github" / "workflows" / "bootstrap-aliyun-host-keys.yml"
        ).read_text()

        for required in (
            "Host-key attestation must be dispatched from main",
            "ALIYUN_ECS_ACCESS_KEY_ID",
            'ALIYUN_CLI_VERSION: "3.3.18"',
            "0823286604dbd8beb8d65dd0694d23c913e7c5d5a02b20a3593a4f8a6517f1d4",
            "sha256sum -c -",
            "DescribeInstances",
            "RunCommand",
            "/etc/ssh/ssh_host_ed25519_key.pub",
            "DescribeInvocationResults",
            "base64 --decode",
            "ssh-keyscan -T 15",
            "comm -23 network-known-hosts.txt attested-known-hosts.txt",
            "actions/upload-artifact@v7",
        ):
            self.assertIn(required, workflow)

        self.assertNotIn("StrictHostKeyChecking=no", workflow)
        self.assertNotIn("UserKnownHostsFile=/dev/null", workflow)
        self.assertNotIn("aliyun-cli-linux-latest", workflow)


if __name__ == "__main__":
    unittest.main()
