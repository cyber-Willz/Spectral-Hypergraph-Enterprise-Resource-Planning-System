#!/usr/bin/env python3
"""v5 smoke test of the cognitive layer. Needs the demo history:
   psql "$DATABASE_URL" -f scripts/scm/seed_cognitive_demo.sql   (empty database, after migrations)
Usage: BASE=http://localhost:8080/api/scm python3 scripts/scm/smoke_test_v5.py"""
import json, os, urllib.request, urllib.error

BASE = os.environ.get("BASE", "http://localhost:8080/api/scm")


def _login():
    root = BASE.rsplit("/api/", 1)[0]
    body = json.dumps({"email": os.environ.get("ADMIN_EMAIL", "admin@example.com"),
                       "password": os.environ.get("ADMIN_PASSWORD", "ChangeMe123!")}).encode()
    req = urllib.request.Request(root + "/api/auth/login", method="POST", data=body, headers={"content-type": "application/json"})
    return json.loads(urllib.request.urlopen(req).read())["token"]


KEY = _login()  # JWT from the suite's login endpoint

def call(method, path, body=None, ok=(200, 201)):
    req = urllib.request.Request(BASE + path, method=method, data=json.dumps(body).encode() if body is not None else None)
    req.add_header("content-type", "application/json")
    if KEY:
        req.add_header("authorization", "Bearer " + KEY)
    try:
        with urllib.request.urlopen(req, timeout=600) as r:
            code, data = r.status, json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        code, data = e.code, json.loads(e.read() or b"null")
    assert code in ok, (method, path, code, data)
    return data

W = "weeks=90"
g = call("GET", f"/cognitive/graph?{W}")
assert g["nodes"] == 28 and g["connected"] and g["hyperedges"]["purchase_order"] > 0, g

m = call("GET", f"/cognitive/memory?{W}&limit=100")
found = {(f["node"], f["metric"]) for f in m["findings"]}
assert ("S3", "lead_time_drift") in found, "planted S3 lead-time drift not found"
assert ("S2", "quality_degradation") in found, "planted S2 defect rise not found"

e = call("GET", f"/cognitive/encoder?{W}&top=5")
assert e["critical_nodes"][0]["node"] == "W1" and e["topology"]["expansion_ratio"] > 0

t = call("POST", "/cognitive/train", {"weeks": 90, "steps": 300})
assert t["wiring"]["dale_law_ok"] and t["fit"]["holdout_weeks"] == 12
trusted = {c["channel"] for c in t["channels"] if c["trusted"]}
assert "volume" in trusted, "CIR-GNN must beat last-week and node-mean on held-out demand"

f = call("GET", f"/cognitive/forecast?{W}&kind=product&channel=volume&limit=3")
assert f["forecasts"] and all(x["value"] >= 0 for x in f["forecasts"])

r = call("GET", f"/cognitive/resilience?{W}")
assert r["hypergraph_spectrum"]["components"] == 1 and r["loss_of_critical_supplier"]

# governance: advisory policies never execute
call("PUT", "/cognitive/policies/create_po", {"autonomy": "advisory", "params": {"max_order_value": 50000}})
p = call("POST", "/cognitive/optimize", {"weeks": 90})
assert p["summary"]["purchase_orders_proposed"] > 0 and p["carrier_ranking"][0]["carrier"] != "K2"
po = next(a for a in p["actions"] if a["kind"] == "create_po")
call("POST", f"/cognitive/actions/{po['action_id']}/approve")
call("POST", f"/cognitive/actions/{po['action_id']}/execute", ok=(409,))
call("PUT", "/cognitive/policies/create_po", {"autonomy": "supervised"})
res = call("POST", f"/cognitive/actions/{po['action_id']}/execute")
assert res["status"] == "draft" and res["po_number"].startswith("PO-")
call("POST", f"/cognitive/actions/{po['action_id']}/execute", ok=(409,))      # no double execution
call("PUT", "/cognitive/policies/create_po", {"autonomy": "advisory"})

# guardrail: orders above the limit are blocked, not proposed
call("PUT", "/cognitive/policies/create_po", {"params": {"max_order_value": 1}})
p2 = call("POST", "/cognitive/optimize", {"weeks": 90})
assert all(a["status"] == "blocked" for a in p2["actions"] if a["kind"] == "create_po")
call("PUT", "/cognitive/policies/create_po", {"params": {"max_order_value": 50000}})

for sc in ({"scenario": "supplier_failure", "supplier": "S1"}, {"scenario": "port_congestion", "pct": 30},
           {"scenario": "demand_surge", "factor": 2}, {"scenario": "warehouse_saturation", "level": 0.95},
           {"scenario": "lead_time", "days": 40}):
    s = call("POST", "/cognitive/twin/simulate", sc)
    assert s["learned_propagation"]["most_affected_nodes"], sc
call("POST", "/cognitive/twin/simulate", {"scenario": "nope"}, ok=(400,))

l = call("POST", f"/cognitive/learning/step?{W}", {})
assert "forecast_error" in l["metrics"] and l["policy_update"]["applied"] is False
print("ALL V5 SMOKE TESTS PASSED")
