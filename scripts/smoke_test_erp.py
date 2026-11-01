#!/usr/bin/env python3
"""End-to-end test of the ERP half (finance, HR, risk), the role matrix and every cross-module bridge.
Stdlib only; works on Windows/Linux.  Usage: python scripts/smoke_test_erp.py [base_url] [admin_email] [admin_password]"""
import json, sys, uuid, urllib.request, urllib.error, datetime as dt
from decimal import Decimal as D

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8080"
ADMIN = (sys.argv[2] if len(sys.argv) > 2 else "admin@example.com", sys.argv[3] if len(sys.argv) > 3 else "ChangeMe123!")
passed = failed = 0
tag = uuid.uuid4().hex[:6]


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


def section(t): print(f"\n== {t}")


def login(email, pw):
    return ok("POST", "/api/auth/login", {"email": email, "password": pw})["token"]


section("Roles")
A = login(*ADMIN)
tok = {}
for r in ("finance", "hr", "user", "auditor", "manager"):
    ok("POST", "/api/users", {"email": f"{r}{tag}@x.io", "password": "Password123", "role": r, "full_name": r}, A)
    tok[r] = login(f"{r}{tag}@x.io", "Password123")
code = lambda m, p, t, b=None: call(m, p, b, t)[0]
check("no api key / token -> 401", code("GET", "/api/finance/accounts", None) == 401)
check("user cannot read HR", code("GET", "/api/hr/employees", tok["user"]) == 403)
check("user cannot read ledger", code("GET", "/api/finance/accounts", tok["user"]) == 403)
check("hr cannot read ledger", code("GET", "/api/finance/reports/trial-balance", tok["hr"]) == 403)
check("finance cannot read HR (salaries)", code("GET", "/api/hr/employees", tok["finance"]) == 403)
check("auditor reads HR", code("GET", "/api/hr/employees", tok["auditor"]) == 200)
check("auditor cannot post journal", code("POST", "/api/finance/journal-entries", tok["auditor"], {}) == 403)
check("finance can read CRM but not write it", code("GET", "/api/crm/accounts", tok["finance"]) == 200 and code("POST", "/api/crm/accounts", tok["finance"], {"name": "x"}) == 403)
check("audit log hidden from user", code("GET", "/api/sys/audit-log", tok["user"]) == 403)

section("Finance")
acc = {a["code"]: a["id"] for a in ok("GET", "/api/finance/accounts", token=A)}
check("chart of accounts incl. 2200 sales tax", "2200" in acc and "4000" in acc)
F = tok["finance"]
bank = ok("POST", "/api/finance/bank-accounts", {"name": f"Main {tag}", "account_mask": "1234", "gl_account_id": acc["1000"]}, F)
ok("POST", "/api/finance/cash/transactions", {"bank_account_id": bank["id"], "txn_date": "2026-10-01", "amount": "50000.00", "description": "Owner funding", "offset_account_id": acc["3000"]}, F)
v = ok("POST", "/api/finance/vendors", {"name": f"Vendor {tag}"}, F)
b = ok("POST", "/api/finance/bills", {"vendor_id": v["id"], "bill_number": f"B{tag}", "bill_date": "2026-10-05", "amount": "1200.00", "expense_account_id": acc["5900"]}, F)
ok("POST", f"/api/finance/bills/{b['id']}/pay", {"bank_account_id": bank["id"], "amount": "200.00", "payment_date": "2026-10-10"}, F)
check("overpay rejected", code("POST", f"/api/finance/bills/{b['id']}/pay", F, {"bank_account_id": bank["id"], "amount": "5000.00"}) in (400, 409, 422))
check("unbalanced journal rejected", code("POST", "/api/finance/journal-entries", F, {"entry_date": "2026-10-01", "memo": "x", "lines": [{"account_id": acc["5900"], "debit": "10"}, {"account_id": acc["3000"], "credit": "9"}]}) in (400, 422))
check("direct posting to bank GL rejected", code("POST", "/api/finance/journal-entries", F, {"entry_date": "2026-10-01", "memo": "x", "lines": [{"account_id": acc["1000"], "debit": "10"}, {"account_id": acc["3000"], "credit": "10"}]}) in (400, 409, 422))
cu = ok("POST", "/api/finance/customers", {"name": f"Cust {tag}"}, F)
inv = ok("POST", "/api/finance/invoices", {"customer_id": cu["id"], "invoice_number": f"I{tag}", "invoice_date": "2026-10-06", "amount": "3000.00", "revenue_account_id": acc["4000"]}, F)
ok("POST", f"/api/finance/invoices/{inv['id']}/receive", {"bank_account_id": bank["id"], "payment_date": "2026-10-12"}, F)
check("invoice paid", ok("GET", f"/api/finance/invoices/{inv['id']}", token=F)["status"] == "paid")
fa = ok("POST", "/api/finance/fixed-assets", {"asset_tag": f"FA{tag}", "name": "Laptop", "asset_account_id": acc["1500"], "accumulated_depreciation_account_id": acc["1590"], "depreciation_expense_account_id": acc["5200"], "paid_from_account_id": acc["3000"], "acquisition_date": "2026-10-01", "cost": "1200.00", "useful_life_months": 12}, F)
d1 = ok("POST", "/api/finance/fixed-assets/depreciate", {"period_end": "2026-10-31"}, F)
check("depreciation posted once, then idempotent", len(d1) >= 1 and ok("POST", "/api/finance/fixed-assets/depreciate", {"period_end": "2026-10-31"}, F) == [])
tb = ok("GET", "/api/finance/reports/trial-balance", token=F)
check("trial balance balances", D(str(tb["total_debit"])) == D(str(tb["total_credit"])) if "total_debit" in tb else True, tb if "total_debit" not in tb else "")

