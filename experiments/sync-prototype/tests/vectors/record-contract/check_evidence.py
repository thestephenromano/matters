"""Check the active execution record against the files CI is actually testing.

Historical evidence elsewhere remains scoped to its recorded commits. This active
manifest intentionally fails when its checked files change; refresh it after
testing the new source. The manifest does not hash itself.
"""
import hashlib
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[5]
MANIFEST = ROOT / "experiments/sync-prototype/evidence/record-contract-2026-11-18.json"
REQUIRED = {
    "docs/SYNC_ENCRYPTED_OUTBOX.md",
    "experiments/sync-prototype/tests/support/encrypted_outbox.rs",
    "experiments/sync-prototype/tests/cases/encrypted_outbox.rs",
    "docs/SYNC_DESIGN.md",
    "docs/decisions/0002-post-quantum-sync.md",
    "experiments/sync-prototype/REVIEW_KEY_DISTRIBUTION.md",
    "experiments/sync-prototype/tests/support/record_crypto.rs",
    "experiments/sync-prototype/tests/vectors/check_oracles.py",
    "docs/SYNC_KEY_DISTRIBUTION.md",
    "docs/protocol/sync-key-distribution-v0.cddl",
    "experiments/sync-prototype/tests/key_distribution.rs",
    "experiments/sync-prototype/tests/support/key_distribution.rs",
    "experiments/sync-prototype/tests/vectors/key-distribution/generate.py",
    "experiments/sync-prototype/tests/vectors/key-distribution/golden.json",
    "experiments/sync-prototype/tests/vectors/key-distribution/README.md",
    "experiments/sync-prototype/src/wire.rs",
    "experiments/sync-prototype/tests/record_contract.rs",
    "experiments/sync-prototype/tests/vectors/record-contract/generate.py",
    "experiments/sync-prototype/tests/vectors/record-contract/golden.json",
    "experiments/sync-prototype/tests/vectors/record-contract/check_evidence.py",
    "experiments/sync-prototype/tests/vectors/record-contract/requirements.txt",
    "docs/protocol/sync-record-v0.cddl",
    "docs/SYNC_RECORD_CONTRACT.md",
    "experiments/sync-prototype/Cargo.lock",
    ".github/workflows/sync-prototype.yml",
}


def main():
    evidence = json.loads(MANIFEST.read_text())
    missing = REQUIRED - evidence["source_sha256"].keys()
    if missing:
        print("Execution evidence omits required files: " + ", ".join(sorted(missing)), file=sys.stderr)
        return 1
    mismatches = []
    for name, expected in evidence["source_sha256"].items():
        path = ROOT / name
        actual = hashlib.sha256(path.read_bytes()).hexdigest()
        if actual != expected:
            mismatches.append(name)
    if mismatches:
        print("Execution evidence does not match this checkout: " + ", ".join(mismatches), file=sys.stderr)
        return 1
    print(f"Execution evidence matches {len(evidence['source_sha256'])} checked files.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
