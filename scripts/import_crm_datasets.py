#!/usr/bin/env python3
"""Imports the bundled CRM datasets in dependency order (stdlib only).
Usage: python scripts/import_crm_datasets.py [base_url] [email] [password]
Safe to re-run: every import is an idempotent upsert."""
import json, os, sys, urllib.request, urllib.error

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8080"
EMAIL = sys.argv[2] if len(sys.argv) > 2 else "admin@example.com"
PASSWORD = sys.argv[3] if len(sys.argv) > 3 else "ChangeMe123!"
D = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "data", "crm")
ORDER = [("accounts", "opportunities/accounts.csv"), ("products", "opportunities/products.csv"), ("sales-teams", "opportunities/sales_teams.csv"),
         ("pipeline", "opportunities/sales_pipeline.csv"), ("funnel", "sales_funnel.csv"), ("product-info", "customer_behaviour/product_info.csv"),
         ("customers", "customer_behaviour/customer_info.csv"), ("sales-orders", "customer_behaviour/sales_data.csv"), ("donors", "donors.csv")]


def post(path, data, token=None, ctype="application/json"):
    req = urllib.request.Request(BASE + path, data=data, method="POST", headers={"Content-Type": ctype})
    if token: req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req) as r: return r.status, json.load(r)
    except urllib.error.HTTPError as e: return e.code, json.load(e)


def login():
    code, d = post("/api/auth/login", json.dumps({"email": EMAIL, "password": PASSWORD}).encode())
    if code != 200: sys.exit(f"login failed: {d}")
    return d["token"]


def import_all(token, verbose=True):
    results = {}
    for kind, rel in ORDER:
        with open(os.path.join(D, rel), "rb") as f: body = f.read()
        code, d = post(f"/api/crm/import/{kind}?filename={os.path.basename(rel)}", body, token, "text/csv")
        if code != 200: sys.exit(f"{kind}: HTTP {code} {d}")
        results[kind] = d
        if verbose:
            print(f"{kind:13} rows={d['rows']:5} inserted={d['inserted']:5} updated={d['updated']:5} rejected={d['rejected']:3}", dict(d["reject_reasons"]) or "")
    return results


if __name__ == "__main__":
    import_all(login())
