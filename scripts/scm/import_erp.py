#!/usr/bin/env python3
"""Load the 'Supply Chain & ERP Simulation' dataset (CSV folder) into PostgreSQL.

    DATABASE_URL=postgres://... python3 scripts/import_erp.py /path/to/synthetic_supply_chain_dataset_v3_eng

Products, suppliers, approved sources (with MOQ and planned lead time taken from the supplier's routes),
purchase orders with receipts and transport mode, sales as 'issue' ledger entries, and the last daily stock
snapshot as opening stock. Run on an empty database.
"""
import io, os, sys, uuid
import pandas as pd, psycopg2

src = sys.argv[1]
conn = psycopg2.connect(os.environ["DATABASE_URL"]); cur = conn.cursor()
U = lambda: str(uuid.uuid4())
def copy(table, df):
    buf = io.StringIO(); df.to_csv(buf, index=False, header=False, na_rep="\\N"); buf.seek(0)
    cur.copy_expert(f"COPY {table} ({','.join(df.columns)}) FROM STDIN WITH (FORMAT csv, NULL '\\N')", buf)
rd = lambda f, **k: pd.read_csv(f"{src}/{f}.csv", **k)

prod, sup, ps, rt = rd("PRODUCT"), rd("SUPPLIER"), rd("PRODUCT_SUPPLIER"), rd("SUPPLY_ROUTE")
pur = rd("PURCHASING", parse_dates=["order_date", "receipt_date"])
sal = rd("SALES", parse_dates=["order_date"]); stock = rd("DAILY_STOCK", parse_dates=["date"])

wh, loc = U(), U()
copy("warehouses", pd.DataFrame({"id": [wh], "code": ["DC"], "name": ["ERP DC"]}))
copy("locations", pd.DataFrame({"id": [loc], "warehouse_id": [wh], "code": ["MAIN"]}))
pid = {p: U() for p in prod.product_id}
copy("products", pd.DataFrame({"id": prod.product_id.map(pid), "sku": prod.product_code.astype(str), "name": prod.product_name,
     "unit_cost": prod.base_cost.round(4), "list_price": prod.base_price.round(4), "holding_cost_rate": prod.holding_cost_rate.round(4)}))
planned = rt.groupby("supplier_id").lead_time_mean_days.mean().round().astype(int)   # supplier-level planned lead time
sid = {s: U() for s in sup.supplier_id}
copy("suppliers", pd.DataFrame({"id": sup.supplier_id.map(sid), "code": "S" + sup.supplier_id.astype(str), "name": sup.supplier_name,
     "country": sup.supplier_country, "lead_time_days": sup.supplier_id.map(planned).fillna(7).astype(int)}))
copy("product_suppliers", pd.DataFrame({"id": [U() for _ in range(len(ps))], "product_id": ps.product_id.map(pid), "supplier_id": ps.supplier_id.map(sid),
     "is_preferred": ps.is_primary.astype(bool), "lead_time_days": ps.supplier_id.map(planned).fillna(7).astype(int), "min_order_qty": ps.min_order_qty.astype(int)}))
mode = {"truck": "road", "air": "air", "rail": "rail", "sea": "sea"}
rt["m"] = rt.transport_mode.str.lower().map(mode)
pur = pur.merge(rt[["route_id", "m"]], on="route_id", how="left")
pur["po"], pur["ln"] = [U() for _ in range(len(pur))], [U() for _ in range(len(pur))]
copy("purchase_orders", pd.DataFrame({"id": pur.po, "po_number": "ERP-" + pur.po_line_id.astype(str), "supplier_id": pur.supplier_id.map(sid), "warehouse_id": wh,
     "status": (pur.qty_received >= pur.qty_ordered).map({True: "received", False: "closed_short"}), "order_date": pur.order_date.dt.date,
     "currency": pur.purch_currency, "transport_mode": pur.m}))
copy("purchase_order_lines", pd.DataFrame({"id": pur.ln, "purchase_order_id": pur.po, "product_id": pur.product_id.map(pid), "quantity_ordered": pur.qty_ordered,
     "quantity_received": pur.qty_received, "unit_cost": pur.unit_cost.round(4)}))
rc = pur[pur.qty_received > 0]
copy("goods_receipts", pd.DataFrame({"id": [U() for _ in range(len(rc))], "purchase_order_id": rc.po, "purchase_order_line_id": rc.ln, "product_id": rc.product_id.map(pid),
     "location_id": loc, "quantity": rc.qty_received, "received_on": rc.receipt_date.dt.date}))
d = sal[sal.qty > 0].groupby(["product_id", "order_date"]).qty.sum().reset_index()
copy("stock_movements", pd.DataFrame({"id": [U() for _ in range(len(d))], "product_id": d.product_id.map(pid), "location_id": loc, "movement_type": "issue",
     "quantity_delta": -d.qty.astype(int), "created_at": d.order_date.dt.strftime("%Y-%m-%d") + " 12:00:00+00"}))
last = stock[stock.date == stock.date.max()]; last = last[last.on_hand_qty > 0]
copy("stock_levels", pd.DataFrame({"id": [U() for _ in range(len(last))], "product_id": last.product_id.map(pid), "location_id": loc, "quantity": last.on_hand_qty.astype(int)}))
conn.commit()
print("imported", len(prod), "products,", len(sup), "suppliers,", len(ps), "source links,", len(pur), "PO lines,", len(d), "issue days; last stock date", stock.date.max().date())