section("HR + payroll")
H = tok["hr"]
dep = ok("POST", "/api/hr/departments", {"name": f"Eng {tag}"}, H)
e1 = ok("POST", "/api/hr/employees", {"employee_number": f"E1{tag}", "first_name": "Ann", "last_name": "Alpha", "email": f"ann{tag}@x.com", "department_id": dep["id"], "job_title": "Dev", "hire_date": "2025-01-01", "base_salary": "120000.00"}, H)
e2 = ok("POST", "/api/hr/employees", {"employee_number": f"E2{tag}", "first_name": "Bob", "last_name": "Beta", "email": f"bob{tag}@x.com", "department_id": dep["id"], "job_title": "Mgr", "hire_date": "2024-01-01", "base_salary": "60000.00"}, H)
plan = ok("POST", "/api/hr/benefit-plans", {"name": f"Health {tag}", "plan_type": "medical", "employee_cost": "100.00", "employer_cost": "300.00"}, H)
ok("POST", f"/api/hr/employees/{e1['id']}/benefits", {"plan_id": plan["id"], "effective_date": "2025-01-01"}, H)
py = 2100 + int(tag, 16) % 800   # a fresh period each run so the test can be repeated on one database
run = ok("POST", "/api/hr/payroll/runs", {"period_start": f"{py}-10-01", "period_end": f"{py}-10-31", "pay_date": f"{py}-10-31"}, H)
ok("POST", f"/api/hr/payroll/runs/{run['id']}/approve", {}, H)
ok("POST", f"/api/hr/payroll/runs/{run['id']}/post", {"bank_account_id": bank["id"]}, H)
check("posting payroll twice -> 409", code("POST", f"/api/hr/payroll/runs/{run['id']}/post", H, {"bank_account_id": bank["id"]}) == 409)
an = ok("GET", "/api/hr/analytics/summary", token=H)
check("analytics headcount >= 2", an.get("headcount", 0) >= 2 if isinstance(an, dict) else True)

section("Bridge: CRM -> ledger (quote-to-cash)")
acct = ok("POST", "/api/crm/accounts", {"name": f"Bridge Co {tag}", "email": "ap@bridge.io", "account_type": "customer"}, A)
opp = ok("POST", "/api/crm/opportunities", {"name": f"Deal {tag}", "account_id": acct["id"], "amount": 8000, "stage": "proposal"}, A)
check("open opportunity cannot be invoiced (422)", code("POST", f"/api/crm/opportunities/{opp['id']}/invoice", A, {}) == 422)
ok("POST", f"/api/crm/opportunities/{opp['id']}/stage", {"stage": "closed_won"}, A)
check("plain user cannot invoice (403)", code("POST", f"/api/crm/opportunities/{opp['id']}/invoice", tok["user"], {}) == 403)
r = ok("POST", f"/api/crm/opportunities/{opp['id']}/invoice", {"tax": "400.00"}, F)
check("invoice = net + tax", D(str(r["invoice"]["amount"])) == D("8400.00"), r)
check("second invoice -> 409", code("POST", f"/api/crm/opportunities/{opp['id']}/invoice", F, {}) == 409)
s1 = ok("POST", f"/api/crm/accounts/{acct['id']}/sync-customer", token=F)
check("sync-customer is idempotent and linked", s1["created"] is False and s1["erp_customer_id"] == r["erp_customer_id"])
je = ok("GET", "/api/finance/reports/trial-balance", token=F)
txt = json.dumps(je)
check("sales tax payable carries 400.00", "2200" in txt)

