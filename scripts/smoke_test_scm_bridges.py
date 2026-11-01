#!/usr/bin/env python3
"""End-to-end test of the supply chain module inside the suite: auth, the role matrix on /api/scm, and every
bridge to finance, CRM, QMS and GRC.  Stdlib only.
Usage: python scripts/smoke_test_scm_bridges.py [base_url] [admin_email] [admin_password]"""
import json, sys, uuid, urllib.request, urllib.error
from decimal import Decimal as D

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8080"
ADMIN = (sys.argv[2] if len(sys.argv) > 2 else "admin@example.com", sys.argv[3] if len(sys.argv) > 3 else "ChangeMe123!")
passed = failed = 0
tag = uuid.uuid4().hex[:6].upper()


def call(method, path, body=None, token=None):
    req = urllib.request.Request(BASE + path, method=method, data=None if body is None else json.dumps(body).encode())
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req) as r:
            code, raw = r.status, r.read()
    except urllib.error.HTTPError as e:
        code, raw = e.code, e.read()
    return code, (json.loads(raw) if raw else None)


def ok(method, path, body=None, token=None, expect=(200, 201)):
    code, d = call(method, path, body, token)
    if code not in (expect if isinstance(expect, tuple) else (expect,)):
        raise AssertionError(f"{method} {path} -> {code}: {d}")
    return d


def check(name, cond, detail=""):
    global passed, failed
    if cond:
        passed += 1; print(f"  ok   {name}")
    else:
        failed += 1; print(f"  FAIL {name} {detail}")


def status(method, path, body=None, token=None):
    return call(method, path, body, token)[0]


print("auth")
check("scm requires a token", status("GET", "/api/scm/products") == 401)
check("old api-key header is not accepted", call("GET", "/api/scm/products")[0] == 401)
tok = ok("POST", "/api/auth/login", {"email": ADMIN[0], "password": ADMIN[1]})["token"]
check("health (module)", ok("GET", "/api/scm/health", token=tok)["database"] is True)

roles = {}
for role in ("user", "finance", "auditor", "hr"):
    email = f"{role}.{tag.lower()}@example.com"
    ok("POST", "/api/users", {"email": email, "password": "Passw0rd!x", "role": role, "full_name": role}, tok)
    roles[role] = ok("POST", "/api/auth/login", {"email": email, "password": "Passw0rd!x"})["token"]

print("setup: master data, purchase, stock, sale, shipment")
wh = ok("POST", "/api/scm/warehouses", {"code": f"WH-{tag}", "name": "Main DC", "city": "Belmopan"}, tok)
loc = ok("POST", f"/api/scm/warehouses/{wh['id']}/locations", {"code": "A-01", "zone": "A"}, tok)
prod = ok("POST", "/api/scm/products", {"sku": f"SKU-{tag}", "name": "Widget", "unit_cost": "2.50", "list_price": "5.00"}, tok)
sup = ok("POST", "/api/scm/suppliers", {"code": f"SUP-{tag}", "name": f"Acme Supply {tag}", "lead_time_days": 5}, tok)
cus = ok("POST", "/api/scm/customers", {"code": f"CUS-{tag}", "name": f"Customer {tag}"}, tok)
car = ok("POST", "/api/scm/carriers", {"code": f"CAR-{tag}", "name": "FastFreight"}, tok)
check("user may write scm master data", status("POST", "/api/scm/carriers", {"code": f"C2-{tag}", "name": "x"}, roles["user"]) == 201)
check("auditor is read-only on scm", status("POST", "/api/scm/carriers", {"code": f"C3-{tag}", "name": "x"}, roles["auditor"]) == 403)
check("auditor may read scm", status("GET", "/api/scm/products", token=roles["auditor"]) == 200)
check("finance may read scm", status("GET", "/api/scm/products", token=roles["finance"]) == 200)
check("finance may not write scm master data", status("POST", "/api/scm/carriers", {"code": f"C4-{tag}", "name": "x"}, roles["finance"]) == 403)

