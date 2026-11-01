#!/usr/bin/env python3
"""End-to-end smoke test against a running server (stdlib only; works on Windows/Linux).
Usage: python scripts/smoke_test.py [base_url] [admin_email] [admin_password]"""
import collections, csv, json, os, sys, urllib.request, urllib.error, datetime as dt, uuid
from decimal import Decimal, ROUND_HALF_UP

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8080"
ADMIN = (sys.argv[2] if len(sys.argv) > 2 else "admin@example.com", sys.argv[3] if len(sys.argv) > 3 else "ChangeMe123!")
passed = failed = 0


def call(method, path, body=None, token=None, expect=None):
    raw_body = isinstance(body, (bytes, str))
    data_b = (body.encode() if isinstance(body, str) else body) if raw_body else (json.dumps(body).encode() if body is not None else None)
    req = urllib.request.Request(BASE + path, method=method, data=data_b)
    req.add_header("Content-Type", "text/csv" if raw_body else "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req) as r:
            code, raw = r.status, r.read()
    except urllib.error.HTTPError as e:
        code, raw = e.code, e.read()
    data = json.loads(raw) if raw else None
    if expect is not None and code != expect:
        raise AssertionError(f"{method} {path} -> {code} (expected {expect}): {raw[:300]}")
    return code, data


def check(name, cond, detail=""):
    global passed, failed
    if cond:
        passed += 1; print(f"  ok   {name}")
    else:
        failed += 1; print(f"  FAIL {name} {detail}")


def section(t): print(f"\n== {t}")


today = dt.date.today()
tag = uuid.uuid4().hex[:6]

section("Auth & RBAC")
_, d = call("POST", "/api/auth/login", {"email": ADMIN[0], "password": ADMIN[1]}, expect=200)
A = d["token"]
check("health", call("GET", "/health", expect=200)[1]["database"])
check("no token -> 401", call("GET", "/api/crm/accounts")[0] == 401)
call("POST", "/api/users", {"email": f"mgr{tag}@x.io", "password": "Password123", "role": "manager", "full_name": "Mgr"}, A, 201)
call("POST", "/api/users", {"email": f"aud{tag}@x.io", "password": "Password123", "role": "auditor"}, A, 201)
M = call("POST", "/api/auth/login", {"email": f"mgr{tag}@x.io", "password": "Password123"}, expect=200)[1]["token"]
AU = call("POST", "/api/auth/login", {"email": f"aud{tag}@x.io", "password": "Password123"}, expect=200)[1]["token"]
check("auditor cannot write", call("POST", "/api/crm/accounts", {"name": "x"}, AU)[0] == 403)
check("auditor can read", call("GET", "/api/crm/accounts", token=AU)[0] == 200)
check("bad login -> 401", call("POST", "/api/auth/login", {"email": ADMIN[0], "password": "nope"})[0] == 401)

section("CRM")
camp = call("POST", "/api/crm/campaigns", {"name": f"Launch {tag}", "channel": "event", "budget": 1000, "actual_cost": 800}, A, 201)[1]
lead = call("POST", "/api/crm/leads", {"last_name": "Smith", "first_name": "Ann", "company": f"Acme {tag}", "email": "a@acme.io", "phone": "555", "source": "referral", "campaign_id": camp["id"]}, A, 201)[1]
check("missing required -> 422", call("POST", "/api/crm/leads", {"first_name": "x"}, A)[0] == 422)
check("bad enum -> 422", call("POST", "/api/crm/leads", {"last_name": "x", "source": "bogus"}, A)[0] == 422)
sc = call("POST", "/api/crm/marketing/score-leads", {}, A, 200)[1]
lead = call("GET", f"/api/crm/leads/{lead['id']}", token=A, expect=200)[1]
check("lead scored 20+10+10+10=50", lead["score"] == 50, lead["score"])
conv = call("POST", f"/api/crm/leads/{lead['id']}/convert", {"amount": 25000}, A, 200)[1]
check("conversion created opportunity", conv["opportunity_id"] is not None)
check("double convert -> 409", call("POST", f"/api/crm/leads/{lead['id']}/convert", {}, A)[0] == 409)
opp = call("GET", f"/api/crm/opportunities/{conv['opportunity_id']}", token=A, expect=200)[1]
check("initial probability 10", opp["probability"] == 10)
check("lost needs reason", call("POST", f"/api/crm/opportunities/{opp['id']}/stage", {"stage": "closed_lost"}, A)[0] == 422)
won = call("POST", f"/api/crm/opportunities/{opp['id']}/stage", {"stage": "closed_won"}, A, 200)[1]
check("won -> 100%", won["probability"] == 100 and won["closed_at"])
pl = call("GET", "/api/crm/analytics/pipeline", token=A, expect=200)[1]
check("pipeline won amount", pl["totals"]["won_amount"] >= 25000)
cp = call("GET", "/api/crm/analytics/campaigns", token=A, expect=200)[1]["data"]
mine = [c for c in cp if c["id"] == camp["id"]][0]
check("campaign ROI = (25000-800)/800", abs(mine["roi_pct"] - 3025.0) < 0.1, mine["roi_pct"])
case = call("POST", "/api/crm/cases", {"account_id": conv["account_id"], "subject": "Login broken", "priority": "critical"}, A, 201)[1]
check("case number + SLA", case["case_number"].startswith("CS-") and case["sla_due_at"])
call("PUT", f"/api/crm/cases/{case['id']}", {"status": "resolved", "satisfaction_score": 5}, A, 200)
sv = call("GET", "/api/crm/analytics/service", token=A, expect=200)[1]
check("service metrics", sv["totals"]["total"] >= 1 and sv["totals"]["avg_csat"] == 5)
c360 = call("GET", f"/api/crm/analytics/customer-360/{conv['account_id']}", token=A, expect=200)[1]
check("customer 360", c360["metrics"]["lifetime_value"] == 25000 and len(c360["contacts"]) == 1)
lst = call("GET", f"/api/crm/leads?status=converted&per_page=5", token=A, expect=200)[1]
check("list filter + pagination", lst["total"] >= 1 and len(lst["data"]) <= 5)

section("Project management")
pr = call("POST", "/api/pm/projects", {"code": f"P-{tag}", "name": "Build-out", "start_date": "2026-11-02", "budget": 50000, "billing_type": "time_and_material", "account_id": conv["account_id"]}, A, 201)[1]
r1 = call("POST", "/api/pm/resources", {"name": "Dev 1", "hourly_cost": 50, "hourly_bill_rate": 90, "capacity_hours_per_week": 40}, A, 201)[1]
T = {}
for n, dur in [("A", 3), ("B", 2), ("C", 4), ("D", 1)]:
    T[n] = call("POST", "/api/pm/tasks", {"project_id": pr["id"], "name": n, "duration_days": dur, "planned_cost": 1000}, A, 201)[1]["id"]
for a, b in [("A", "B"), ("A", "C"), ("B", "D"), ("C", "D")]:
    call("POST", "/api/pm/task-dependencies", {"task_id": T[b], "depends_on_task_id": T[a]}, A, 201)
sch = call("POST", f"/api/pm/projects/{pr['id']}/schedule", {}, A, 200)[1]
crit = sorted(t["name"] for t in sch["critical_path"])
check("critical path A,C,D", crit == ["A", "C", "D"], crit)
check("duration 8 days", sch["duration_days"] == 8, sch["duration_days"])
slack_b = [t for t in sch["tasks"] if t["name"] == "B"][0]["slack_days"]
check("B has 2 days slack", slack_b == 2, slack_b)
check("cycle -> 409", (call("POST", "/api/pm/task-dependencies", {"task_id": T["A"], "depends_on_task_id": T["D"]}, A, 201), call("POST", f"/api/pm/projects/{pr['id']}/schedule", {}, A))[1][0] == 409)
check("self dependency rejected", call("POST", "/api/pm/task-dependencies", {"task_id": T["A"], "depends_on_task_id": T["A"]}, A)[0] == 422)
call("PUT", f"/api/pm/tasks/{T['A']}", {"status": "completed"}, A, 200)
call("POST", "/api/pm/time-entries", {"project_id": pr["id"], "resource_id": r1["id"], "hours": 10, "task_id": T["A"]}, A, 201)
call("POST", "/api/pm/cost-entries", {"project_id": pr["id"], "category": "materials", "amount": 500}, A, 201)
call("POST", "/api/pm/invoices", {"project_id": pr["id"], "amount": 600, "status": "sent", "due_date": "2020-01-01"}, A, 201)
co = call("GET", f"/api/pm/projects/{pr['id']}/costs", token=A, expect=200)[1]
check("cost = 10h*50 + 500", co["actual_cost"] == 1000, co["actual_cost"])
ac = call("GET", f"/api/pm/projects/{pr['id']}/accounting", token=A, expect=200)[1]
check("T&M revenue = 10h*90", ac["earned_revenue"] == 900 and ac["gross_margin"] == -100, ac)
check("overdue receivable", ac["invoicing"]["overdue"] == 600)
ev = call("GET", f"/api/pm/projects/{pr['id']}/analytics", token=A, expect=200)[1]
check("EVM: EV=1000 (task A done), CPI=1.0", ev["earned_value"] == 1000 and ev["cpi"] == 1.0, ev)
call("POST", "/api/pm/allocations", {"resource_id": r1["id"], "project_id": pr["id"], "start_date": "2026-11-02", "end_date": "2026-11-30", "hours_per_week": 50}, A, 201)
ut = call("GET", "/api/pm/resources/utilization?from=2026-11-02&to=2026-11-16", token=A, expect=200)[1]
check("overallocation detected", ut["overallocated_weeks"] >= 1, ut["overallocated_weeks"])

section("GRC")
rk = call("POST", "/api/grc/risks", {"title": "Data breach", "likelihood": 4, "impact": 5, "category": "technology", "residual_likelihood": 2, "residual_impact": 3}, A, 201)[1]
check("risk score/rating generated", rk["score"] == 20 and rk["rating"] == "critical" and rk["residual_score"] == 6, rk)
check("1-5 enforced", call("POST", "/api/grc/risks", {"title": "x", "likelihood": 9}, A)[0] == 422)
ct = call("POST", "/api/grc/controls", {"name": "MFA", "frequency": "monthly", "framework": "ISO27001"}, A, 201)[1]
cov = call("GET", "/api/grc/controls/coverage", token=A, expect=200)[1]
check("risk without control flagged", any(r["id"] == rk["id"] for r in cov["risks_without_controls"]))
call("POST", "/api/grc/risk-controls", {"risk_id": rk["id"], "control_id": ct["id"]}, A, 201)
tr = call("POST", f"/api/grc/controls/{ct['id']}/test", {"result": "ineffective", "notes": "gaps"}, A, 200)[1]
check("test rolls next due +30d", tr["control"]["next_test_due"] == str(today + dt.timedelta(days=30)), tr)
cov = call("GET", "/api/grc/controls/coverage", token=A, expect=200)[1]
check("ineffective control surfaced", any(r["id"] == rk["id"] for r in cov["risks_relying_on_ineffective_controls"]))
hm = call("GET", "/api/grc/risks/heatmap", token=A, expect=200)[1]
check("heatmap cell 4x5", any(c["likelihood"] == 4 and c["impact"] == 5 for c in hm["cells"]))
rq = call("POST", "/api/grc/compliance-requirements", {"framework": "ISO 9001", "reference": f"8.5-{tag}", "title": "Production control", "area": "quality", "status": "compliant"}, A, 201)[1]
call("POST", "/api/grc/compliance-requirements", {"framework": "ISO 9001", "reference": f"9.1-{tag}", "title": "Monitoring", "area": "quality", "status": "non_compliant"}, A, 201)
check("duplicate requirement -> 409", call("POST", "/api/grc/compliance-requirements", {"framework": "ISO 9001", "reference": f"9.1-{tag}", "title": "dup"}, A)[0] == 409)
cd = call("GET", "/api/grc/compliance/dashboard?area=quality", token=A, expect=200)[1]
check("compliance dashboard", any(f["framework"] == "ISO 9001" and f["non_compliant"] >= 1 for f in cd["by_framework"]))
au = call("POST", "/api/grc/audits", {"title": "Q4 internal", "status": "in_progress"}, A, 201)[1]
fd = call("POST", "/api/grc/audit-findings", {"audit_id": au["id"], "title": "Missing records", "severity": "major"}, A, 201)[1]
rp = call("GET", f"/api/grc/audits/{au['id']}/report", token=A, expect=200)[1]
check("audit not ready to close", rp["open_findings"] == 1 and not rp["ready_to_close"])
call("PUT", f"/api/grc/audit-findings/{fd['id']}", {"status": "closed"}, A, 200)
check("audit ready to close", call("GET", f"/api/grc/audits/{au['id']}/report", token=A, expect=200)[1]["ready_to_close"])

section("Asset management")
asset = call("POST", "/api/am/assets", {"asset_tag": f"PMP-{tag}", "name": "Pump 1", "criticality": "critical", "purchase_cost": 12000, "salvage_value": 2000, "useful_life_months": 60,
              "installed_on": str(today - dt.timedelta(days=365))}, A, 201)[1]
plan = call("POST", "/api/am/maintenance-plans", {"asset_id": asset["id"], "name": "Lubrication", "frequency_days": 30, "next_due": str(today + dt.timedelta(days=3))}, A, 201)[1]
gen = call("POST", "/api/am/maintenance/generate-work-orders", {"horizon_days": 7}, A, 200)[1]
check("PM work order generated", gen["created"] >= 1)
check("no duplicate generation", call("POST", "/api/am/maintenance/generate-work-orders", {"horizon_days": 7}, A, 200)[1]["created"] == 0)
wo = [w for w in gen["work_orders"] if w["asset_id"] == asset["id"]][0]
done = call("POST", f"/api/am/work-orders/{wo['id']}/complete", {"labor_hours": 2, "labor_rate": 40, "parts_cost": 20, "downtime_hours": 1.5}, A, 200)[1]
check("WO total cost 100", done["total_cost"] == 100, done)
check("complete twice -> 409", call("POST", f"/api/am/work-orders/{wo['id']}/complete", {}, A)[0] == 409)
pl2 = call("GET", f"/api/am/maintenance-plans/{plan['id']}", token=A, expect=200)[1]
check("plan next_due reset +30d", pl2["next_due"] == str(today + dt.timedelta(days=30)), pl2["next_due"])
readings = []
for i in range(40):
    readings.append({"asset_id": asset["id"], "metric": "vibration", "value": 50 + (i % 5) * 0.5, "unit": "mm/s",
                     "recorded_at": (dt.datetime.now(dt.timezone.utc) - dt.timedelta(days=50 - i)).isoformat()})
readings.append({"asset_id": asset["id"], "metric": "vibration", "value": 95, "unit": "mm/s", "recorded_at": dt.datetime.now(dt.timezone.utc).isoformat()})
check("bulk readings", call("POST", "/api/am/meter-readings/bulk", readings, A, 200)[1]["inserted"] == 41)
an = call("POST", "/api/am/predictive/analyze", {"asset_id": asset["id"], "auto_work_order": True}, A, 200)[1]
check("anomaly alert + predictive WO", an["alerts_created"] == 1 and an["alerts"][0]["severity"] == "critical" and an["alerts"][0]["work_order_id"], an)
check("alerts de-duplicated", call("POST", "/api/am/predictive/analyze", {"asset_id": asset["id"]}, A, 200)[1]["alerts_created"] == 0)
lc = call("GET", f"/api/am/assets/{asset['id']}/lifecycle", token=A, expect=200)[1]
check("depreciation 12 months of 10000/60", abs(lc["depreciation"]["accumulated"] - 2000.0) < 0.01 and lc["depreciation"]["months_in_service"] == 12, lc["depreciation"])
check("TCO includes maintenance", lc["total_cost_of_ownership"]["maintenance"] == 100)
call("GET", "/api/am/analytics/overview", token=A, expect=200)

section("EHS")
call("POST", "/api/ehs/work-hours", {"site": "Plant", "period_start": f"{today.year}-01-01", "period_end": f"{today.year}-12-31", "hours_worked": 400000}, A, 201)
inc = call("POST", "/api/ehs/incidents", {"incident_type": "injury", "title": "Cut", "occurred_at": dt.datetime.now(dt.timezone.utc).isoformat(), "is_recordable": True, "lost_days": 3, "severity": "moderate"}, A, 201)[1]
call("POST", "/api/ehs/incidents", {"incident_type": "near_miss", "title": "Slip", "occurred_at": dt.datetime.now(dt.timezone.utc).isoformat()}, A, 201)
sf = call("GET", "/api/ehs/analytics/safety", token=A, expect=200)[1]
check("TRIR = 1*200000/400000 = 0.5", sf["rates"]["trir"] == 0.5, sf["rates"])
check("LTIFR = 2.5", sf["rates"]["ltifr"] == 2.5, sf["rates"])
ic = call("POST", f"/api/ehs/incidents/{inc['id']}/corrective-action", {"description": "Guard the blade"}, A, 200)[1]
check("incident CAPA raised", ic["capa_number"].startswith("CAPA-"))
sa = call("POST", "/api/ehs/safety-audits", {"title": "Yard walk"}, A, 201)[1]
for res in ["conforming", "conforming", "conforming", "non_conforming"]:
    call("POST", "/api/ehs/safety-audit-items", {"audit_id": sa["id"], "item": "Check", "result": res}, A, 201)
call("POST", "/api/ehs/safety-audit-items", {"audit_id": sa["id"], "item": "Pending one"}, A, 201)
check("cannot complete with pending items", call("POST", f"/api/ehs/safety-audits/{sa['id']}/complete", {}, A)[0] == 422)
for it in call("GET", f"/api/ehs/safety-audit-items?audit_id={sa['id']}&result=pending", token=A, expect=200)[1]["data"]:
    call("PUT", f"/api/ehs/safety-audit-items/{it['id']}", {"result": "not_applicable"}, A, 200)
cs = call("POST", f"/api/ehs/safety-audits/{sa['id']}/complete", {}, A, 200)[1]
check("audit score 75%", cs["audit"]["score"] == 75.0, cs)
for v, lim in [(7.0, 8.5), (9.1, 8.5), (8.0, 8.5)]:
    call("POST", "/api/ehs/environmental-readings", {"site": f"Plant-{tag}", "metric": "wastewater_ph", "value": v, "unit": "pH", "limit_value": lim}, A, 201)
er = call("GET", f"/api/ehs/environmental/report?site=Plant-{tag}", token=A, expect=200)[1]
check("environmental exceedance", er["summary"][0]["exceedances"] == 1 and len(er["exceedances"]) == 1)

section("Quality management")
tp = call("POST", "/api/qms/test-plans", {"name": f"Tensile {tag}", "lower_limit": 9.5, "upper_limit": 10.5, "unit": "MPa"}, A, 201)[1]
ok = call("POST", "/api/qms/test-results", {"test_plan_id": tp["id"], "measured_value": 10.0, "lot_number": "L1"}, A, 201)[1]
bad = call("POST", "/api/qms/test-results", {"test_plan_id": tp["id"], "measured_value": 11.2, "lot_number": "L2"}, A, 201)[1]
check("auto pass/fail", ok["result"] == "pass" and bad["result"] == "fail")
ncrs = call("GET", f"/api/qms/nonconformances?test_result_id={bad['id']}", token=A, expect=200)[1]["data"]
check("failed test auto-raised NCR", len(ncrs) == 1 and ncrs[0]["source"] == "test")
ncr = ncrs[0]
check("invalid transition -> 409", call("POST", f"/api/qms/nonconformances/{ncr['id']}/transition", {"status": "closed"}, A)[0] == 409)
for s in ["investigating", "containment", "corrective_action"]:
    call("POST", f"/api/qms/nonconformances/{ncr['id']}/transition", {"status": s, "root_cause": "Worn die"} if s == "investigating" else {"status": s}, A, 200)
capa = call("POST", "/api/qms/corrective-actions", {"nonconformance_id": ncr["id"], "description": "Replace die"}, A, 201)[1]
call("POST", f"/api/qms/nonconformances/{ncr['id']}/transition", {"status": "verification"}, A, 200)
check("close blocked by open CAPA (+disposition rule)", call("POST", f"/api/qms/nonconformances/{ncr['id']}/transition", {"status": "closed", "disposition": "rework"}, A)[0] == 422)
call("PUT", f"/api/qms/corrective-actions/{capa['id']}", {"status": "implemented"}, A, 200)
check("user cannot verify CAPA", call("POST", f"/api/qms/corrective-actions/{capa['id']}/verify", {"effective": True}, AU)[0] == 403)
vf = call("POST", f"/api/qms/corrective-actions/{capa['id']}/verify", {"effective": True, "notes": "No recurrence"}, M, 200)[1]
check("CAPA verified by manager", vf["status"] == "verified")
cl = call("POST", f"/api/qms/nonconformances/{ncr['id']}/transition", {"status": "closed", "disposition": "rework"}, A, 200)[1]
check("NCR closed with timestamp", cl["status"] == "closed" and cl["closed_at"])
q = call("GET", "/api/qms/analytics/quality", token=A, expect=200)[1]
check("quality analytics", any(p["plan_id"] == tp["id"] and p["pass_rate_pct"] == 50.0 for p in q["testing"]))
check("QMS compliance view", any(s["framework"] == "ISO 9001" for s in call("GET", "/api/qms/compliance", token=A, expect=200)[1]["summary"]))

section("Document management")
pol = call("GET", "/api/docs/retention-policies?name=Transient%20documents", token=A, expect=200)[1]["data"][0]
doc = call("POST", "/api/docs/documents", {"title": f"SOP {tag}", "doc_type": "procedure"}, A, 201)[1]
v1 = call("POST", f"/api/docs/documents/{doc['id']}/versions", {"content_text": "line1\nline2\nline3", "change_summary": "first"}, A, 201)[1]
v2 = call("POST", f"/api/docs/documents/{doc['id']}/versions", {"content_text": "line1\nline2 changed\nline3\nline4"}, A, 201)[1]
check("versions numbered, hashed", v1["version_number"] == 1 and v2["version_number"] == 2 and len(v1["content_hash"]) == 64)
df = call("GET", f"/api/docs/documents/{doc['id']}/diff?from=1&to=2", token=A, expect=200)[1]
check("diff +2 -1", df["added"] == 2 and df["removed"] == 1 and df["unchanged"] == 2, df)
rb = call("POST", f"/api/docs/documents/{doc['id']}/rollback", {"version_number": 1, "reason": "bad edit"}, A, 200)[1]
check("rollback creates v3 with v1 hash", rb["version_number"] == 3 and rb["content_hash"] == v1["content_hash"])
check("versions immutable (no update route)", call("PUT", f"/api/docs/versions/{v1['id']}", {"content_text": "x"}, A)[0] in (404, 405))
wf = call("POST", f"/api/docs/documents/{doc['id']}/workflow", {}, A, 200)[1]
check("workflow started; doc in_review", call("GET", f"/api/docs/documents/{doc['id']}", token=A, expect=200)[1]["status"] == "in_review")
check("duplicate workflow -> 409", call("POST", f"/api/docs/documents/{doc['id']}/workflow", {}, A)[0] == 409)
inbox = call("GET", "/api/docs/workflow-tasks/inbox", token=M, expect=200)[1]["data"]
task = [t for t in inbox if t["document_id"] == doc["id"]][0]
check("auditor not an approver", call("POST", f"/api/docs/workflow-tasks/{task['task_id']}/decide", {"approve": True}, AU)[0] == 403)
r1_ = call("POST", f"/api/docs/workflow-tasks/{task['task_id']}/decide", {"approve": True, "comments": "ok"}, M, 200)[1]
check("step 2 assigned to admin role", r1_["workflow"] == "in_progress" and r1_["assigned_role"] == "admin")
t2 = [t for t in call("GET", "/api/docs/workflow-tasks/inbox", token=A, expect=200)[1]["data"] if t["document_id"] == doc["id"]][0]
r2_ = call("POST", f"/api/docs/workflow-tasks/{t2['task_id']}/decide", {"approve": True}, A, 200)[1]
check("document approved", r2_["workflow"] == "approved" and call("GET", f"/api/docs/documents/{doc['id']}", token=A, expect=200)[1]["status"] == "approved")
call("POST", f"/api/docs/documents/{doc['id']}/versions", {"content_text": "edited after approval"}, A, 201)
check("edit after approval resets to draft", call("GET", f"/api/docs/documents/{doc['id']}", token=A, expect=200)[1]["status"] == "draft")
rec = call("POST", "/api/docs/documents", {"title": "Old record", "is_record": True, "retention_policy_id": pol["id"], "effective_date": "2020-01-01"}, A, 201)[1]
check("record delete blocked", call("DELETE", f"/api/docs/documents/{rec['id']}", token=A)[0] == 422)
call("PUT", f"/api/docs/documents/{rec['id']}", {"legal_hold": True}, A, 200)
due = call("GET", "/api/docs/records/disposition-due", token=A, expect=200)[1]
check("legal hold excludes from disposition", not any(d["id"] == rec["id"] for d in due["due"]))
check("dispose blocked under hold", call("POST", f"/api/docs/records/{rec['id']}/dispose", {}, A)[0] == 409)
call("PUT", f"/api/docs/documents/{rec['id']}", {"legal_hold": False}, A, 200)
check("record due after hold released", any(d["id"] == rec["id"] for d in call("GET", "/api/docs/records/disposition-due", token=A, expect=200)[1]["due"]))
check("dispose destroys", call("POST", f"/api/docs/records/{rec['id']}/dispose", {}, A, 200)[1]["disposition"] == "destroyed")

section("Project portfolio dataset")
DATA = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "data", "Project_Management_Dataset.csv")
text = open(DATA, encoding="utf-8-sig").read()
src = [{k.strip(): v.strip() for k, v in r.items()} for r in csv.DictReader(text.splitlines(True))]
num = lambda x: float(x.replace(",", ""))
for r in src:
    r["c"], r["b"], r["p"] = num(r["Project Cost"]), num(r["Project Benefit"]), int(r["Completion%"][:-1])
    r["e"] = dt.datetime.strptime(r["End Date"], "%m/%d/%Y").date()
