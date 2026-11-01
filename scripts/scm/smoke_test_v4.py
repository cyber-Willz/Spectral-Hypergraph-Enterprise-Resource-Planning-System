#!/usr/bin/env python3
"""End-to-end test of the v0.4 dataset-driven features: MOQ, excess cover, lead-time accuracy, early-delivery
metrics, landed cost, AQL inspection, production, shipment risk and carrier scorecards, utilisation,
reconciliation and KPIs. Usage as smoke_test.py."""
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
AS_OF = (today - dt.timedelta(days=today.weekday()) + dt.timedelta(days=7)).isoformat()

wh = call("POST", "/warehouses", {"code": f"V4-{tag}", "name": "V4 DC"}, 201)
bin_ = call("POST", f"/warehouses/{wh['id']}/locations", {"code": "A1", "capacity": 1000}, 201)
sup = call("POST", "/suppliers", {"code": f"S4-{tag}", "name": "Slowpoke", "lead_time_days": 5}, 201)
cus = call("POST", "/customers", {"code": f"C4-{tag}", "name": "Cust"}, 201)
car = call("POST", "/carriers", {"code": f"K4-{tag}", "name": "FastFreight"}, 201)
mk = lambda sku, **kw: call("POST", "/products", {"sku": f"{sku}-{tag}", "name": sku, "unit_cost": "10", "list_price": "20", "lead_time_days": 7, **kw}, 201)

# --- validation of new enums
call("POST", "/purchase-orders", {"supplier_id": sup["id"], "warehouse_id": wh["id"], "incoterm": "XYZ", "lines": [{"product_id": mk("BAD1")["id"], "quantity": 1}]}, 400)
call("POST", "/purchase-orders", {"supplier_id": sup["id"], "warehouse_id": wh["id"], "transport_mode": "teleport", "lines": [{"product_id": mk("BAD2")["id"], "quantity": 1}]}, 400)
call("POST", "/products", {"sku": f"BAD3-{tag}", "name": "x", "aql_defect_pct": 120}, 400)

# --- MOQ / order multiple + excess cover
moq = mk("MOQ")
call("POST", f"/products/{moq['id']}/suppliers", {"supplier_id": sup["id"], "is_preferred": True, "lead_time_days": 5, "min_order_qty": 400, "order_multiple": 100}, 201)
call("POST", f"/products/{moq['id']}/suppliers", {"supplier_id": sup["id"], "min_order_qty": 0}, 400)
call("POST", "/inventory/adjust", {"product_id": moq["id"], "location_id": bin_["id"], "delta": 500}, 200)
so = call("POST", "/sales-orders", {"customer_id": cus["id"], "warehouse_id": wh["id"], "lines": [{"product_id": moq["id"], "quantity": 7}]}, 201)
call("POST", f"/sales-orders/{so['order']['id']}/confirm", expect=200)
ship = call("POST", f"/sales-orders/{so['order']['id']}/ship", {"carrier_id": car["id"], "tracking_number": f"T{tag}",
            "expected_delivery_date": d(-1), "freight_cost": "40",
            "risk": {"weather": "Storm", "traffic_index": 85, "supplier_reliability": 60, "geopolitical_risk": 7}}, 201)
sh = ship["shipment"]
assert sh["delay_risk"] is not None and sh["delay_risk"] > 0.5 and sh["disruption_risk"] > 0.7, sh
assert sh["risk_context"]["warehouse_utilization"] is not None, sh["risk_context"]  # filled from bin capacity
call("POST", f"/shipments/{sh['id']}/events", {"status": "delivered"}, 200)  # delivered today, promised yesterday -> late
cs = [c for c in call("GET", "/reports/carrier-scorecards", expect=200) if c["code"] == f"K4-{tag}"][0]
assert cs["judged"] == 1 and cs["on_time_rate"] == 0.0 and cs["observed_late_rate"] == 1.0 and cs["avg_days_late_when_late"] == 1.0, cs
assert cs["predicted_late_rate"] == sh["delay_risk"] and cs["avg_freight_cost"] == 40.0, cs

ex = call("GET", f"/reports/excess-cover?as_of={AS_OF}&warehouse_id={wh['id']}", expect=200)
row = [p for p in ex["products"] if p["sku"] == moq["sku"]]
assert row and row[0]["moq_driven"] is True and row[0]["cover_days"] > 90 and row[0]["excess_value"] > 0, ex
assert ex["summary"]["moq_driven_products"] >= 1