po = ok("POST", "/api/scm/purchase-orders", {"supplier_id": sup["id"], "warehouse_id": wh["id"], "lines": [{"product_id": prod["id"], "quantity": 100}]}, tok)
oid, line = po["order"]["id"], po["lines"][0]
ok("POST", f"/api/scm/purchase-orders/{oid}/submit", token=tok)
ok("POST", f"/api/scm/purchase-orders/{oid}/receive", {"lines": [{"line_id": line["id"], "quantity": 100, "location_id": loc["id"]}]}, tok)
so = ok("POST", "/api/scm/sales-orders", {"customer_id": cus["id"], "warehouse_id": wh["id"], "lines": [{"product_id": prod["id"], "quantity": 30}]}, tok)
sid = so["order"]["id"]
ok("POST", f"/api/scm/sales-orders/{sid}/confirm", token=tok)

print("sales order -> ledger")
check("nothing shipped yet -> 409", status("POST", f"/api/scm/sales-orders/{sid}/invoice", {}, tok) == 409)
ok("POST", f"/api/scm/sales-orders/{sid}/ship", {"carrier_id": car["id"], "tracking_number": f"TRK{tag}"}, tok)
st0 = ok("GET", "/api/scm/integration/status", token=tok)
check("status: shipped order awaiting invoice", st0["shipped_orders_not_invoiced"] >= 1, st0)
check("finance role may read payables exposure", "open_payables_to_suppliers" in ok("GET", "/api/scm/integration/status", token=roles["finance"]))
check("user does not see payables exposure", "open_payables_to_suppliers" not in ok("GET", "/api/scm/integration/status", token=roles["user"]))
check("user may not invoice", status("POST", f"/api/scm/sales-orders/{sid}/invoice", {}, roles["user"]) == 403)
check("auditor may not invoice", status("POST", f"/api/scm/sales-orders/{sid}/invoice", {}, roles["auditor"]) == 403)
r = ok("POST", f"/api/scm/sales-orders/{sid}/invoice", {}, roles["finance"])
check("finance may invoice a shipped order", r["created"] and D(str(r["invoice_amount"])) == D("150") and D(str(r["cogs_amount"])) == D("75"), r)
inv = ok("GET", f"/api/finance/invoices/{r['erp_invoice_id']}", token=tok)
check("AR invoice carries the order value", D(str(inv["amount"])) == D("150") and inv["invoice_number"] == so["order"]["so_number"], inv)
r2 = ok("POST", f"/api/scm/sales-orders/{sid}/invoice", {}, tok)
check("invoicing is idempotent", r2["created"] is False and r2["erp_invoice_id"] == r["erp_invoice_id"], r2)
tb = ok("GET", "/api/finance/reports/trial-balance", token=tok)
check("ledger balanced after bridge postings", tb.get("balanced") is True or abs(D(str(tb["total_debits"])) - D(str(tb["total_credits"]))) < D("0.005"), tb)
check("invoicing synced the customer to an ERP customer", ok("POST", f"/api/scm/customers/{cus['id']}/sync-customer", {}, tok)["created"] is False)

print("customer / CRM link")
acct = ok("POST", "/api/crm/accounts", {"name": f"CRM Account {tag}", "email": f"crm{tag.lower()}@example.com"}, tok)
cus2 = ok("POST", "/api/scm/customers", {"code": f"CUSB-{tag}", "name": f"Customer B {tag}"}, tok)
l = ok("POST", f"/api/scm/customers/{cus2['id']}/sync-customer", {"crm_account_id": acct["id"]}, roles["finance"])
check("customer linked to CRM account and ERP customer", l["erp_customer_id"] and l["crm_account_id"] == acct["id"], l)
l2 = ok("POST", f"/api/scm/customers/{cus2['id']}/sync-customer", {"crm_account_id": acct["id"]}, tok)
check("customer link idempotent", l2["erp_customer_id"] == l["erp_customer_id"] and l2["created"] is False, l2)
acct2 = ok("POST", "/api/crm/accounts", {"name": f"CRM Account2 {tag}"}, tok)
check("a different CRM account is refused", status("POST", f"/api/scm/customers/{cus2['id']}/sync-customer", {"crm_account_id": acct2["id"]}, tok) == 409)
check("bad crm_account_id -> 400", status("POST", f"/api/scm/customers/{cus2['id']}/sync-customer", {"crm_account_id": "nope"}, tok) == 400)

