"""Evidence-led WebView groups over RDX's loaded DEX project. Python 3.9+, no dependencies."""
import hashlib
import json
import re
import sys

SINKS = [
    "loadUrl(Ljava/lang/String;)V",
    "loadUrl(Ljava/lang/String;Ljava/util/Map;)V",
    "loadData(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)V",
    "loadDataWithBaseURL(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)V",
    "evaluateJavascript(Ljava/lang/String;Landroid/webkit/ValueCallback;)V",
]
LIMITATIONS = [
    "Groups identify static receiver origins, not unique runtime WebView instances.",
    "Receiver origins follow bounded control flow; joins keep only agreed origins. Switches, unsupported alias chains and ambiguous paths remain unresolved.",
    "Client registrations are local candidates on the same receiver origin; their runtime ordering is not inferred.",
    "Host candidates use declared caller ancestry. Compose routes, navigation reachability, field writers in other methods, reflection and native calls are not inferred.",
    "Only the listed platform load/evaluate signatures and their indexed inherited aliases are queried. Overrides may require separate tracing.",
]


class Host:
    def __init__(self, request):
        self.request_id = request["id"]
        self.serial = 0

    def query(self, tool, arguments):
        if self.serial >= 450:
            raise ValueError("Plugin query budget reached (450); narrow package_prefix or max_sites")
        self.serial += 1
        message = dict(protocol=2, id=self.request_id, method="host.query",
                       query_id=self.serial, tool=tool, arguments=arguments)
        print(json.dumps(message), flush=True)
        line = sys.stdin.buffer.readline(8 * 1024 * 1024 + 1)
        if len(line) > 8 * 1024 * 1024:
            raise ValueError("Host response exceeds 8 MiB")
        answer = json.loads(line)
        if answer.get("query_id") != self.serial or answer.get("id") != self.request_id or answer.get("protocol") != 2:
            raise ValueError("Host response identity mismatch")
        if "error" in answer:
            raise ValueError(answer["error"])
        return answer["result"]


def owner(method):
    return method.split("(", 1)[0].rsplit(".", 1)[0]


def regs(operand):
    region = re.search(r"\{([^}]*)\}", operand)
    if not region:
        return []
    text = region.group(1)
    values = [int(x) for x in re.findall(r"v(\d+)", text)]
    if ".." in text and len(values) == 2:
        return list(range(values[0], values[1] + 1))
    return values


def parameters(method):
    signature = method.split("(", 1)[1].split(")", 1)[0]
    return re.findall(r"\[*L[^;]+;|\[*[ZBCSIJFD]", signature)


def origin(kind, key, **details):
    return dict(kind=kind, key=key, **details)