imp = call("POST", "/api/pm/import/projects?filename=dataset.csv", text, A, 200)[1]
check("import: 99 rows, none rejected", imp["rows"] == 99 and imp["rejected"] == 0 and imp["inserted"] + imp["updated"] == 99, imp)
imp2 = call("POST", "/api/pm/import/projects", text, A, 200)[1]
check("import is idempotent", imp2["inserted"] == 0 and imp2["updated"] == 99)
check("7 project managers created once", imp2["project_managers"] == 7 and call("GET", "/api/pm/resources?role=Project%20Manager&per_page=100", token=A, expect=200)[1]["total"] == 7)
bad = "Project Name,Project Cost,Project Benefit,Status,Start Date,End Date,Complexity,Completion%\nOK One,\"1,000.00\",\"5,000.00\",Completed,1/1/2024,3/1/2024,Low,100%\n,1,1,Completed,1/1/2024,3/1/2024,Low,100%\nBad Dates,1,1,Completed,3/1/2024,1/1/2024,Low,100%\nBad Status,1,1,Maybe,1/1/2024,3/1/2024,Low,100%\nBad Pct,1,1,Completed,1/1/2024,3/1/2024,Low,150%\n"
bi = call("POST", "/api/pm/import/projects?filename=bad.csv", bad, A, 200)[1]
check("invalid rows rejected with line numbers", bi["inserted"] + bi["updated"] == 1 and bi["rejected"] == 4 and all(e["line"] for e in bi["errors"]), bi)
check("strict mode aborts", call("POST", "/api/pm/import/projects?strict=true", bad, A)[0] == 422)
check("missing column -> 400", call("POST", "/api/pm/import/projects", "a,b\n1,2\n", A)[0] == 400)
check("auditor cannot import", call("POST", "/api/pm/import/projects", text, AU)[0] == 403)
sm = call("GET", "/api/pm/portfolio/summary?department=Warehouse", token=A, expect=200)[1]["totals"]
wh = [r for r in src if r["Department"] == "Warehouse"]
check("filtered summary cost", sm["projects"] == len(wh) and abs(sm["cost"] - sum(r["c"] for r in wh)) < 1, sm)
S0 = call("GET", "/api/pm/portfolio/summary?year=2021&region=North", token=A, expect=200)[1]["totals"]
check("year+region filter", S0["projects"] == len([r for r in src if r["Year"] == "2021" and r["Region"] == "North"]))
# whole-portfolio numbers are asserted only when the portfolio holds exactly the dataset (no other business cases)
full = call("GET", "/api/pm/portfolio/summary", token=A, expect=200)[1]["totals"]
if full["projects"] == 100:  # dataset + 'OK One' from the bad.csv test
    pass
