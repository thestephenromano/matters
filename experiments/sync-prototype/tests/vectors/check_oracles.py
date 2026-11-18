"""Exercise the real fixture generators with and without Python optimization."""
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent


class OracleGuards(unittest.TestCase):
    def test_regeneration_and_drift_in_both_modes(self):
        for name in ("record-contract", "key-distribution"):
            for flags in ([], ["-O"]):
                with self.subTest(name=name, flags=flags), tempfile.TemporaryDirectory() as tmp:
                    for file in ("generate.py", "golden.json"):
                        shutil.copyfile(ROOT / name / file, Path(tmp) / file)
                    command = [sys.executable, *flags, str(Path(tmp) / "generate.py"), "--check"]
                    good = subprocess.run(command, capture_output=True, text=True)
                    self.assertEqual(good.returncode, 0, good.stderr)
                    golden = Path(tmp) / "golden.json"
                    drift = golden.read_text() + "\n"
                    golden.write_text(drift)
                    bad = subprocess.run(command, capture_output=True, text=True)
                    self.assertNotEqual(bad.returncode, 0)
                    self.assertIn("fixture drift", bad.stderr)
                    self.assertEqual(golden.read_text(), drift, "check must never rewrite evidence")

    def test_pins_in_both_modes(self):
        for name, packages in (("record-contract", ("cbor2", "cryptography")), ("key-distribution", ("cbor2",))):
            for package in packages:
                for flags in ([], ["-O"]):
                    with self.subTest(name=name, package=package, flags=flags):
                        # Override only metadata in the child; installed oracles stay intact.
                        script = (
                            "import importlib.metadata, runpy, sys\n"
                            "original = importlib.metadata.version\n"
                            "importlib.metadata.version = lambda p: '0.0.0' if p == sys.argv[2] else original(p)\n"
                            "runpy.run_path(sys.argv[1], run_name='__main__')\n"
                        )
                        result = subprocess.run([sys.executable, *flags, "-c", script, str(ROOT / name / "generate.py"), package, "--check"], capture_output=True, text=True)
                        self.assertNotEqual(result.returncode, 0)
                        self.assertIn("oracle requires " + package, result.stderr)


if __name__ == "__main__":
    unittest.main()
