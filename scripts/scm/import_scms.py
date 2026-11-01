#!/usr/bin/env python3
"""Load the SCMS Delivery History (PEPFAR health commodities) workbook into PostgreSQL.

    DATABASE_URL=postgres://... python3 scripts/import_scms.py SCMS_Delivery_History_Raw_Data.xlsx

Only lines with PO-sent, scheduled and delivered dates are imported (about 45% of the file). Lines are grouped into one PO per
delivery note; vendor INCO terms, shipment mode, numeric freight and insurance are kept.
Quantities are in PACKS and Line Item Value = quantity x Pack Price (98.8% of rows); the file's
'Unit Price' is per single unit, so Pack Price is used as the unit cost.
Freight recorded as text ('Freight Included in Commodity Cost', 'See DN-...') is imported as 0 with
freight_included set for the first kind.
"""
import io, os, sys, uuid
import pandas as pd, psycopg2

conn = psycopg2.connect(os.environ["DATABASE_URL"]); cur = conn.cursor()
U = lambda: str(uuid.uuid4())
def copy(table, df):
    buf = io.StringIO(); df.to_csv(buf, index=False, header=False, na_rep="\\N"); buf.seek(0)
    cur.copy_expert(f"COPY {table} ({','.join(df.columns)}) FROM STDIN WITH (FORMAT csv, NULL '\\N')", buf)
s = pd.read_excel(sys.argv[1]); s.columns = [c.replace("ï»¿", "").replace("Ã¯Â»Â¿", "").strip() for c in s.columns]
for c in ["PO Sent to Vendor Date", "Scheduled Delivery Date", "Delivered to Client Date"]:
    s[c + "_d"] = pd.to_datetime(s[c], errors="coerce")
total = len(s)
s = s.dropna(subset=["PO Sent to Vendor Date_d", "Scheduled Delivery Date_d", "Delivered to Client Date_d"]).reset_index(drop=True)
s = s[s["Line Item Quantity"] > 0].reset_index(drop=True)
fr = pd.to_numeric(s["Freight Cost (USD)"], errors="coerce"); ins = pd.to_numeric(s["Line Item Insurance (USD)"], errors="coerce")
included = s["Freight Cost (USD)"].astype(str).str.startswith("Freight Included")
mode = s["Shipment Mode"].map({"Air": "air", "Air Charter": "air_charter", "Ocean": "sea", "Truck": "road"})
inco = s["Vendor INCO Term"].where(s["Vendor INCO Term"].isin(["EXW", "FCA", "FOB", "CIF", "CIP", "DDP", "DAP", "CPT", "DDU"]))

# One PO per delivery note (ASN/DN #) and vendor: SCMS books freight once per delivery, and the other lines of that
# delivery only say "See DN-...". Grouping them lets landed-cost allocation spread the freight over the right lines.
s["grp"] = s["ASN/DN #"].astype(str) + "|" + s.Vendor.astype(str)
s["fr_num"], s["ins_num"], s["fr_incl"] = fr.fillna(0), ins.fillna(0), included
g = s.groupby("grp", sort=False)
hdr = g.agg(first=("ID", "first"), vendor=("Vendor", "first"), order=("PO Sent to Vendor Date_d", "min"), exp=("Scheduled Delivery Date_d", "max"),
            freight=("fr_num", "sum"), insurance=("ins_num", "sum"), incl=("fr_incl", "any")).reset_index()
hdr["id"] = [U() for _ in range(len(hdr))]; pid_of = dict(zip(hdr.grp, hdr.id))
first = s.drop_duplicates("grp").set_index("grp")
hdr["mode"] = hdr.grp.map(mode.groupby(s.grp).first()); hdr["inco"] = hdr.grp.map(inco.groupby(s.grp).first())

wh, loc = U(), U()
copy("warehouses", pd.DataFrame({"id": [wh], "code": ["SCMS"], "name": ["SCMS"]}))
copy("locations", pd.DataFrame({"id": [loc], "warehouse_id": [wh], "code": ["MAIN"]}))
vendors = sorted(s.Vendor.astype(str).unique()); vid = {v: U() for v in vendors}
copy("suppliers", pd.DataFrame({"id": [vid[v] for v in vendors], "code": [f"V{i:03d}" for i in range(len(vendors))], "name": vendors}))
items = sorted(s["Item Description"].astype(str).unique()); iid = {i: U() for i in items}
copy("products", pd.DataFrame({"id": [iid[i] for i in items], "sku": [f"I{n:04d}" for n in range(len(items))], "name": items}))
s["po"] = s.grp.map(pid_of); s["ln"] = [U() for _ in range(len(s))]
copy("purchase_orders", pd.DataFrame({"id": hdr.id, "po_number": "SC-" + hdr.index.astype(str) + "-" + hdr["first"].astype(str), "supplier_id": hdr.vendor.astype(str).map(vid), "warehouse_id": wh,
     "status": "received", "order_date": hdr.order.dt.date, "expected_date": hdr.exp.dt.date, "incoterm": hdr.inco, "transport_mode": hdr["mode"],
     "freight_cost": hdr.freight.round(4), "insurance_cost": hdr.insurance.round(4), "freight_included": hdr.incl}))
copy("purchase_order_lines", pd.DataFrame({"id": s.ln, "purchase_order_id": s.po, "product_id": s["Item Description"].astype(str).map(iid),
     "quantity_ordered": s["Line Item Quantity"].astype(int), "quantity_received": s["Line Item Quantity"].astype(int),
     "unit_cost": s["Pack Price"].astype(float).round(4), "promised_date": s["Scheduled Delivery Date_d"].dt.date}))
copy("goods_receipts", pd.DataFrame({"id": [U() for _ in range(len(s))], "purchase_order_id": s.po, "purchase_order_line_id": s.ln, "product_id": s["Item Description"].astype(str).map(iid),
     "location_id": loc, "quantity": s["Line Item Quantity"].astype(int), "received_on": s["Delivered to Client Date_d"].dt.date}))
conn.commit()
print(f"imported {len(s)} of {total} lines in {len(hdr)} delivery-note POs; {len(vendors)} vendors, {len(items)} items; numeric freight on {int(fr.notna().sum())} lines")