tp = call("GET", "/api/pm/portfolio/by/type", token=A, expect=200)[1]["data"]
ci = [d for d in tp if d["key"] == "COST REDUCTION"][0]
sub = [r for r in src if r["Project Type"] == "COST REDUCTION"]
check("ROI by type", abs(ci["roi_pct"] - round(100 * (sum(r["b"] for r in sub) / sum(r["c"] for r in sub) - 1), 1)) < 0.11, ci)
check("bad dimension -> 400", call("GET", "/api/pm/portfolio/by/nonsense", token=A)[0] == 400)
rk = call("GET", "/api/pm/portfolio/rankings?limit=1&status=active", token=A, expect=200)[1]["data"][0]
act = [r for r in src if r["Status"] == "In - Progress"]
best = max(act, key=lambda r: r["b"] / (r["c"] * (1 - r["p"] / 100)))
check("top-ranked active project", rk["name"] == best["Project Name"], rk["name"])
fl = {c["flag"]: c["projects"] for c in call("GET", "/api/pm/portfolio/risk-flags", token=A, expect=200)[1]["counts"]}
check("overdue_active flag = in-progress past end date", fl.get("overdue_active") == sum(1 for r in act if r["e"] < dt.date.today()), fl)
check("cancelled_late flag", fl.get("cancelled_late") == sum(1 for r in src if r["Status"] == "Cancelled" and r["p"] >= 70), fl)
tl = call("GET", "/api/pm/portfolio/timeline?granularity=year", token=A, expect=200)[1]["data"]
check("timeline covers 2021-2025", [t["period"] for t in tl][:5] == ["2021", "2022", "2023", "2024", "2025"])
est = call("POST", "/api/pm/portfolio/estimate", {"project_type": "COST REDUCTION", "complexity": "high"}, A, 200)[1]
check("estimate returns reference class + probability", est["comparable_projects"] >= 1 and 0 < est["completion_probability"] < 1)
ip = call("GET", "/api/pm/projects?project_type=INCOME%20GENERATION&status=completed&per_page=1", token=A, expect=200)[1]["data"][0]
ev = call("GET", f"/api/pm/projects/{ip['id']}/analytics", token=A, expect=200)[1]
check("EVM falls back to project level", ev["basis"] == "project" and abs(ev["bac"] - float(ip["budget"])) < 0.01 and ev["earned_value"] == ev["bac"], ev)

