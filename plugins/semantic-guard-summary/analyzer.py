"""Present native DEX guard evidence using the protocol-2 host. Python 3.9+."""
import hashlib
import json
import sys

LABELS = {
    "equals": "Exact equality (case-sensitive)",
    "equals_ignore_case": "Exact equality (case-insensitive)",
    "starts_with": "Prefix comparison",
    "ends_with": "Suffix comparison",
    "contains": "Substring comparison",
}
MAX_FRAME = 2 * 1024 * 1024 + 8192


def null_equality_label(operation, cases):
    if operation not in ("equals", "equals_ignore_case"):
        return None
    strings = sorted({
        condition["argument_index"]
        for case in cases
        for condition in case.get("when", [])
        if condition.get("equals") in ("null", "non_null")
    })
    if len(strings) != 2:
        return None
    expected = {("null", "null"): True, ("null", "non_null"): False, ("non_null", "null"): False}
    covered = set()
    for case in cases:
        conditions = {item["argument_index"]: item["equals"] for item in case.get("when", [])}
        shape = tuple(conditions.get(index) for index in strings)
        if shape in expected:
            outcome = case["outcome"]
            if outcome.get("kind") != "constant" or outcome.get("value") is not expected[shape]:
                return None
            covered.add(shape)
    if covered == set(expected):
        return "Both null returns true. One null returns false."
    return None


def result_card(evidence, project):
    method = evidence.get("method_id")
    site = evidence.get("call_site") or {}
    cases = evidence.get("cases", [])
    proven = evidence.get("status") == "proven"
    outcomes = [case["outcome"] for case in cases]
    if not proven:
        title = "Comparison semantics unresolved"
        explanation = "Some paths require semantics outside the supported DEX proof."
    elif outcomes and all(
        outcome.get("kind") == "constant" and isinstance(outcome.get("value"), bool)
        for outcome in outcomes
    ):
        values = {outcome["value"] for outcome in outcomes}
        if len(values) == 1:
            value = str(outcomes[0]["value"]).lower()
            title = f"Always {value}"
            explanation = f"Every inspected path through the helper body returns {value}."
        else:
            title = "Conditional boolean result"
            explanation = "The helper body returns different boolean values across the listed cases."
    else:
        has_comparison = any(outcome.get("kind") == "comparison" for outcome in outcomes)
        comparison = LABELS.get(evidence.get("operation")) if has_comparison else None
        title = comparison or ("Conditional comparison" if has_comparison else "Helper outcome")
        explanation = "Supported DEX paths establish the listed outcomes inside the helper body."
    observations = []
    packaged_resource = False
    if method:
        observations.append(dict(label="DEX helper", value=method))
    for arg in site.get("arguments", []):
        if arg.get("descriptor") == "Z" and arg.get("status") == "constant" and arg.get("value") in (0, 1):
            observations.append(dict(label=f"Boolean argument {arg['argument_index']}",
                                     value=str(bool(arg["value"])).lower()))
        resource = arg.get("resource", {})
        if resource.get("status") == "resolved":
            packaged_resource = True
            observations.append(dict(label=f"Packaged resource for argument {arg['argument_index']}",
                                     value=f"{resource['resource_name']} = {resource['value']}"))
    if packaged_resource:
        observations.append(dict(label="Resource scope",
                                 value="Packaged values describe the loaded APK. Runtime resource selection requires separate evidence."))
    null_cases = [case for case in cases if any(
        condition.get("equals") == "null" for condition in case.get("when", [])
    )]
    if null_cases:
        null_outcomes = [case["outcome"] for case in null_cases]
        null_equality = null_equality_label(evidence.get("operation"), cases) if proven else None
        if null_equality:
            observations.append(dict(label="Null arguments", value=null_equality))
        elif all(outcome.get("kind") == "constant" and outcome.get("value") is False for outcome in null_outcomes):
            observations.append(dict(label="Null arguments", value="The inspected cases return false."))
        elif any(outcome.get("kind") == "throws_null_pointer" for outcome in null_outcomes):
            observations.append(dict(label="Null arguments", value="Some inspected cases throw a null pointer exception."))
        else:
            observations.append(dict(label="Null arguments", value="See the per-argument cases in the evidence."))
    locations = []
    if site:
        locations.append(dict(label="Caller invoke", method_id=site["caller_method_id"], offset=site["code_unit_offset"]))
    seen = set()
    for case in cases:
        outcome = case["outcome"]
        point = (outcome.get("method_id"), outcome.get("code_unit_offset"))
        if point[0] and point[1] is not None and point not in seen:
            seen.add(point)
            label = "Boolean return" if outcome.get("kind") == "constant" else "Comparison instruction"
            locations.append(dict(label=label, method_id=point[0], offset=point[1]))
    key = json.dumps([project.get("apk_sha256"), method, site.get("caller_method_id"), site.get("code_unit_offset")])
    item = {
        "group_id": hashlib.sha256(key.encode()).hexdigest(),
        "label": title,
        "method_id": method,
        "guard": evidence,
        "human": {
            "title": title,
            "explanation": explanation,
            "observations": observations,
            "locations": locations,
        },
    }
    return {
        "title": "Semantic guard summary",
        "summary": title,
        "items": [item],
        "coverage": {"status": "complete" if proven else "partial", "scope": evidence.get("scope")},
        "apk_sha256": project.get("apk_sha256"),
    }


def read_frame():
    line = sys.stdin.buffer.readline(MAX_FRAME + 1)
    if not line.endswith(b"\n") or len(line) > MAX_FRAME:
        raise ValueError("Missing or oversized protocol frame")
    return json.loads(line)


def send(message):
    print(json.dumps(message), flush=True)


def main():
    request = read_frame()
    request_id = request.get("id")
    try:
        if request.get("protocol") != 2 or request.get("method") != "project.analyze":
            raise ValueError("Expected a protocol-2 project analysis request")
        arguments = request.get("arguments", {})
        selectors = set(arguments) if isinstance(arguments, dict) else set()
        method_aliases = {"method_id", "method_signature", "signature"}
        if not selectors or not (
            selectors <= method_aliases or selectors == {"caller_method_id", "call_site_offset"}
        ):
            raise ValueError("Supply method_id or caller_method_id with call_site_offset")
        send(dict(protocol=2, id=request_id, method="host.query", query_id=1,
                  tool="get_guard_summary", arguments=arguments))
        reply = read_frame()
        if reply.get("protocol") != 2 or reply.get("id") != request_id or reply.get("query_id") != 1:
            raise ValueError("Host response identity mismatch")
        if "error" in reply:
            raise ValueError(reply["error"])
        send(dict(protocol=2, id=request_id, result=result_card(reply["result"], request.get("project", {}))))
    except (KeyError, ValueError) as error:
        send(dict(protocol=2, id=request_id, error=str(error)))


if __name__ == "__main__":
    main()
