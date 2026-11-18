"""Witness evidence drift detection, removals, and the prototype ownership boundary."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest

SOURCE = Path(__file__).parent / "record-contract/check_evidence.py"
spec = importlib.util.spec_from_file_location("evidence_checker", SOURCE)
checker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checker)


class EvidenceGuards(unittest.TestCase):
    def test_each_required_binding_rejects_drift_removal_and_omission(self):
        import hashlib
        self.assertIn("ci/sqlite_runtime.rs", checker.REQUIRED)
        self.assertIn("experiments/sync-prototype/baseline/Cargo.toml", checker.REQUIRED)
        # The baseline shares the app lockfile; it has none of its own to bind.
        self.assertNotIn("experiments/sync-prototype/baseline/Cargo.lock", checker.REQUIRED)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            original_root, original_manifest = checker.ROOT, checker.MANIFEST
            checker.ROOT, checker.MANIFEST = root, root / "manifest.json"
            try:
                evidence = {"source_sha256": {name: hashlib.sha256(b"approved").hexdigest() for name in checker.REQUIRED}}
                for name in checker.REQUIRED:
                    path = root / name
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(b"approved")
                def run():
                    checker.MANIFEST.write_text(json.dumps(evidence))
                    with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                        return checker.main()
                self.assertEqual(run(), 0)
                for name in sorted(checker.REQUIRED):
                    path = root / name
                    with self.subTest(path=name, case="changed"):
                        path.write_bytes(b"changed")
                        self.assertEqual(run(), 1)
                    with self.subTest(path=name, case="deleted"):
                        path.unlink()
                        self.assertEqual(run(), 1)
                    path.write_bytes(b"approved")
                    with self.subTest(path=name, case="omitted hash"):
                        digest = evidence["source_sha256"].pop(name)
                        self.assertEqual(run(), 1)
                        evidence["source_sha256"][name] = digest
                self.assertEqual(run(), 0)
            finally:
                checker.ROOT, checker.MANIFEST = original_root, original_manifest

    def test_required_bindings_stay_inside_prototype_owned_paths(self):
        # The prototype workflow runs on every PR, so binding an app-owned or
        # project-wide CI file would block unrelated app changes. App-side
        # SQLite claims are enforced by tests that app CI runs.
        owned_prefixes = (
            "experiments/sync-prototype/",
            "docs/SYNC_",
            "docs/SQLITE_",
            "docs/protocol/sync-",
            "docs/decisions/0002-post-quantum-sync.md",
            "ci/sqlite_",
            ".github/workflows/sync-prototype.yml",
        )
        outside = sorted(name for name in checker.REQUIRED if not name.startswith(owned_prefixes))
        self.assertEqual(outside, [])
        for name in (".github/workflows/ci.yml", "ci/check.sh", "Cargo.lock", "Cargo.toml"):
            self.assertNotIn(name, checker.REQUIRED)
        evidence = json.loads(checker.MANIFEST.read_text())
        unbound = sorted(name for name in evidence["source_sha256"] if not name.startswith(owned_prefixes))
        self.assertEqual(unbound, [])


if __name__ == "__main__":
    unittest.main()