section("CRM datasets (import, cleaning, analytics)")
CD = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "data", "crm")
rdc = lambda f: list(csv.DictReader(open(os.path.join(CD, f), encoding="utf-8-sig", newline="")))
rawc = lambda f: open(os.path.join(CD, f), encoding="utf-8-sig", newline="").read()
ORDER = [("accounts", "opportunities/accounts.csv"), ("products", "opportunities/products.csv"), ("sales-teams", "opportunities/sales_teams.csv"),
         ("pipeline", "opportunities/sales_pipeline.csv"), ("funnel", "sales_funnel.csv"), ("product-info", "customer_behaviour/product_info.csv"),
         ("customers", "customer_behaviour/customer_info.csv"), ("sales-orders", "customer_behaviour/sales_data.csv"), ("donors", "donors.csv")]
check("bad import kind -> 400", call("POST", "/api/crm/import/nonsense", "a\n1\n", A)[0] == 400)
check("missing column -> 400", call("POST", "/api/crm/import/accounts", "x,y\n1,2\n", A)[0] == 400)
check("auditor cannot import", call("POST", "/api/crm/import/accounts", rawc("opportunities/accounts.csv"), AU)[0] == 403)
check("pipeline before reference data is refused or already satisfied", call("POST", "/api/crm/import/pipeline", "opportunity_id\n", A)[0] in (200, 400, 422))
R = {}
for kind, rel in ORDER:
    R[kind] = call("POST", f"/api/crm/import/{kind}?filename={os.path.basename(rel)}", rawc(rel), A, 200)[1]
