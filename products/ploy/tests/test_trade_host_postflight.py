import json
import hashlib
import os
import pathlib
import stat
import subprocess
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "verify_trade_host_postflight.sh"
SHA = "a" * 40


def executable(path: pathlib.Path, content: str) -> None:
    path.write_text(content, encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


class TradeHostPostflightTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.temp.name)
        release = self.root / "releases" / SHA
        (release / "bin").mkdir(parents=True)
        for name in ("ployd", "ployctl", "ploy-runner"):
            executable(release / "bin" / name, "#!/bin/sh\nexit 0\n")
        (self.root / "bin").mkdir(parents=True)
        manifest_files = [release / "bin" / name for name in ("ployd", "ployctl", "ploy-runner")]
        (release / "FILES.sha256").write_text(
            "".join(
                f"{hashlib.sha256(item.read_bytes()).hexdigest()}  ./{item.relative_to(release)}\n"
                for item in manifest_files
            ),
            encoding="utf-8",
        )
        (release / "release.json").write_text(
            json.dumps({"git_sha": SHA, "bundle_sha256": "f" * 64}),
            encoding="utf-8",
        )
        (self.root / "current").symlink_to(pathlib.Path("releases") / SHA)
        (self.root / ".env").write_text(
            f"PLOY_RELEASE_SHA={SHA}\n"
            f"PLOY_LIVE_APPROVAL_FILE={self.root}/data/live-approvals/pending.json\n",
            encoding="utf-8",
        )

        self.bin = self.root / "fake-bin"
        self.bin.mkdir()
        executable(
            self.bin / "systemctl",
            """#!/bin/sh
if [ "$1" = is-active ]; then exit 0; fi
if [ "$1" = show ]; then
  case "$3" in
    Restart) printf '%s\n' "${FAKE_RESTART:-always}" ;;
    RestartUSec) printf '%s\n' "${FAKE_RESTART_USEC:-5s}" ;;
    MemoryHigh) printf '%s\n' "${FAKE_MEMORY_HIGH:-1342177280}" ;;
    MemoryMax) printf '%s\n' "${FAKE_MEMORY_MAX:-1610612736}" ;;
    OOMPolicy) printf '%s\n' "${FAKE_OOM_POLICY:-kill}" ;;
  esac
  exit 0
fi
exit 1
""",
        )
        executable(self.bin / "curl", "#!/bin/sh\nexit 0\n")
        executable(self.bin / "pgrep", "#!/bin/sh\nexit ${FAKE_PGREP_EXIT:-1}\n")
        executable(
            self.bin / "ployctl",
            """#!/bin/sh
if [ "$1 $2" = "deployments inspect" ]; then
  echo 'pm5d.threelayer.live mode=Live lifecycle=Enabled desired=Paused observed=Paused'
elif [ "$1 $2" = "deployments list" ]; then
  echo 'pm5d.threelayer.live mode=Live lifecycle=Enabled desired=Paused observed=Paused'
else
  echo ok
fi
""",
        )

    def tearDown(self):
        self.temp.cleanup()

    def run_postflight(self, **overrides):
        env = os.environ.copy()
        env.update(
            {
                "PLOY_ROOT_DIR": str(self.root),
                "SYSTEMCTL": str(self.bin / "systemctl"),
                "PLOYCTL": str(self.bin / "ployctl"),
                "CURL": str(self.bin / "curl"),
                "PGREP": str(self.bin / "pgrep"),
                **overrides,
            }
        )
        return subprocess.run(
            [str(SCRIPT), SHA, "paused"],
            env=env,
            text=True,
            capture_output=True,
            check=False,
        )

    def test_valid_trade_host_passes(self):
        result = self.run_postflight()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_each_guardrail_and_host_build_check_fails_closed(self):
        cases = {
            "FAKE_RESTART": "on-failure",
            "FAKE_RESTART_USEC": "1s",
            "FAKE_MEMORY_HIGH": "1",
            "FAKE_MEMORY_MAX": "2",
            "FAKE_OOM_POLICY": "continue",
            "FAKE_PGREP_EXIT": "0",
        }
        for key, value in cases.items():
            with self.subTest(key=key):
                result = self.run_postflight(**{key: value})
                self.assertNotEqual(result.returncode, 0)

if __name__ == "__main__":
    unittest.main()
