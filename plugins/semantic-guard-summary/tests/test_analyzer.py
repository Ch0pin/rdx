import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import unittest

PLUGIN = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("semantic_guard_summary", PLUGIN / "analyzer.py")
ANALYZER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ANALYZER)


class GuardPresenterTests(unittest.TestCase):
    def test_protocol_and_evidence(self):
        manifest = json.loads((PLUGIN / "plugin.json").read_text())
        self.assertEqual(manifest["protocol"], 2)
        self.assertEqual(manifest["capability"], "project.analyze")
        self.assertEqual(manifest["executable"], "python3")
        self.assertNotIn("enabled", manifest)
        arguments = {"caller_method_id": "example.Caller.run()V", "call_site_offset": 38}
        evidence = {
            "status": "proven",
            "operation": "equals",
            "method_id": "example.Guard.equal(Ljava/lang/String;Ljava/lang/String;Z)Z",
            "call_site": {
                "caller_method_id": arguments["caller_method_id"],
                "code_unit_offset": 38,
                "arguments": [{"descriptor": "Z", "status": "constant", "value": 0, "argument_index": 2}],
            },
            "cases": [
                {"when": [{"argument_index": 0, "equals": "null"}],
                 "outcome": {"kind": "constant", "value": False}},
                {"when": [{"argument_index": 0, "equals": "non_null"}],
                 "outcome": {"kind": "comparison", "operation": "equals"}},
            ],
        }
        request = {"protocol": 2, "id": 4, "method": "project.analyze",
                   "project": {"apk_sha256": "f" * 64}, "arguments": arguments}
        host_reply = {"protocol": 2, "id": 4, "query_id": 1, "result": evidence}
        completed = subprocess.run(
            [sys.executable, str(PLUGIN / "analyzer.py")],
            input=json.dumps(request) + "\n" + json.dumps(host_reply) + "\n",
            text=True, capture_output=True, check=True, timeout=5,
        )
        query, reply = map(json.loads, completed.stdout.splitlines())
        self.assertEqual(query["tool"], "get_guard_summary")
        self.assertEqual(query["arguments"], arguments)
        card = reply["result"]["items"][0]
        self.assertEqual(card["guard"], evidence)
        self.assertEqual(card["human"]["title"], "Exact equality (case-sensitive)")
        self.assertIn({"label": "Boolean argument 2", "value": "false"}, card["human"]["observations"])

    def test_partial_evidence_remains_unresolved(self):
        card = ANALYZER.result_card({"status": "unknown", "operation": "equals"}, {})
        self.assertEqual(card["coverage"]["status"], "partial")
        self.assertEqual(card["items"][0]["human"]["title"], "Comparison semantics unresolved")

    def test_proven_constant_boolean_results_have_accurate_titles(self):
        for values, title in (
            ([True, True], "Always true"),
            ([False], "Always false"),
            ([True, False], "Conditional boolean result"),
        ):
            with self.subTest(values=values):
                cases = [{"outcome": {"kind": "constant", "value": value}} for value in values]
                result = ANALYZER.result_card({"status": "proven", "operation": None, "cases": cases}, {})
                self.assertEqual(result["summary"], title)
                self.assertIn("helper body", result["items"][0]["human"]["explanation"])

    def test_multiple_comparison_operators_keep_conditional_comparison_title(self):
        cases = [{"outcome": {"kind": "comparison", "operation": operation}}
                 for operation in ("equals", "equals_ignore_case")]
        result = ANALYZER.result_card({"status": "proven", "operation": None, "cases": cases}, {})
        self.assertEqual(result["items"][0]["human"]["title"], "Conditional comparison")

    def test_null_safe_equality_and_packaged_value_card(self):
        cases = [{
            "when": [{"argument_index": 0, "equals": left}, {"argument_index": 1, "equals": right},
                     {"argument_index": 2, "equals": False}],
            "outcome": {"kind": "constant", "value": value},
        } for left, right, value in (
            ("null", "null", True), ("null", "non_null", False), ("non_null", "null", False)
        )]
        cases.append({
            "when": [{"argument_index": 0, "equals": "non_null"}, {"argument_index": 1, "equals": "non_null"},
                     {"argument_index": 2, "equals": False}],
            "outcome": {"kind": "comparison", "operation": "equals"},
        })
        evidence = {
            "status": "proven",
            "operation": "equals",
            "method_id": "example.Guard.equal(Ljava/lang/String;Ljava/lang/String;Z)Z",
            "cases": cases,
            "call_site": {"caller_method_id": "example.Caller.run()V", "code_unit_offset": 38,
                          "arguments": [{"argument_index": 1, "resource": {
                              "status": "resolved", "resource_name": "example:string/domain",
                              "value": ".example.com",
                          }}]},
        }
        human = ANALYZER.result_card(evidence, {})["items"][0]["human"]
        self.assertIn({"label": "Null arguments", "value": "Both null returns true. One null returns false."},
                      human["observations"])
        self.assertIn({"label": "Packaged resource for argument 1", "value": "example:string/domain = .example.com"},
                      human["observations"])
        self.assertIn({"label": "Resource scope", "value":
                       "Packaged values describe the loaded APK. Runtime resource selection requires separate evidence."},
                      human["observations"])
        cases[-2]["outcome"]["value"] = True
        self.assertIsNone(ANALYZER.null_equality_label("equals", cases))


if __name__ == "__main__":
    unittest.main()