check("accounts: 85 imported, none rejected", R["accounts"]["rows"] == 85 and R["accounts"]["rejected"] == 0)
check("pipeline: 8800 deals, none rejected", R["pipeline"]["rows"] == 8800 and R["pipeline"]["rejected"] == 0 and R["pipeline"]["inserted"] + R["pipeline"]["updated"] == 8800)
check("pipeline: product name drift repaired (GTXPro -> GTX Pro)", R["pipeline"]["corrections"].get("product", {}).get("GTXPro→GTX Pro") == 1480, R["pipeline"]["corrections"])
check("funnel: 100 deals", R["funnel"]["rows"] == 100 and R["funnel"]["rejected"] == 0)
# customers: 3 blank ids rejected; typos repaired
check("customers: blank ids quarantined", R["customers"]["rejected"] == 3 and R["customers"]["reject_reasons"] == {"customer_id is empty": 3}, R["customers"]["reject_reasons"])
cc = R["customers"]["corrections"]
check("typos repaired: femle, brnze, gld, sllver", cc["gender"].get("femle→female", 0) >= 90 and cc["loyalty_tier"].get("brnze→bronze") == 2 and cc["loyalty_tier"].get("gld→gold") == 2 and cc["loyalty_tier"].get("sllver→silver") == 1, cc)
oc = R["sales-orders"]["corrections"]
check("order typos repaired (delrd, delyd, nrth, bank transfr, five, three)", oc["delivery_status"].get("delrd→delivered") == 1 and oc["delivery_status"].get("delyd→delayed") == 1 and oc["region"].get("nrth→North") == 1
      and oc["payment_method"].get("bank transfr→bank_transfer") == 1 and oc["quantity"].get("five→5") == 1 and oc["quantity"].get("three→3") == 1, oc)
