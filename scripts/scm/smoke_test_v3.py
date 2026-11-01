#!/usr/bin/env python3
"""End-to-end test of the closed loop: sourcing -> forecast -> replenishment -> contract check -> PO ->
inspection at receipt -> quarantine/rework -> pick/pack/stage -> resilience. Usage as smoke_test.py."""
import datetime as dt, json, os, sys, urllib.request, urllib.error, uuid

BASE = os.environ.get("BASE", "http://localhost:8080/api/scm")


def _login():
    root = BASE.rsplit("/api/", 1)[0]
    body = json.dumps({"email": os.environ.get("ADMIN_EMAIL", "admin@example.com"),
                       "password": os.environ.get("ADMIN_PASSWORD", "ChangeMe123!")}).encode()
    req = urllib.request.Request(root + "/api/auth/login", method="POST", data=body, headers={"content-type": "application/json"})
    return json.loads(urllib.request.urlopen(req).read())["token"]


KEY = _login()  # JWT from the suite's login endpoint

def call(method, path, body=None, expect=None):
    req = urllib.request.Request(BASE + path, method=method, data=json.dumps(body).encode() if body is not None else None)
    req.add_header("content-type", "application/json")
    if KEY: req.add_header("authorization", "Bearer " + KEY)
    try:
        with urllib.request.urlopen(req, timeout=300) as r: status, data = r.status, json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e: status, data = e.code, json.loads(e.read() or b"null")
    if expect is not None and status != expect: sys.exit(f"FAIL {method} {path}: expected {expect}, got {status}: {data}")
    return data

today = dt.date.today(); d = lambda n: (today + dt.timedelta(days=n)).isoformat()
tag = uuid.uuid4().hex[:6].upper()
# the Monday after this week: issues made today then sit in the last complete week of history
AS_OF = (today - dt.timedelta(days=today.weekday()) + dt.timedelta(days=7)).isoformat()

wh = call("POST", "/warehouses", {"code": f"L3-{tag}", "name": "Loop DC"}, 201)
bin_ = call("POST", f"/warehouses/{wh['id']}/locations", {"code": "A1"}, 201)
s1 = call("POST", "/suppliers", {"code": f"A-{tag}", "name": "Alpha", "payment_terms_days": 30}, 201)
s2 = call("POST", "/suppliers", {"code": f"B-{tag}", "name": "Beta"}, 201)
cus = call("POST", "/customers", {"code": f"C-{tag}", "name": "Cust"}, 201)
pa = call("POST", "/products", {"sku": f"PA-{tag}", "name": "Dual-sourced", "unit_cost": "4", "list_price": "10", "lead_time_days": 14, "reorder_qty": 20}, 201)
pb = call("POST", "/products", {"sku": f"PB-{tag}", "name": "Sole-sourced", "unit_cost": "6", "list_price": "15", "lead_time_days": 21}, 201)

# --- approved sources
src = lambda pid, sid, **kw: call("POST", f"/products/{pid}/suppliers", {"supplier_id": sid, **kw}, 201)
src(pa["id"], s1["id"], is_preferred=True, lead_time_days=10, unit_cost="3.80")
src(pa["id"], s2["id"], lead_time_days=20)
src(pb["id"], s1["id"], lead_time_days=21)
assert len(call("GET", f"/products/{pa['id']}/suppliers", expect=200)) == 2
call("POST", f"/products/{pa['id']}/suppliers", {"supplier_id": s1["id"], "allocation_share": 1.5}, 400)

# --- stock + demand (ship 70 of A today -> issue ledger)
call("POST", "/inventory/adjust", {"product_id": pa["id"], "location_id": bin_["id"], "delta": 100}, 200)
so = call("POST", "/sales-orders", {"customer_id": cus["id"], "warehouse_id": wh["id"], "lines": [{"product_id": pa["id"], "quantity": 70}]}, 201)
call("POST", f"/sales-orders/{so['order']['id']}/confirm", expect=200)
oid = so["order"]["id"]
# pick -> pack -> stage must run in order
call("POST", f"/sales-orders/{oid}/fulfil", {"stage": "packed"}, 409)
call("POST", f"/sales-orders/{oid}/ship", {"require_staging": True}, 409)
for st in ("picked", "packed", "staged"):
    assert call("POST", f"/sales-orders/{oid}/fulfil", {"stage": st}, 200)["order"]["fulfillment_stage"] == st
call("POST", f"/sales-orders/{oid}/fulfil", {"stage": "staged"}, 409)
call("POST", f"/sales-orders/{oid}/ship", {"require_staging": True}, 201)
assert call("GET", f"/sales-orders/{oid}", expect=200)["order"]["fulfillment_stage"] == "none"

# --- forecast engine
fc = [x for x in call("GET", f"/forecast/demand?warehouse_id={wh['id']}&as_of={AS_OF}&product_id={pa['id']}", expect=200)]
assert len(fc) == 1 and fc[0]["forecast_per_week"] == 70.0 and fc[0]["method"] == "ma13", fc
call("GET", f"/forecast/demand?method=nonsense", expect=400)
call("GET", f"/forecast/demand?method=sba&warehouse_id={wh['id']}&as_of={AS_OF}", expect=200)