def analyze_dex(method, text, report=None):
    """Bounded must-origin propagation. A join keeps only facts agreed by every incoming path."""
    from collections import deque
    instructions = []
    for line in text.splitlines():
        match = re.match(r"\s*([0-9a-f]+):\s+([\w/-]+)\s*(.*?)\s*(?://.*)?$", line)
        if match:
            instructions.append((int(match[1], 16), match[2], match[3].split(" //", 1)[0]))
    header = re.search(r"\.registers (\d+) // inputs=(\d+)", text)
    access = re.search(r"// access=0x([0-9a-f]+)", text)
    initial = {}
    if header and access:
        first = int(header[1]) - int(header[2])
        if not int(access[1], 16) & 8:
            initial[first] = origin("this", "this:" + owner(method), type=owner(method))
            first += 1
        for index, ty in enumerate(parameters(method)):
            initial[first] = origin("parameter", method + ":arg" + str(index), parameter_index=index, descriptor=ty)
            first += 2 if ty in ("J", "D") else 1
    by_pc = {pc: (opcode, operand) for pc, opcode, operand in instructions}
    normal = {}
    exceptional = {}
    status = "complete_supported_cfg"
    if not header or not access or not instructions or len(instructions) > 4096 or int(header[1]) > 512:
        status = "unsupported_or_budget"
    if any("switch" in opcode for _, opcode, _ in instructions):
        status = "switch_cfg_not_supported"
    handlers = [(int(start, 16), int(end, 16), int(handler, 16)) for start, end, handler in
                re.findall(r"\.catch[^\n]*?\{\s*@([0-9a-f]+)\s*\.\.\s*@([0-9a-f]+)\s*\}\s*@([0-9a-f]+)", text)]
    for index, (pc, opcode, operand) in enumerate(instructions):
        following = [instructions[index + 1][0]] if index + 1 < len(instructions) else []
        targets = [int(x, 16) for x in re.findall(r"@([0-9a-f]+)", operand)]
        if opcode.startswith("goto"):
            normal[pc] = targets
        elif opcode.startswith("if-"):
            normal[pc] = following + targets
        elif opcode.startswith(("return", "throw")):
            normal[pc] = []
        else:
            normal[pc] = following
        exceptional[pc] = [handler for start, end, handler in handlers if start <= pc < end]
        if any(target not in by_pc for target in normal[pc] + exceptional[pc]):
            status = "invalid_cfg_target"

    def transfer(pc, incoming):
        opcode, operand = by_pc[pc]
        rr = [int(x) for x in re.findall(r"\bv(\d+)\b", operand)]
        state = dict(incoming)
        pending = state.pop(-1, None)
        if opcode.startswith("move-result"):
            if rr:
                state.pop(rr[0], None)
                if pending and opcode == "move-result-object":
                    state[rr[0]] = pending
                if "wide" in opcode:
                    state.pop(rr[0] + 1, None)
        elif opcode.startswith("invoke-"):
            target = operand.split("},", 1)[-1].strip()
            if ")L" in target or ")[" in target:
                state[-1] = origin("call_result", method + "@" + hex(pc), target_method_id=target)
        elif opcode.startswith("move") and len(rr) >= 2:
            value = incoming.get(rr[1])
            state.pop(rr[0], None)
            if value:
                state[rr[0]] = value
            if "wide" in opcode:
                state.pop(rr[0] + 1, None)
        elif opcode == "new-instance" and rr:
            state[rr[0]] = origin("allocation", method + "@" + hex(pc), type=operand.split(",", 1)[1].strip(), allocation_offset=pc)
        elif opcode in ("iget-object", "sget-object") and rr:
            field = operand.rsplit(",", 1)[-1].strip()
            base = incoming.get(rr[1]) if opcode == "iget-object" and len(rr) > 1 else origin("static", "static")
            state.pop(rr[0], None)
            if base and base["kind"] in ("this", "parameter", "allocation", "static"):
                state[rr[0]] = origin("field", field + ":base=" + base["key"], field_id=field, base=base)
        elif opcode == "check-cast":
            pass
        elif rr and not opcode.startswith(("if-", "goto", "return", "throw", "iput", "sput", "aput", "monitor", "filled-new-array", "fill-array-data")):
            state.pop(rr[0], None)
            if "wide" in opcode or (opcode.split("/")[0].endswith(("-long", "-double")) and not opcode.startswith("cmp")):
                state.pop(rr[0] + 1, None)
        return state

    states = {}
    steps = 0
    if status == "complete_supported_cfg":
        states[instructions[0][0]] = initial
        queue = deque([instructions[0][0]])
        queued = set(queue)
        retained = len(initial)
        while queue:
            pc = queue.popleft(); queued.remove(pc)
            steps += 1
            if steps > 25000 or retained > 200000:
                status = "dataflow_budget_exhausted"; states = {}; break
            before = states[pc]
            after = transfer(pc, before)
            exception_state = {r: value for r, value in before.items() if r != -1}
            for targets, candidate in ((normal[pc], after), (exceptional[pc], exception_state)):
                for target in targets:
                    previous = states.get(target)
                    merged = dict(candidate) if previous is None else {r: value for r, value in previous.items() if candidate.get(r) == value}
                    if previous is None or merged != previous:
                        retained += len(merged) - (len(previous) if previous is not None else 0)
                        states[target] = merged
                        if target not in queued:
                            queue.append(target); queued.add(target)
    returned = set()
    for pc, opcode, operand in instructions:
        if opcode == "return-object":
            rr = [int(x) for x in re.findall(r"\bv(\d+)\b", operand)]
            value = states.get(pc, {}).get(rr[0]) if rr else None
            if value and value["kind"] == "allocation":
                returned.add(value["key"])
    calls = {}; clients = []
    for pc, opcode, operand in instructions:
        if not opcode.startswith("invoke-"):
            continue
        arguments = regs(operand)
        target = operand.split("},", 1)[-1].strip()
        state = states.get(pc, {})
        receiver = state.get(arguments[0]) if arguments and "static" not in opcode else None
        receiver = dict(receiver) if receiver else origin("unresolved", method + "@" + hex(pc))
        if receiver["key"] in returned:
            receiver["returned_by"] = method
        calls[pc] = dict(receiver=receiver, target=target, registers=arguments)
        if (".setWebViewClient(" in target or ".setWebChromeClient(" in target) and len(arguments) >= 2:
            clients.append(dict(offset=pc, method_id=method, registration_method=target,
                                receiver=receiver, client=state.get(arguments[1]) or {"kind": "unresolved"}))
    if report is not None:
        report.update(status=status, instruction_count=len(instructions), transfer_steps=steps,
                      exceptional_flow="conservative pre-instruction state for all protected instructions")
    return calls, clients


