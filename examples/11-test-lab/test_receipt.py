import json
import unittest

from receipt import export


class ReceiptTests(unittest.TestCase):
    def test_observation_cannot_replace_receipt_identity(self):
        record = {"id": "rite-original", "name": "report-to-test", "stages": [{
            "stage": "verify", "summon": "s-original", "hash": "hash", "accepted": True,
            "output": {"verdict": "verified_fix", "evidence": {
                "parent": "a", "head": "b", "rows": [{"rite_id": "other", "status": "pass"}]
            }},
        }]}
        self.assertEqual(json.loads(export(record, False))["rite_id"], "rite-original")

    def test_duplicate_stages_are_rejected(self):
        stage = {"stage": "verify", "accepted": True, "output": {"patch": "patch"}}
        record = {"name": "report-to-test", "stages": [stage, stage]}
        with self.assertRaisesRegex(ValueError, "duplicate stage"):
            export(record, True)
