#!/usr/bin/env python3
"""End-to-end smoke test against a running server.
Usage: BASE=http://localhost:8080/api/scm python3 scripts/smoke_test.py
"""
import json, os, sys, urllib.request, urllib.error, uuid

BASE = os.environ.get("BASE", "http://localhost:8080/api/scm")


def _login():
    root = BASE.rsplit("/api/", 1)[0]
    body = json.dumps({"email": os.environ.get("ADMIN_EMAIL", "admin@example.com"),
                       "password": os.environ.get("ADMIN_PASSWORD", "ChangeMe123!")}).encode()
    req = urllib.request.Request(root + "/api/auth/login", method="POST", data=body, headers={"content-type": "application/json"})
    return json.loads(urllib.request.urlopen(req).read())["token"]


KEY = _login()  # JWT from the suite's login endpoint

def call(method, path, body=None, expect=None):
    req = urllib.request.Request(BASE + path, method=method,
                                 data=json.dumps(body).encode() if body is not None else None)
    req.add_header("content-type", "application/json")
    if KEY:
        req.add_header("authorization", "Bearer " + KEY)
    try:
        with urllib.request.urlopen(req) as r:
            status, data = r.status, json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        status, data = e.code, json.loads(e.read() or b"null")
    if expect is not None and status != expect:
        sys.exit(f"FAIL {method} {path}: expected {expect}, got {status}: {data}")
    return data

def stock(product_id):
    rows = call("GET", f"/inventory/stock?product_id={product_id}", expect=200)
    return sum(r["quantity"] for r in rows), sum(r["reserved"] for r in rows)

tag = uuid.uuid4().hex[:6].upper()
wh = call("POST", "/warehouses", {"code": f"WH-{tag}", "name": "Main DC", "city": "Belize City"}, 201)
bin_a = call("POST", f"/warehouses/{wh['id']}/locations", {"code": "A-01-01", "zone": "A"}, 201)
bin_b = call("POST", f"/warehouses/{wh['id']}/locations", {"code": "B-01-01", "zone": "B"}, 201)
prod = call("POST", "/products", {"sku": f"SKU-{tag}", "name": "Widget", "unit_cost": "2.50",
                                  "list_price": "5.00", "reorder_point": 20, "reorder_qty": 100}, 201)
sup = call("POST", "/suppliers", {"code": f"SUP-{tag}", "name": "Acme Supply", "lead_time_days": 5}, 201)
cus = call("POST", "/customers", {"code": f"CUS-{tag}", "name": "Customer One"}, 201)
car = call("POST", "/carriers", {"code": f"CAR-{tag}", "name": "FastFreight"}, 201)

# Low-stock report flags the product before any stock exists
low = call("GET", "/reports/low-stock", expect=200)
assert any(r["product_id"] == prod["id"] for r in low), "expected product in low-stock report"

# Purchase order: create -> submit -> partial receive -> full receive
po = call("POST", "/purchase-orders", {"supplier_id": sup["id"], "warehouse_id": wh["id"],
          "lines": [{"product_id": prod["id"], "quantity": 100}]}, 201)
line = po["lines"][0]
call("POST", f"/purchase-orders/{po['order']['id']}/receive",
     {"lines": [{"line_id": line["id"], "quantity": 10, "location_id": bin_a["id"]}]}, 409)  # not submitted
call("POST", f"/purchase-orders/{po['order']['id']}/submit", expect=200)
low = call("GET", "/reports/low-stock", expect=200)
assert not any(r["product_id"] == prod["id"] for r in low), "open PO should cover the reorder point"
r1 = call("POST", f"/purchase-orders/{po['order']['id']}/receive",
          {"lines": [{"line_id": line["id"], "quantity": 60, "location_id": bin_a["id"], "lot_number": "L1"}]}, 200)
assert r1["order"]["status"] == "partially_received"
call("POST", f"/purchase-orders/{po['order']['id']}/receive",
     {"lines": [{"line_id": line["id"], "quantity": 41, "location_id": bin_a["id"]}]}, 409)  # over-receive
r2 = call("POST", f"/purchase-orders/{po['order']['id']}/receive",
          {"lines": [{"line_id": line["id"], "quantity": 40, "location_id": bin_b["id"]}]}, 200)
assert r2["order"]["status"] == "received"
assert stock(prod["id"]) == (100, 0)