def short_method(method):
    """Display-only spelling; exact binary selectors stay in the evidence."""
    head = method.split("(", 1)[0]
    parts = head.rsplit(".", 2)
    return ".".join(parts[-2:]) + "()"


def human_view(group):
    receiver = group["receiver_origin"]
    method = group["method_id"]
    kind = receiver["kind"]
    if kind == "field":
        name = receiver["field_id"].split(":", 1)[0]
        short = ".".join(name.rsplit(".", 2)[-2:])
        title = name
        explanation = "The app keeps this WebView in " + short + ". The code locations below use that stored reference."
    elif kind == "allocation":
        title = method.split("(", 1)[0] + "()"
        explanation = "This code creates a WebView and uses it to display content."
    elif kind == "parameter":
        title = method.split("(", 1)[0] + "() · argument " + str(receiver["parameter_index"] + 1)
        explanation = "This method receives a WebView from its caller as argument " + str(receiver["parameter_index"] + 1) + ". The scan has not connected it to a specific creation site."
    elif kind == "call_result":
        target = receiver["target_method_id"]
        title = target.split("(", 1)[0] + "()"
        explanation = "The app calls " + short_method(target) + " and uses the returned WebView. Its creation was not traced further."
    elif kind == "this":
        title = receiver.get("type", owner(method))
        explanation = "This method uses the WebView object it belongs to."
    else:
        title = method.split("(", 1)[0] + "()"
        explanation = ""
    clients = sorted({x["client"]["type"].rsplit(".", 1)[-1] for x in group["client_registration_candidates"] if x["client"].get("type")})
    hosts = sorted({x["class_id"] for x in group["host_candidates"]})
    creation = ("Created here and returned by " + short_method(group["factory_candidate"]) if group.get("factory_candidate") else
                "Created in this method" if kind == "allocation" else
                "Not determined from the inspected code")
    observations = ([dict(label="Containing Activity/Fragment:", value=", ".join(hosts))] if hosts else []) + [
        dict(label="WebViewClient / WebChromeClient:", value=", ".join(clients) + " — assigned in inspected code" if clients else "No assignment found in inspected methods; setup may happen elsewhere"),
        dict(label="Creation", value=creation),
    ]
    locations = [dict(label=("Run JavaScript" if ".evaluateJavascript(" in x["target"] else "Load content") + " in " + short_method(x["method_id"]), method_id=x["method_id"], offset=x["offset"]) for x in group["sinks"]]
    return dict(title=title, explanation=explanation, observations=observations, locations=locations,
                technical_note="The scan could not trace which WebView receives this call." if kind == "unresolved" else None)