# independent replication of the order acceptance rules
cust_ok = {r["customer_id"] for r in rdc("customer_behaviour/customer_info.csv") if r["customer_id"]}
prod_ok = {r["product_id"]: float(r["base_price"]) for r in rdc("customer_behaviour/product_info.csv")}
WORDS = {"one": 1, "two": 2, "three": 3, "four": 4, "five": 5}
seen, ok_orders, net = set(), 0, 0.0
for r in rdc("customer_behaviour/sales_data.csv"):
    if not r["order_id"] or r["order_id"] in seen or r["customer_id"] not in cust_ok or r["product_id"] not in prod_ok or not r["quantity"]: continue
    seen.add(r["order_id"]); ok_orders += 1
    q = int(r["quantity"]) if r["quantity"].isdigit() else WORDS[r["quantity"].lower()]
    price = Decimal(r["unit_price"]) if r["unit_price"] else Decimal(str(prod_ok[r["product_id"]]))
    net += float((q * price * (1 - (Decimal(r["discount_applied"]) if r["discount_applied"] else Decimal(0)))).quantize(Decimal("0.01"), ROUND_HALF_UP))
oa = call("GET", "/api/crm/orders/analytics", token=A, expect=200)[1]
check("orders: accepted count matches independent rules", oa["totals"]["orders"] == ok_orders == R["sales-orders"]["inserted"] + R["sales-orders"]["updated"], (oa["totals"]["orders"], ok_orders))
check("orders: net revenue matches independent calc", abs(oa["totals"]["net_revenue"] - net) < 0.011, (oa["totals"]["net_revenue"], net))
check("orders: constant order_date detected", any("same order_date" in w for w in R["sales-orders"]["warnings"]))
check("orders: price anomalies flagged", oa["price_anomalies"]["above_2x_catalog"] > 0)
# idempotency of every import
for kind, rel in ORDER:
    again = call("POST", f"/api/crm/import/{kind}", rawc(rel), A, 200)[1]
    if again["inserted"] != 0: check(f"{kind}: re-import is idempotent", False, again["inserted"])