# --- replenishment run: no source for B (unsupplied history), contract validation for A
dry = call("POST", "/replenishment/run", {"warehouse_id": wh["id"], "as_of": AS_OF, "dry_run": True}, 201)
mine = lambda lst: [x for x in lst if x.get("sku", "").endswith(tag)]
assert dry["dry_run"] and not any(o["po_id"] for o in dry["orders"])
blocked = mine(dry["skipped"]); assert blocked and blocked[0]["reason"] == "no_active_contract", dry["skipped"]
# a contract in force unlocks the order
ct = call("POST", "/contracts", {"supplier_id": s1["id"], "contract_number": f"K-{tag}", "start_date": d(-30), "end_date": d(200), "currency": "usd"}, 201)
run = call("POST", "/replenishment/run", {"warehouse_id": wh["id"], "as_of": AS_OF}, 201)
orders = [o for o in run["orders"] if o["supplier"] == s1["code"]]
assert len(orders) == 1, run
po = orders[0]; line = [l for l in po["lines"] if l["sku"] == pa["sku"]][0]
assert po["status"] == "draft" and po["contract"] == f"K-{tag}" and line["source"] == "preferred source", po
assert float(line["unit_cost"]) == 3.8, po
assert line["lead_days"] == 10 and line["quantity"] >= 20
detail = call("GET", f"/purchase-orders/{po['po_id']}", expect=200)
assert detail["order"]["auto_generated"] and detail["order"]["po_type"] == "contract" and detail["order"]["contract_id"] == ct["id"]
# idempotent: product now sits on a draft order
again = call("POST", "/replenishment/run", {"warehouse_id": wh["id"], "as_of": AS_OF}, 201)
assert any(x["reason"] == "draft_po_exists" for x in mine(again["skipped"])), again["skipped"]
call("POST", f"/purchase-orders/{po['po_id']}/submit", expect=200)

# --- inspection at receipt: 8 of 30 units fail -> quarantined + incident; rework returns 3, scrap 5
pl = detail["lines"][0]
rcv = call("POST", f"/purchase-orders/{po['po_id']}/receive", {"lines": [{"line_id": pl["id"], "quantity": 30, "location_id": bin_["id"],
           "lot_number": "L1", "rejected_qty": 8, "defect_type": "Cracked housing", "severity": "critical"}]}, 200)
stock = {x["lot_number"]: x for x in call("GET", f"/inventory/stock?product_id={pa['id']}", expect=200)}
assert stock["L1"]["quantity"] == 30 and stock["L1"]["blocked"] == 8 and stock["L1"]["available"] == 22, stock["L1"]
inc = [i for i in call("GET", f"/quality-incidents?supplier_id={s1['id']}", expect=200)][0]
assert inc["severity"] == "critical" and inc["quarantined_qty"] == 8 and inc["purchase_order_id"] == po["po_id"]
call("POST", f"/quality-incidents/{inc['id']}/resolve", {"rework_qty": 9}, 409)
call("POST", f"/quality-incidents/{inc['id']}/resolve", {"rework_qty": 3, "scrap_qty": 5}, 200)
inc = call("GET", f"/quality-incidents/{inc['id']}", expect=200)
assert inc["status"] == "closed" and inc["reworked_qty"] == 3 and inc["scrapped_qty"] == 5
stock = {x["lot_number"]: x for x in call("GET", f"/inventory/stock?product_id={pa['id']}", expect=200)}
assert stock["L1"]["quantity"] == 25 and stock["L1"]["blocked"] == 0

# --- resilience on this little network
conc = call("GET", f"/resilience/concentration?as_of={AS_OF}", expect=200)
prods = {p["sku"]: p for p in conc["products"] if p["sku"].endswith(tag)}
assert prods[pa["sku"]]["sole_source"] is False and prods[pb["sku"]]["sole_source"] is True, prods
assert prods[pa["sku"]]["qualified_suppliers"] == 2
sp = call("GET", f"/resilience/spectral?as_of={AS_OF}", expect=200)
assert sp["spectrum"]["suppliers"] >= 2 and sp["clusters"] and "null_model" in sp
dis = call("POST", "/resilience/disruption", {"supplier_codes": [s1["code"]], "as_of": AS_OF, "top": 200}, 200)
imp = {p["sku"]: p["impact"] for p in dis["products"] if p["sku"].endswith(tag)}
assert imp[pb["sku"]]["sole_source"] is True and imp[pb["sku"]]["uncovered_share"] == 1.0
assert imp[pa["sku"]]["sole_source"] is False and imp[pa["sku"]]["covered_share"] > 0
call("POST", "/resilience/disruption", {"supplier_codes": ["NOPE"]}, 404)
call("POST", "/resilience/disruption", {"supplier_codes": []}, 400)

# --- the one-call planning cycle
cyc = call("POST", "/planning/cycle", {"warehouse_id": wh["id"], "as_of": AS_OF, "create_pos": False}, 200)
assert cyc["forecast"]["products"] >= 1 and cyc["replenishment"]["dry_run"] is True
assert cyc["resilience"]["fragmentation_risk"] in ("low", "medium", "high")
fa = call("GET", f"/reports/forecast-accuracy?warehouse_id={wh['id']}", expect=200)  # statistical rows are in the future: none completed
print("ALL V3 SMOKE TESTS PASSED")