def inventory(host, arguments, project):
    allowed = {"max_sites", "package_prefix", "include_callers"}
    if set(arguments) - allowed:
        raise ValueError("Accepted arguments: max_sites, package_prefix, include_callers")
    maximum = arguments.get("max_sites", 100)
    if type(maximum) is not int or not 1 <= maximum <= 200:
        raise ValueError("max_sites must be an integer in 1..200")
    prefix = arguments.get("package_prefix", "")
    if not isinstance(prefix, str):
        raise ValueError("package_prefix must be a string")
    prefix = prefix.rstrip(".")
    include_callers = arguments.get("include_callers", True)
    if not isinstance(include_callers, bool):
        raise ValueError("include_callers must be boolean")
    sites = []
    queries = []
    errors = []
    for sink in SINKS:
        target = "android.webkit.WebView." + sink
        query = dict(method_id=target, direction="callers", projection="call_sites", depth=1, limit=100)
        if prefix:
            query["package_prefix"] = prefix
        offset = 0
        while True:
            if len(sites) >= maximum:
                queries.append(dict(target=target, status="site_budget", next_offset=offset))
                break
            query["offset"] = offset
            query["limit"] = min(100, maximum-len(sites))
            try:
                response = host.query("get_call_graph", query)
            except ValueError as error:
                errors.append(dict(tool="get_call_graph", target=target, error=str(error)))
                break
            rows = response.get("call_sites", [])
            sites.extend(rows)
            queries.append(dict(target=target, total=response.get("matching_total"), returned=len(rows),
                                next_offset=response.get("next_offset"), scan_complete=response.get("scan_complete")))
            if response.get("next_offset") is None:
                break
            offset = response["next_offset"]
    methods = {}
    flow_reports = {}
    groups = {}
    hierarchies = {}
    callers = {}

    def host_screen(name):
        chain = []
        current = name
        for _ in range(12):
            if current in ("android.app.Activity", "android.app.Fragment", "androidx.fragment.app.Fragment"):
                return dict(class_id=name, host_kind=current, ancestry=chain, relationship="declared_ancestry")
            if current not in hierarchies:
                try:
                    hierarchies[current] = host.query("get_class_hierarchy", {"class_name": current})
                except ValueError:
                    hierarchies[current] = {}
            parent = hierarchies[current].get("superclass") or {}
            current = parent.get("class_id")
            if not current or current in chain:
                break
            chain.append(current)
        return None

    for site in sites:
        method = site["caller_method_id"]
        pc = site["call_site_offset"]
        if method not in methods:
            try:
                response = host.query("get_method_source", {"method_id": method, "format": "dex", "limit": 128000})
                if response.get("next_offset") is not None:
                    errors.append(dict(method_id=method, error="DEX body exceeds 128000 character analysis budget"))
                    methods[method] = ({}, [])
                else:
                    flow_reports[method] = {}
                    methods[method] = analyze_dex(method, response["text"], flow_reports[method])
            except ValueError as error:
                errors.append(dict(method_id=method, error=str(error)))
                methods[method] = ({}, [])
        calls, registrations = methods[method]
        call = calls.get(pc, {})
        receiver = call.get("receiver") or origin("unresolved", method + "@" + hex(pc))
        key = receiver["key"]
        gid = hashlib.sha256((project["apk_sha256"] + "|" + key).encode()).hexdigest()[:20]
        if gid not in groups:
            if receiver["kind"] == "field":
                label = "Field · " + receiver["field_id"].split(":", 1)[0]
            elif receiver["kind"] == "parameter":
                label = "Parameter " + str(receiver["parameter_index"]) + " · " + method.split("(", 1)[0]
            else:
                label = receiver["kind"].capitalize() + " · " + method.split("(", 1)[0] + " @ " + hex(pc)
            groups[gid] = dict(group_id=gid, label=label,
                               method_id=method, receiver_origin=receiver, sinks=[],
                               client_registration_candidates=[], caller_sites=[], host_candidates=[],
                               runtime_instance_id=None, runtime_verified=False)
        group = groups[gid]
        group["sinks"].append(dict(method_id=method, offset=pc, dex_entry=site.get("dex_entry"),
                                   target=site["callee_method_id"], raw_target=site.get("raw_callee_method_id"),
                                   receiver_register=(call.get("registers") or [None])[0]))
        for client in registrations:
            if receiver["kind"] != "unresolved" and client["receiver"]["key"] == key and client not in group["client_registration_candidates"]:
                group["client_registration_candidates"].append(client)
        screen = host_screen(owner(method))
        if screen and screen not in group["host_candidates"]:
            group["host_candidates"].append(screen)
        if include_callers:
            if method not in callers:
                try:
                    response = host.query("get_call_graph", dict(method_id=method, direction="callers", projection="call_sites", depth=1, limit=10))
                    callers[method] = response.get("call_sites", [])
                    if response.get("next_offset") is not None:
                        errors.append(dict(method_id=method, error="Caller context limited to 10 sites; query get_call_graph to continue"))
                except ValueError as error:
                    callers[method] = []
                    errors.append(dict(method_id=method, error=str(error)))
            for caller in callers[method]:
                compact = {k: caller.get(k) for k in ("caller_method_id", "call_site_offset", "callee_method_id", "dex_entry")}
                if compact not in group["caller_sites"]:
                    group["caller_sites"].append(compact)
    for group in groups.values():
        group["description"] = f"{len(group['sinks'])} sink site(s) · {len(group['client_registration_candidates'])} local client registration(s) · {len(group['caller_sites'])} caller site(s)"
        group["factory_candidate"] = group["receiver_origin"].get("returned_by")
        client_names = sorted({c["client"].get("type", c["client"].get("kind", "unresolved")) for c in group["client_registration_candidates"]})
        host_names = sorted({h["class_id"] for h in group["host_candidates"]})
        group["facts"] = [dict(label="Client candidates", value=", ".join(client_names) or "unresolved"),
                          dict(label="Host candidates", value=", ".join(host_names) or "unresolved"),
                          dict(label="Factory candidate", value=group["factory_candidate"] or "unresolved"),
                          dict(label="Runtime identity", value="Not observed; this is a static receiver group")]

    for group in groups.values():
        group["human"] = human_view(group)

    complete = not errors and all(q.get("status") != "site_budget" and q.get("next_offset") is None and q.get("scan_complete") for q in queries)
    identified = sum(len(g["sinks"]) for g in groups.values() if g["receiver_origin"]["kind"] != "unresolved")
    scan_note = "Finished scanning the supported WebView calls in this scope." if complete else "Some code remains unchecked or could not be analyzed. Open Scan details for the limits and reasons."
    scan_note += f" The WebView reference could be traced for {identified} of {len(sites)} call locations."
    return dict(title="WebView inventory", summary=f"Found {len(sites)} code locations that load content or run JavaScript, grouped into {len(groups)} WebView references.",
                human_note="This describes references in the app's code. A reference can be used by multiple WebViews while the app runs. Containing Activity/Fragment identifies where the code is declared; the visible screen requires runtime confirmation.",
                scan_note=scan_note,
                items=list(groups.values()), coverage=dict(status="scanned_supported_sinks" if complete else "partial",
                sink_scan_complete=complete, runtime_mapping_complete=False, max_sites=maximum, package_prefix=prefix,
                queries=queries, diagnostics=errors, limitations=LIMITATIONS,
                receiver_tracking={"identified_sites":sum(len(g["sinks"]) for g in groups.values() if g["receiver_origin"]["kind"] != "unresolved"),
                                   "unidentified_sites":sum(len(g["sinks"]) for g in groups.values() if g["receiver_origin"]["kind"] == "unresolved"),
                                   "method_status_counts":{status:sum(r["status"]==status for r in flow_reports.values()) for status in sorted({r["status"] for r in flow_reports.values()})},
                                   "unsupported_methods":[{"method_id":m,**r} for m,r in flow_reports.items() if r["status"]!="complete_supported_cfg"][:20],
                                   "unsupported_methods_truncated":sum(r["status"]!="complete_supported_cfg" for r in flow_reports.values())>20}))


def main():
    request = json.loads(sys.stdin.buffer.readline(65537))
    try:
        if request.get("protocol") != 2 or request.get("method") != "project.analyze":
            raise ValueError("Requires RDX project plugin protocol 2")
        result = inventory(Host(request), request.get("arguments", {}), request["project"])
        response = dict(protocol=2, id=request["id"], result=result)
    except Exception as error:
        response = dict(protocol=2, id=request.get("id"), error=str(error))
    print(json.dumps(response), flush=True)


if __name__ == "__main__":
    main()