# Transfer + adjustment
call("POST", "/inventory/transfer", {"product_id": prod["id"], "from_location_id": bin_b["id"],
     "to_location_id": bin_a["id"], "quantity": 15}, 200)
call("POST", "/inventory/transfer", {"product_id": prod["id"], "from_location_id": bin_b["id"],
     "to_location_id": bin_a["id"], "quantity": 999}, 409)
call("POST", "/inventory/adjust", {"product_id": prod["id"], "location_id": bin_a["id"],
     "delta": -5, "reason": "damaged"}, 200)
call("POST", "/inventory/adjust", {"product_id": prod["id"], "location_id": bin_a["id"],
     "delta": -500}, 409)
assert stock(prod["id"]) == (95, 0)
moves = call("GET", f"/inventory/movements?product_id={prod['id']}", expect=200)
assert {m["movement_type"] for m in moves} >= {"receipt", "transfer_in", "transfer_out", "adjustment"}

# Sales order 1: 30 units, fully allocated, shipped
so1 = call("POST", "/sales-orders", {"customer_id": cus["id"], "warehouse_id": wh["id"],
           "lines": [{"product_id": prod["id"], "quantity": 30}]}, 201)
assert so1["lines"][0]["unit_price"] == "5.0000" or so1["lines"][0]["unit_price"].startswith("5")
c1 = call("POST", f"/sales-orders/{so1['order']['id']}/confirm", expect=200)
assert c1["order"]["status"] == "confirmed" and c1["lines"][0]["quantity_allocated"] == 30
assert stock(prod["id"]) == (95, 30)
sh1 = call("POST", f"/sales-orders/{so1['order']['id']}/ship",
           {"carrier_id": car["id"], "tracking_number": f"TRK{tag}"}, 201)
assert sh1["items"][0]["quantity"] == 30 and sh1["shipment"]["status"] == "in_transit"
assert stock(prod["id"]) == (65, 0)
assert call("GET", f"/sales-orders/{so1['order']['id']}", expect=200)["order"]["status"] == "shipped"
call("POST", f"/sales-orders/{so1['order']['id']}/cancel", expect=409)
call("POST", f"/shipments/{sh1['shipment']['id']}/events", {"status": "bogus"}, 400)
call("POST", f"/shipments/{sh1['shipment']['id']}/events",
     {"status": "out_for_delivery", "location": "Belize City"}, 200)
d = call("POST", f"/shipments/{sh1['shipment']['id']}/events", {"status": "delivered"}, 200)
assert d["shipment"]["delivered_at"] and len(d["events"]) == 3
call("POST", f"/shipments/{sh1['shipment']['id']}/events", {"status": "exception"}, 409)
assert call("GET", f"/tracking/TRK{tag}", expect=200)["shipment"]["status"] == "delivered"

# Sales order 2: wants 100 but only 65 available -> partially allocated, then cancel releases stock
so2 = call("POST", "/sales-orders", {"customer_id": cus["id"], "warehouse_id": wh["id"],
           "lines": [{"product_id": prod["id"], "quantity": 100}]}, 201)
c2 = call("POST", f"/sales-orders/{so2['order']['id']}/confirm", expect=200)
assert c2["order"]["status"] == "partially_allocated" and c2["lines"][0]["quantity_allocated"] == 65
assert stock(prod["id"]) == (65, 65)
call("POST", "/inventory/adjust", {"product_id": prod["id"], "location_id": bin_a["id"], "delta": -1}, 409)  # reserved
# Restock arrives; retry allocation fills the rest
call("POST", "/inventory/adjust", {"product_id": prod["id"], "location_id": bin_b["id"], "delta": 50}, 200)
c3 = call("POST", f"/sales-orders/{so2['order']['id']}/allocate", expect=200)
assert c3["order"]["status"] == "confirmed" and c3["lines"][0]["quantity_allocated"] == 100
c4 = call("POST", f"/sales-orders/{so2['order']['id']}/cancel", expect=200)
assert c4["order"]["status"] == "cancelled"
assert stock(prod["id"]) == (115, 0)

# Reports
val = call("GET", "/reports/valuation", expect=200)
mine = [v for v in val if v["warehouse_id"] == wh["id"]][0]
assert mine["units"] == 115 and float(mine["value"]) == 115 * 2.5, mine
call("GET", "/products?q=" + prod["sku"], expect=200)
print("ALL SMOKE TESTS PASSED")