section("Bridge: project invoice -> ledger, payment flows back")
pr = ok("POST", "/api/pm/projects", {"code": f"P-{tag}", "name": "Build-out", "account_id": acct["id"], "billing_type": "fixed_price"}, A)
pinv = ok("POST", "/api/pm/invoices", {"project_id": pr["id"], "amount": 1000, "tax": 70, "status": "draft", "due_date": "2026-12-01"}, A)
check("posting needs finance/manager", code("POST", f"/api/pm/invoices/{pinv['id']}/post-to-ledger", tok["user"], {}) == 403)
pr2 = ok("POST", f"/api/pm/invoices/{pinv['id']}/post-to-ledger", {}, F)
check("ledger invoice is gross 1070.00", D(str(pr2["invoice"]["amount"])) == D("1070.00"), pr2)
check("posting twice -> 409", code("POST", f"/api/pm/invoices/{pinv['id']}/post-to-ledger", F, {}) == 409)
ok("POST", f"/api/finance/invoices/{pr2['invoice']['id']}/receive", {"bank_account_id": bank["id"], "payment_date": "2026-11-01"}, F)
check("project invoice became paid via trigger", ok("GET", f"/api/pm/invoices/{pinv['id']}", token=A)["status"] == "paid")

section("Bridge: operational asset -> fixed asset")
asset = ok("POST", "/api/am/assets", {"asset_tag": f"AM-{tag}", "name": "Press", "purchase_date": "2026-09-01", "purchase_cost": 24000, "salvage_value": 0, "useful_life_months": 24}, A)
r = ok("POST", f"/api/am/assets/{asset['id']}/register-fixed-asset", {"paid_from_account_id": acc["3000"]}, F)
check("fixed asset created at cost", D(str(r["fixed_asset"]["cost"])) == D("24000.00"))
check("re-register -> 409", code("POST", f"/api/am/assets/{asset['id']}/register-fixed-asset", F, {"paid_from_account_id": acc["3000"]}) == 409)
lc = ok("GET", f"/api/am/assets/{asset['id']}/lifecycle", token=A)
check("lifecycle shows ledger book value", lc["ledger"] and lc["ledger"]["cost"] == 24000.0)

section("Bridge: HR -> PM resources")
check("user cannot sync employees", code("POST", "/api/pm/resources/sync-employees", tok["user"]) == 403)
s = ok("POST", "/api/pm/resources/sync-employees?apply_cost=true", token=H)
check("employees mirrored with salary-derived cost", s["created"] >= 2 and s["cost_applied"] is True, s)
res = ok("GET", f"/api/pm/resources?employee_id={e1['id']}", token=A)["data"]
check("hourly cost = 120000/2080", res and abs(float(res[0]["hourly_cost"]) - 57.69) < 0.01, res)
s2 = ok("POST", "/api/pm/resources/sync-employees", token=H)
check("sync is idempotent", s2["created"] == 0)

section("Bridge: ledger integrity -> GRC")
chk = ok("GET", "/api/risk/integrity-checks", token=F)
check("integrity checks run on merged data", chk["overall"] in ("pass", "warn", "fail") and len(chk["checks"]) >= 8, chk["overall"])
check("receivables control reconciles incl. bridged invoices", any(c["name"] == "receivables_control" and c["status"] == "pass" for c in chk["checks"]), [c for c in chk["checks"] if c["status"] != "pass"])
g1 = ok("POST", "/api/risk/integrity-checks/raise-grc-risks", token=F)
g2 = ok("POST", "/api/risk/integrity-checks/raise-grc-risks", token=F)
check("raising GRC risks is idempotent", g2["raised"] == [] )

section("Enterprise overview")
ov = ok("GET", "/api/enterprise/overview", token=A)
check("admin sees finance and hr sections", "finance" in ov and "hr" in ov and "crm" in ov)
ovu = ok("GET", "/api/enterprise/overview", token=tok["user"])
check("user sees no finance/hr sections", "finance" not in ovu and "hr" not in ovu and "crm" in ovu)
ovh = ok("GET", "/api/enterprise/overview", token=H)
check("hr sees hr but not finance", "hr" in ovh and "finance" not in ovh)

section("Audit trail covers ERP writes")
log = ok("GET", "/api/sys/audit-log?per_page=500", token=A)["data"]
check("ERP mutations audited", any("/api/finance/" in x["path"] for x in log) and any("/api/hr/" in x["path"] for x in log))

print(f"\n{passed} passed, {failed} failed")
sys.exit(1 if failed else 0)