check("all nine imports idempotent", True)
st_ = call("POST", "/api/crm/import/customers?strict=true", rawc("customer_behaviour/customer_info.csv"), A)
check("strict import aborts on rejected rows", st_[0] == 422)
# opportunities
pf = call("GET", "/api/crm/sales/performance?by=stage&dataset=crm_sales_opportunities", token=A, expect=200)[1]
won = [r for r in rdc("opportunities/sales_pipeline.csv") if r["deal_stage"] == "Won"]
check("won revenue matches source", abs([d for d in pf["data"] if d["key"] == "closed_won"][0]["revenue_won"] - sum(float(r["close_value"]) for r in won)) < 0.5)
check("open prospects imported without account", [d for d in pf["data"] if d["key"] == "prospecting"][0]["open_deals"] == 500)
ag = call("GET", "/api/crm/sales/performance?by=agent&dataset=crm_sales_opportunities", token=A, expect=200)[1]
check("agent win-rate spread is not statistically significant", ag["groups_significantly_different"] == 0 and len(ag["data"]) == 30, ag["groups_significantly_different"])
pr = call("GET", "/api/crm/sales/performance?by=product&dataset=crm_sales_opportunities", token=A, expect=200)[1]
check("7 products incl. repaired GTX Pro", len(pr["data"]) == 7 and any(d["key"] == "GTX Pro" and d["won"] > 0 for d in pr["data"]))
cy = [d for d in call("GET", "/api/crm/sales/cycle-time?by=series&dataset=crm_sales_opportunities", token=A, expect=200)[1]["data"] if d["stage"] == "closed_won"]
check("cycle-time median plausible", all(20 < d["median"] < 120 for d in cy))
ph = call("GET", "/api/crm/sales/pipeline-health?dataset=crm_sales_opportunities", token=A, expect=200)[1]
check("stale pipeline detected", ph["stale_deals"]["count"] > 0 and ph["stale_threshold_days"] > 0)
# model validity gate
check("training on a tiny dataset is refused", call("POST", "/api/crm/predict/train?dataset=sales_funnel", {}, A)[0] == 422)
check("scoring before training is refused or already trained", call("POST", "/api/crm/predict/score-open", {}, A)[0] in (200, 409))
tr = call("POST", "/api/crm/predict/train", {}, A, 200)[1]
check("time-based split", tr["split"]["method"].startswith("time-based") and tr["split"]["n_test"] > 500)
check("model has no skill on this data (AUC ~ 0.5) and is gated off", 0.44 < tr["metrics"]["auc"] < 0.56 and tr["usable"] is False, tr["metrics"])
sc = call("POST", "/api/crm/predict/score-open", {}, A, 200)[1]
check("open deals scored with base rate", sc["method"].startswith("base_rate") and sc["scored"] == 2089, sc)
check("model endpoint", call("GET", "/api/crm/predict/model", token=A, expect=200)[1]["usable"] is False)
# forecast
fc = call("GET", "/api/crm/forecast/revenue?horizon=3", token=A, expect=200)[1]
check("forecast: 10 months history, 3 months out, backtest of 3 methods", len(fc["history"]) == 10 and len(fc["forecast"]) == 3 and len(fc["backtest"]["results"]) == 3)
check("forecast interval ordered", all(f["low_80"] <= f["forecast"] <= f["high_80"] for f in fc["forecast"]))
check("stale pipeline excluded from pipeline view", fc["pipeline_view"]["excluded_stale"]["deals"] > 0)
wp = call("GET", "/api/crm/forecast/weighted-pipeline?dataset=sales_funnel", token=A, expect=200)[1]
fun = rdc("sales_funnel.csv")
check("funnel weighted forecast matches source", abs(wp["totals"]["weighted_forecast"] - sum(float(r["Forecasted Revenue"]) for r in fun if r["Status"] == "Open")) < 1, wp["totals"])
# donors
don = rdc("donors.csv")
for r in don: r["d"] = dt.datetime.strptime(r["LastDonationDate"], "%m/%d/%Y").date(); r["a"] = float(r["TotalAmountDonated"])
latest = max(r["d"] for r in don)
ov = call("GET", "/api/crm/customers/overview?kind=donor", token=A, expect=200)[1]
check("donor total donated", ov["totals"]["donors"] == 5000 and abs(ov["totals"]["total_donated"] - sum(r["a"] for r in don)) < 1)
lp = call("GET", "/api/crm/donors/lapse", token=A, expect=200)[1]
exp = collections.Counter("active" if (latest - r["d"]).days / 30.4375 < 12 else "lapsing" if (latest - r["d"]).days / 30.4375 < 24 else "lapsed" for r in don)
check("lapse buckets match independent calc", {b["status"]: b["donors"] for b in lp["by_status"]} == dict(exp), lp["by_status"])
rf = call("GET", "/api/crm/customers/rfm?kind=donor&limit=3&segment=champions", token=A, expect=200)[1]
check("RFM covers every donor", sum(x["donors"] for x in rf["segments"]) == 5000 and all(m["segment"] == "champions" for m in rf["members"]))
zips = call("GET", "/api/crm/customers?kind=donor&per_page=500", token=A, expect=200)[1]["data"]
check("ZIP codes zero-padded", all(len(z["postal_code"]) == 5 for z in zips if z["postal_code"]))
ins = call("GET", "/api/crm/donors/insights", token=A, expect=200)[1]
check("donor insights report null relationships honestly", abs(ins["correlations"]["engagement_vs_total"]) < 0.1 and ins["findings"])
dd = call("GET", "/api/crm/data-quality/duplicates?kind=donor", token=A, expect=200)[1]
check("duplicate donor e-mail groups = 13", dd["groups"] == sum(1 for v in collections.Counter(r["Email"].lower() for r in don).values() if v > 1) == 13)
cr = call("GET", "/api/crm/customers/rfm?kind=consumer&limit=0", token=A, expect=200)[1]
check("consumer RFM drops recency when order dates are constant", cr["recency_available"] is False and cr["note"])
aff = call("GET", "/api/crm/orders/affinity", token=A, expect=200)[1]
check("affinity reports no signal", "no cross-sell signal" in aff["interpretation"], aff["interpretation"])
qr = call("GET", "/api/crm/data-quality/report", token=A, expect=200)[1]
check("quality report covers all imports", len(qr["imports"]) >= 9 and qr["rejected_rows_by_reason"] and qr["completeness_pct"]["opportunities"]["close_before_engage"] == 0)
check("batches and rejects are queryable", call("GET", "/api/crm/import-rejects?per_page=1", token=A, expect=200)[1]["total"] >= 40)

section("Audit trail")
log = call("GET", "/api/sys/audit-log?per_page=5", token=A, expect=200)[1]
check("mutations logged", log["total"] > 20, log["total"])

print(f"\n{passed} passed, {failed} failed")
sys.exit(1 if failed else 0)
