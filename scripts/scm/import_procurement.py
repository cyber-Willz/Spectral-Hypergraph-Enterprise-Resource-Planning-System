#!/usr/bin/env python3
"""Load the 'Purchase Orders & Supplier Performance' dataset (Dataset_Procurement.xlsx) into PostgreSQL.

    DATABASE_URL=postgres://... python3 scripts/import_procurement.py Dataset_Procurement.xlsx

One PO + one line per spreadsheet row, one warehouse, items become products. Useful for exercising the
supplier-concentration / spectral / disruption reports on a densely multi-sourced network.
"""
import io, os, sys, uuid
import pandas as pd, psycopg2

p = pd.read_excel(sys.argv[1], sheet_name="Data")
conn = psycopg2.connect(os.environ["DATABASE_URL"]); cur = conn.cursor()
U = lambda: str(uuid.uuid4())
def copy(table, df):
    buf = io.StringIO(); df.to_csv(buf, index=False, header=False, na_rep="\\N"); buf.seek(0)
    cur.copy_expert(f"COPY {table} ({','.join(df.columns)}) FROM STDIN WITH (FORMAT csv, NULL '\\N')", buf)

p["PO Date"] = pd.to_datetime(p["PO Date"], format="%d/%m/%Y")
wh = U(); copy("warehouses", pd.DataFrame({"id": [wh], "code": ["PROC"], "name": ["Procurement DC"]}))
items = p.groupby("Item Code")["Unit Price"].mean().reset_index()
items["id"] = [U() for _ in range(len(items))]
copy("products", pd.DataFrame({"id": items.id, "sku": items["Item Code"], "name": items["Item Code"],
                               "unit_cost": items["Unit Price"].round(4), "list_price": (items["Unit Price"] * 1.2).round(4)}))
sups = sorted(p["Supplier ID"].unique()); sid = {s: U() for s in sups}
copy("suppliers", pd.DataFrame({"id": [sid[s] for s in sups], "code": sups, "name": sups}))
pid = dict(zip(items["Item Code"], items.id))
p["po_uuid"] = [U() for _ in range(len(p))]
copy("purchase_orders", pd.DataFrame({"id": p.po_uuid, "po_number": p["PO Number"].astype(str) + "-" + p.index.astype(str),
     "supplier_id": p["Supplier ID"].map(sid), "warehouse_id": wh, "status": "received",
     "order_date": p["PO Date"].dt.date, "currency": p["Currency"]}))
copy("purchase_order_lines", pd.DataFrame({"id": [U() for _ in range(len(p))], "purchase_order_id": p.po_uuid,
     "product_id": p["Item Code"].map(pid), "quantity_ordered": p["Quantity"].astype(int),
     "quantity_received": p["Quantity"].astype(int), "unit_cost": p["Unit Price"].round(4)}))
conn.commit()
print("imported", len(p), "POs;", len(items), "items;", len(sups), "suppliers; last PO date", p["PO Date"].max().date())