# replenishment honours the MOQ and multiple (after a contract exists)
call("POST", "/contracts", {"supplier_id": sup["id"], "contract_number": f"K-{tag}", "start_date": d(-30), "end_date": d(200)}, 201)
call("POST", "/inventory/adjust", {"product_id": moq["id"], "location_id": bin_["id"], "delta": -483}, 200)   # stock down to 10: now below the reorder point
run = call("POST", "/replenishment/run", {"warehouse_id": wh["id"], "as_of": AS_OF, "dry_run": True}, 201)
lines = [l for o in run["orders"] for l in o["lines"] if l["sku"] == moq["sku"]]
assert lines and lines[0]["quantity"] >= 400 and lines[0]["quantity"] % 100 == 0 and lines[0]["moq_overbuy"] == lines[0]["quantity"] - lines[0]["needed_quantity"], run

# --- lead-time accuracy: master says 5 days, receipts say 20
lt = mk("LT")
lines_in = [{"product_id": lt["id"], "quantity": 1} for _ in range(12)]
po = call("POST", "/purchase-orders", {"supplier_id": sup["id"], "warehouse_id": wh["id"], "order_date": d(-40), "expected_date": d(-35),
          "transport_mode": "sea", "incoterm": "cif", "freight_cost": "100", "insurance_cost": "20", "lines": lines_in}, 201)
assert po["order"]["incoterm"] == "CIF" and po["order"]["transport_mode"] == "sea"
call("POST", f"/purchase-orders/{po['order']['id']}/submit", expect=200)
call("POST", f"/purchase-orders/{po['order']['id']}/receive", {"received_on": d(-20),
     "lines": [{"line_id": l["id"], "quantity": 1, "location_id": bin_["id"]} for l in po["lines"]]}, 200)
acc = call("GET", "/reports/lead-time-accuracy", expect=200)
me = [x for x in acc["suppliers"] if x["code"] == f"S4-{tag}"][0]
assert me["receipts"] >= 12 and me["master_data"] == "understated" and me["bias_days"] > 5, me
assert any(m["mode"] == "sea" for m in acc["by_transport_mode"])

# --- early-delivery metrics: promised d(-35), received d(-20)? that is LATE; build an early one
early = call("POST", "/purchase-orders", {"supplier_id": sup["id"], "warehouse_id": wh["id"], "order_date": d(-30), "expected_date": d(-10),
             "lines": [{"product_id": lt["id"], "quantity": 5}]}, 201)
call("POST", f"/purchase-orders/{early['order']['id']}/submit", expect=200)
call("POST", f"/purchase-orders/{early['order']['id']}/receive", {"received_on": d(-20), "lines": [{"line_id": early["lines"][0]["id"], "quantity": 5, "location_id": bin_["id"]}]}, 200)
sc = call("GET", f"/suppliers/{sup['id']}/scorecard?early_tolerance_days=3", expect=200)
assert sc["early_rate"] is not None and 0 < sc["early_rate"] < 1 and sc["avg_days_early"] == 10.0, sc   # 1 early line out of 14 judged
assert abs(sc["on_window_rate"] - (sc["on_time_rate"] - sc["early_rate"])) < 1e-9
sc_loose = call("GET", f"/suppliers/{sup['id']}/scorecard?early_tolerance_days=30", expect=200)
assert sc_loose["early_rate"] == 0.0

# --- landed cost: freight 100 + insurance 20 spread by value over received lines
lc = call("GET", f"/purchase-orders/{po['order']['id']}/landed-cost", expect=200)
assert lc["basis"] == "received" and float(lc["freight"]) == 100 and len(lc["lines"]) == 12
assert abs(sum(float(l["allocated_freight"]) for l in lc["lines"]) - 100) < 1e-6
assert abs(float(lc["lines"][0]["landed_unit_cost"]) - (float(lc["lines"][0]["unit_cost"]) + 120.0 / 12.0)) < 1e-6, lc["lines"][0]
call("POST", f"/purchase-orders/{po['order']['id']}/freight", {"freight_included": True}, 200)
assert float(call("GET", f"/purchase-orders/{po['order']['id']}/landed-cost", expect=200)["freight"]) == 0

