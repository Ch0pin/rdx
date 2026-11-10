import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("analyzer", Path(__file__).parents[1] / "analyzer.py")
analyzer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(analyzer)


class ReceiverTests(unittest.TestCase):
    def run_dex(self, body, method="demo.Screen.load()V", registers=5, inputs=1):
        return analyzer.analyze_dex(method, f".method {method} // access=0x1\n.registers {registers} // inputs={inputs}\n" + body)

    def test_separate_fields_and_moves(self):
        calls, _ = self.run_dex('''
0000: iget-object v0, v4, demo.Screen.first:Landroid/webkit/WebView;
0002: move-object v1, v0
0003: invoke-virtual {v1, v2}, android.webkit.WebView.loadUrl(Ljava/lang/String;)V
0006: iget-object v0, v4, demo.Screen.second:Landroid/webkit/WebView;
0008: invoke-virtual {v0, v2}, android.webkit.WebView.loadUrl(Ljava/lang/String;)V
''')
        self.assertEqual(calls[3]["receiver"]["kind"], "field")
        self.assertNotEqual(calls[3]["receiver"]["key"], calls[8]["receiver"]["key"])

    def test_parameter_words_include_wide_inputs(self):
        calls, _ = self.run_dex('''0000: invoke-virtual {v3, v4}, android.webkit.WebView.loadUrl(Ljava/lang/String;)V''',
                                method="demo.Screen.load(JLandroid/webkit/WebView;Ljava/lang/String;)V", registers=5, inputs=5)
        self.assertEqual(calls[0]["receiver"]["parameter_index"], 1)

    def test_join_does_not_inherit_false_branch_receiver(self):
        calls, _ = self.run_dex('''
0000: iget-object v0, v4, demo.Screen.first:Landroid/webkit/WebView;
0002: if-eqz v1, @0008
0004: iget-object v0, v4, demo.Screen.second:Landroid/webkit/WebView;
0006: goto @0008
0008: invoke-virtual {v0, v2}, android.webkit.WebView.loadUrl(Ljava/lang/String;)V
''')
        self.assertEqual(calls[8]["receiver"]["kind"], "unresolved")

    def test_client_registration_same_allocation(self):
        calls, clients = self.run_dex('''
0000: new-instance v0, android.webkit.WebView
0002: new-instance v1, demo.Client
0004: invoke-virtual {v0, v1}, android.webkit.WebView.setWebViewClient(Landroid/webkit/WebViewClient;)V
0007: invoke-virtual {v0, v2}, android.webkit.WebView.loadUrl(Ljava/lang/String;)V
''')
        self.assertEqual(calls[7]["receiver"]["key"], clients[0]["receiver"]["key"])
        self.assertEqual(clients[0]["client"]["type"], "demo.Client")

    def test_register_overwrite_and_switch_clear_provenance(self):
        calls, _ = self.run_dex('''
0000: new-instance v0, android.webkit.WebView
0002: const/4 v0, 0
0003: invoke-virtual {v0, v1}, android.webkit.WebView.loadUrl(Ljava/lang/String;)V
0006: packed-switch v1, @0020
0009: new-instance v0, android.webkit.WebView
000b: invoke-virtual {v0, v1}, android.webkit.WebView.loadUrl(Ljava/lang/String;)V
''')
        self.assertEqual(calls[3]["receiver"]["kind"], "unresolved")
        self.assertEqual(calls[11]["receiver"]["kind"], "unresolved")

    def test_range_registers(self):
        self.assertEqual(analyzer.regs("{v12 .. v14}, demo.C.x()V"), [12, 13, 14])

    def test_branch_and_null_guard_preserve_distinct_receiver_fields(self):
        calls, _ = self.run_dex('''
0000: if-eqz v2, @0008
0002: iget-object v0, v4, demo.Screen.secondary:Landroid/webkit/WebView;
0004: invoke-virtual {v0, v3}, android.webkit.WebView.loadUrl(Ljava/lang/String;)V
0007: return-void
0008: iget-object v0, v4, demo.Screen.primary:Landroid/webkit/WebView;
000a: if-eqz v0, @000f
000c: invoke-virtual {v0, v3}, android.webkit.WebView.loadUrl(Ljava/lang/String;)V
000f: return-void
''')
        self.assertIn("secondary", calls[4]["receiver"]["field_id"])
        self.assertIn("primary", calls[12]["receiver"]["field_id"])

    def test_loop_keeps_unchanged_this_but_rejects_conflicting_receiver(self):
        calls, _ = self.run_dex('''
0000: iget-object v0, v4, demo.Screen.first:Landroid/webkit/WebView;
0002: if-eqz v1, @0008
0004: iget-object v0, v4, demo.Screen.second:Landroid/webkit/WebView;
0006: goto @0002
0008: invoke-virtual {v0, v2}, android.webkit.WebView.loadUrl(Ljava/lang/String;)V
000b: invoke-super {v4, v2}, android.webkit.WebView.loadUrl(Ljava/lang/String;)V
000e: return-void
''')
        self.assertEqual(calls[8]["receiver"]["kind"], "unresolved")
        self.assertEqual(calls[11]["receiver"]["kind"], "this")

    def test_exception_uses_pre_instruction_register_state(self):
        calls, _ = self.run_dex('''
0000: iget-object v0, v4, demo.Screen.first:Landroid/webkit/WebView;
0002: iget-object v0, v4, demo.Screen.second:Landroid/webkit/WebView;
0004: return-void
0005: move-exception v2
0006: invoke-virtual {v0, v3}, android.webkit.WebView.loadUrl(Ljava/lang/String;)V
0009: return-void
.catchall { @0002 .. @0004 } @0005
''')
        self.assertIn("first", calls[6]["receiver"]["field_id"])

    def test_wide_writes_invalidate_both_register_words(self):
        for instruction in ["const-wide/16 v0, 0", "int-to-long v0, v2", "move-result-wide v0"]:
            calls, _ = self.run_dex("0000: new-instance v1, android.webkit.WebView\n0002: " + instruction + "\n0004: invoke-virtual {v1, v2}, android.webkit.WebView.loadUrl(Ljava/lang/String;)V")
            self.assertEqual(calls[4]["receiver"]["kind"], "unresolved", instruction)

    def test_factory_requires_return_of_same_allocation(self):
        calls, _ = self.run_dex('''
0000: new-instance v0, android.webkit.WebView
0002: invoke-virtual {v0, v1}, android.webkit.WebView.loadUrl(Ljava/lang/String;)V
0005: return-object v0
''')
        self.assertEqual(calls[2]["receiver"]["returned_by"], "demo.Screen.load()V")
        calls, _ = self.run_dex('''
0000: new-instance v0, android.webkit.WebView
0002: invoke-virtual {v0, v1}, android.webkit.WebView.loadUrl(Ljava/lang/String;)V
0005: return-object v1
''')
        self.assertNotIn("returned_by", calls[2]["receiver"])

    def test_budget_does_not_claim_complete_inventory(self):
        class Host:
            def query(self, tool, args):
                if tool == "get_call_graph":
                    return {"call_sites": [{"caller_method_id": "demo.C.m()V", "call_site_offset": 0, "callee_method_id": args["method_id"]}], "next_offset": 1, "matching_total": 10, "scan_complete": True}
                if tool == "get_method_source":
                    return {"text": ".method demo.C.m()V // access=0x1\n.registers 2 // inputs=1\n0000: invoke-virtual {v0, v1}, android.webkit.WebView.loadUrl(Ljava/lang/String;)V", "next_offset": None}
                return {}
        result = analyzer.inventory(Host(), {"max_sites": 1, "include_callers": False}, {"apk_sha256": "test"})
        self.assertEqual(result["coverage"]["status"], "partial")
        self.assertFalse(result["coverage"]["runtime_mapping_complete"])
        self.assertEqual(len(result["items"]), 1)

    def test_human_view_explains_unknown_origin_without_claiming_absence(self):
        group = {"method_id": "demo.Screen.load()V", "receiver_origin": analyzer.origin("unresolved", "x"),
                 "sinks": [{"method_id": "demo.Screen.load()V", "offset": 5, "target": "android.webkit.WebView.loadUrl(Ljava/lang/String;)V"}],
                 "client_registration_candidates": [], "host_candidates": [], "factory_candidate": None}
        human = analyzer.human_view(group)
        self.assertEqual(human["explanation"], "")
        self.assertIn("could not trace which WebView", human["technical_note"])
        self.assertNotIn("unresolved", str(human))
        self.assertIn("setup may happen elsewhere", str(human))
        self.assertEqual(human["locations"][0]["method_id"], group["method_id"])

    def test_human_view_keeps_field_and_returned_object_distinct(self):
        group = {"method_id": "demo.Screen.load()V", "receiver_origin": analyzer.origin("field", "x", field_id="demo.Screen.help:Landroid/webkit/WebView;"),
                 "sinks": [], "client_registration_candidates": [], "host_candidates": [], "factory_candidate": None}
        human = analyzer.human_view(group)
        self.assertEqual(human["title"], "demo.Screen.help")
        self.assertIn("keeps this WebView", human["explanation"])
        group["receiver_origin"] = analyzer.origin("call_result", "x", target_method_id="demo.Factory.get()Landroid/webkit/WebView;")
        human = analyzer.human_view(group)
        self.assertEqual(human["title"], "demo.Factory.get()")
        self.assertIn("creation was not traced", human["explanation"])


if __name__ == "__main__":
    unittest.main()