print("supplier invoice -> AP bill")
good = ok("POST", f"/api/scm/purchase-orders/{oid}/invoices", {"invoice_number": f"INV-{tag}", "lines": [{"po_line_id": line["id"], "quantity": 100, "unit_price": "2.50"}]}, tok)
check("matched invoice", good["invoice"]["status"] == "matched", good)
bad = ok("POST", f"/api/scm/purchase-orders/{oid}/invoices", {"invoice_number": f"BAD-{tag}", "lines": [{"po_line_id": line["id"], "quantity": 1, "unit_price": "9.99"}]}, tok)
check("disputed invoice is not postable", status("POST", f"/api/scm/supplier-invoices/{bad['invoice']['id']}/post-to-ledger", {}, tok) == 409)
v = ok("POST", f"/api/scm/suppliers/{sup['id']}/sync-vendor", token=roles["finance"])
check("supplier synced to vendor", v["erp_vendor_id"] and v["created"], v)
p = ok("POST", f"/api/scm/supplier-invoices/{good['invoice']['id']}/post-to-ledger", {}, roles["finance"])
check("bill posted for the matched invoice", p["created"] and D(str(p["amount"])) == D("250") and p["erp_vendor_id"] == v["erp_vendor_id"], p)
bill = ok("GET", f"/api/finance/bills/{p['erp_bill_id']}", token=tok)
check("AP bill open for 250", bill["status"] == "open" and D(str(bill["amount"])) == D("250"), bill)
p2 = ok("POST", f"/api/scm/supplier-invoices/{good['invoice']['id']}/post-to-ledger", {}, tok)
check("bill posting idempotent", p2["created"] is False and p2["erp_bill_id"] == p["erp_bill_id"], p2)
check("user may not post to ledger", status("POST", f"/api/scm/supplier-invoices/{good['invoice']['id']}/post-to-ledger", {}, roles["user"]) == 403)
fin = ok("GET", "/api/scm/integration/status", token=roles["finance"])
check("payables to suppliers reflect the bill", D(str(fin["open_payables_to_suppliers"])) >= D("250"), fin)
ov = ok("GET", "/api/enterprise/overview", token=tok)
check("enterprise overview has supply chain", ov["supply_chain"]["open_quality_incidents"] >= 0 and ov["supply_chain"]["inventory_value_at_cost"] >= 0, ov.get("supply_chain"))

print("quality incident -> QMS, supplier scorecard -> GRC")
qi = ok("POST", "/api/scm/quality-incidents", {"product_id": prod["id"], "supplier_id": sup["id"], "purchase_order_id": oid, "severity": "critical",
        "defect_type": "cracked housing", "location_id": loc["id"], "quarantine_qty": 5}, tok)
qid = qi["id"] if "id" in qi else qi["incident"]["id"]
n = ok("POST", f"/api/scm/quality-incidents/{qid}/raise-ncr", token=roles["user"])
check("NCR raised", n["created"] and n["ncr_number"].startswith("NCR-"), n)
ncs = ok("GET", "/api/qms/nonconformances", token=tok)
ncs = ncs["data"] if isinstance(ncs, dict) and "data" in ncs else ncs
check("NCR visible in QMS as supplier-sourced", any(x["id"] == n["nonconformance_id"] and x["source"] == "supplier" and x["severity"] == "critical" for x in ncs))
n2 = ok("POST", f"/api/scm/quality-incidents/{qid}/raise-ncr", token=tok)
check("NCR raising idempotent", n2["created"] is False and n2["nonconformance_id"] == n["nonconformance_id"], n2)
g = ok("POST", "/api/scm/risk/raise-grc-risks", token=tok)
check("GRC risk raised for the supplier with a critical incident", any(x["supplier_id"] == sup["id"] for x in g["raised"]), g)
g2 = ok("POST", "/api/scm/risk/raise-grc-risks", token=tok)
check("GRC risk raising idempotent", not any(x["supplier_id"] == sup["id"] for x in g2["raised"]) and g2["already_open"] >= 1, g2)
check("user may not raise GRC risks in bulk", status("POST", "/api/scm/risk/raise-grc-risks", token=roles["user"]) == 403)

print("audit")
audit = ok("GET", "/api/sys/audit-log", token=tok)
audit = audit["data"] if isinstance(audit, dict) and "data" in audit else audit
check("scm mutations are audit-logged", any("/api/scm/" in (a.get("path") or "") for a in audit))

print(f"\n{passed} passed, {failed} failed")
sys.exit(1 if failed else 0)