# --- AQL inspection
aql = mk("AQL", aql_defect_pct=2.0)
p2 = call("POST", "/purchase-orders", {"supplier_id": sup["id"], "warehouse_id": wh["id"], "lines": [{"product_id": aql["id"], "quantity": 100}, {"product_id": aql["id"], "quantity": 60}]}, 201)
call("POST", f"/purchase-orders/{p2['order']['id']}/submit", expect=200)
call("POST", f"/purchase-orders/{p2['order']['id']}/receive", {"lines": [
    {"line_id": p2["lines"][0]["id"], "quantity": 100, "location_id": bin_["id"], "lot_number": "BAD", "defect_rate_pct": 3.5},
    {"line_id": p2["lines"][1]["id"], "quantity": 60, "location_id": bin_["id"], "lot_number": "OK", "defect_rate_pct": 1.0}]}, 200)
st = {x["lot_number"]: x for x in call("GET", f"/inventory/stock?product_id={aql['id']}", expect=200)}
assert st["BAD"]["blocked"] == 100 and st["BAD"]["available"] == 0 and st["OK"]["blocked"] == 0 and st["OK"]["available"] == 60, st
inc = [i for i in call("GET", f"/quality-incidents?product_id={aql['id']}", expect=200)]
assert len(inc) == 1 and inc[0]["defect_type"] == "AQL exceeded"
call("POST", "/purchase-orders/" + p2["order"]["id"] + "/receive", {"lines": []}, 400)

# --- production run + yield / cost variance (standard cost 10; actual 1350 / 90 good = 15 -> +50%)
pr = mk("MAKE")
run1 = call("POST", "/production-runs", {"product_id": pr["id"], "location_id": bin_["id"], "quantity_started": 100, "scrap_pct": 10, "hours": 5, "cost": "1350", "produced_on": d(-2)}, 201)
assert run1["quantity_good"] == 90 and run1["quantity_scrap"] == 10
call("POST", "/production-runs", {"product_id": pr["id"], "location_id": bin_["id"], "quantity_started": 10, "scrap_pct": 5, "scrap_qty": 1}, 400)
call("POST", "/production-runs", {"product_id": pr["id"], "location_id": bin_["id"], "quantity_started": 10, "scrap_qty": 11}, 400)
yr = call("GET", f"/reports/production-yield?warehouse_id={wh['id']}", expect=200)
y = [p for p in yr["products"] if p["sku"] == pr["sku"]][0]
assert y["scrap_rate"] == 0.1 and y["units_per_hour"] == 18.0 and abs(y["actual_unit_cost"] - 15.0) < 1e-9 and abs(y["cost_variance"] - 0.5) < 1e-9, y
assert any(m["movement_type"] == "production" and m["quantity_delta"] == 90 for m in call("GET", f"/inventory/movements?product_id={pr['id']}", expect=200))

# --- standalone shipment-risk endpoint
r1 = call("POST", "/risk/shipment", {"weather": "clear", "traffic_index": 20, "supplier_reliability": 95, "carrier_reliability": 95, "warehouse_utilization": 50}, 200)
r2 = call("POST", "/risk/shipment", {"weather": "storm", "traffic_index": 90, "supplier_reliability": 55, "carrier_reliability": 60, "warehouse_utilization": 95}, 200)
assert r1["delay"]["probability"] < 0.05 and r2["delay"]["probability"] > 0.9 and r1["delay"]["inputs_imputed"] == 0
r3 = call("POST", "/risk/shipment", {"weather": "hurricane", "geopolitical_risk": 8}, 200)
assert r3["disruption"]["probability"] > 0.99 and r3["delay"] is None  # too few inputs for a delay estimate
call("POST", "/risk/shipment", {"weather": "meteor"}, 400)
call("POST", "/risk/shipment", {"traffic_index": 150}, 400)

# --- utilisation, reconciliation, KPIs
ut = [w for w in call("GET", "/reports/warehouse-utilization", expect=200)["warehouses"] if w["code"] == f"V4-{tag}"][0]
assert ut["capacity_units"] == 1000 and ut["utilization_pct"] is not None and ut["status"] in ("ok", "tight", "congested"), ut
rec = call("GET", "/reports/stock-reconciliation?limit=20", expect=200)
assert rec["balanced"] is True, rec
kp = call("GET", f"/reports/kpis?as_of={AS_OF}&warehouse_id={wh['id']}", expect=200)
names = {k["kpi"]: k for k in kp["kpis"]}
assert set(names) >= {"inventory_turnover", "order_fill_rate", "supplier_lead_time_mean", "supplier_on_time_rate"}
assert names["order_fill_rate"]["value"] == 100.0 and names["order_fill_rate"]["position"] == "strong", names["order_fill_rate"]
call("GET", "/reports/kpis?window_days=5", expect=400)
print("ALL V4 SMOKE TESTS PASSED")
