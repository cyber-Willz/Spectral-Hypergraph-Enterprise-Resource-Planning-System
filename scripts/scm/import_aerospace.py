#!/usr/bin/env python3
"""Load the 'Aerospace Supply Chain Performance & Forecasting' dataset straight into PostgreSQL.

    pip install psycopg2-binary pandas
    DATABASE_URL=postgres://... python3 scripts/import_aerospace.py /path/to/dataset_dir

Mapping: site -> warehouse + one bin; part -> product (criticality, lead time, shelf life);
purchase order row -> PO + line + goods receipt (closed_short when received < ordered);
quality incident -> quality_incidents; weekly consumption -> 'issue' ledger entries;
forecast_qty -> demand_forecasts (baseline / adjusted). Run it on an empty database.
"""
import io, os, sys, uuid
import pandas as pd, psycopg2

src = sys.argv[1] if len(sys.argv) > 1 else "."
conn = psycopg2.connect(os.environ["DATABASE_URL"])
cur = conn.cursor()
U = lambda: str(uuid.uuid4())

def copy(table, cols, df):
    buf = io.StringIO(); df[cols].to_csv(buf, index=False, header=False, na_rep="\\N")
    buf.seek(0)
    cur.copy_expert(f"COPY {table} ({','.join(cols)}) FROM STDIN WITH (FORMAT csv, NULL '\\N')", buf)

parts = pd.read_csv(f"{src}/parts_master.csv")
pos = pd.read_csv(f"{src}/purchase_orders.csv")
qi = pd.read_csv(f"{src}/quality_incidents.csv")
hist = pd.read_csv(f"{src}/supply_chain_history.csv")

# warehouses + bins
sites = sorted(hist.site_id.unique())
wh = pd.DataFrame({"id": [U() for _ in sites], "code": sites, "name": [f"Site {s}" for s in sites]})
copy("warehouses", ["id", "code", "name"], wh)
bins = pd.DataFrame({"id": [U() for _ in sites], "warehouse_id": wh.id, "code": "MAIN"})
copy("locations", ["id", "warehouse_id", "code"], bins)
wh_of, bin_of = dict(zip(sites, wh.id)), dict(zip(sites, bins.id))

# products
parts["id"] = [U() for _ in range(len(parts))]
p = pd.DataFrame({"id": parts.id, "sku": parts.part_id, "name": parts.part_id + " " + parts.part_family,
                  "unit_cost": parts.unit_cost, "list_price": parts.unit_cost,
                  "criticality_class": parts.criticality_class, "lead_time_days": parts.lead_time_days,
                  "shelf_life_days": parts.shelf_life_days.astype("Int64"),
                  "is_repairable": parts.is_repairable.eq("Yes")})
copy("products", list(p.columns), p)
prod_of = dict(zip(parts.part_id, parts.id))

# suppliers
sups = sorted(set(pos.supplier_id) | set(parts.supplier_id_primary))
sd = pd.DataFrame({"id": [U() for _ in sups], "code": sups, "name": sups})
copy("suppliers", ["id", "code", "name"], sd)
sup_of = dict(zip(sups, sd.id))

# purchase orders / lines / receipts
pos["po_uuid"], pos["line_uuid"] = [U() for _ in range(len(pos))], [U() for _ in range(len(pos))]
copy("purchase_orders", ["id", "po_number", "supplier_id", "warehouse_id", "status", "expected_date", "order_date"],
     pd.DataFrame({"id": pos.po_uuid, "po_number": pos.po_id, "supplier_id": pos.supplier_id.map(sup_of),
                   "warehouse_id": pos.site_id.map(wh_of),
                   "status": (pos.received_qty >= pos.ordered_qty).map({True: "received", False: "closed_short"}),
                   "expected_date": pos.promised_date, "order_date": pos.order_date}))
copy("purchase_order_lines", ["id", "purchase_order_id", "product_id", "quantity_ordered", "quantity_received", "unit_cost", "promised_date"],
     pd.DataFrame({"id": pos.line_uuid, "purchase_order_id": pos.po_uuid, "product_id": pos.part_id.map(prod_of),
                   "quantity_ordered": pos.ordered_qty, "quantity_received": pos.received_qty,
                   "unit_cost": pos.part_id.map(dict(zip(parts.part_id, parts.unit_cost))), "promised_date": pos.promised_date}))
rc = pos[pos.received_qty > 0]
copy("goods_receipts", ["id", "purchase_order_id", "purchase_order_line_id", "product_id", "location_id", "quantity", "received_on"],
     pd.DataFrame({"id": [U() for _ in range(len(rc))], "purchase_order_id": rc.po_uuid, "purchase_order_line_id": rc.line_uuid,
                   "product_id": rc.part_id.map(prod_of), "location_id": rc.site_id.map(bin_of),
                   "quantity": rc.received_qty, "received_on": rc.receipt_date}))

# quality incidents (scrapped on receipt: no live stock effect)
copy("quality_incidents", ["id", "supplier_id", "product_id", "severity", "defect_type", "quarantined_qty", "scrapped_qty", "status", "occurred_on"],
     pd.DataFrame({"id": [U() for _ in range(len(qi))], "supplier_id": qi.supplier_id.map(sup_of),
                   "product_id": qi.part_id.map(prod_of), "severity": qi.defect_severity.str.lower(),
                   "defect_type": qi.defect_type, "quarantined_qty": qi.scrap_qty, "scrapped_qty": qi.scrap_qty,
                   "status": "closed", "occurred_on": qi.incident_date}))

# consumption -> issue ledger (zero weeks are simply absent)
c = hist[hist.consumption_qty > 0]
copy("stock_movements", ["id", "product_id", "location_id", "movement_type", "quantity_delta", "created_at"],
     pd.DataFrame({"id": [U() for _ in range(len(c))], "product_id": c.part_id.map(prod_of),
                   "location_id": c.site_id.map(bin_of), "movement_type": "issue",
                   "quantity_delta": -c.consumption_qty, "created_at": c.date + " 12:00:00+00"}))
copy("demand_forecasts", ["id", "warehouse_id", "product_id", "period_start", "forecast_type", "forecast_qty", "uplift_pct"],
     pd.DataFrame({"id": [U() for _ in range(len(hist))], "warehouse_id": hist.site_id.map(wh_of),
                   "product_id": hist.part_id.map(prod_of), "period_start": hist.date,
                   "forecast_type": hist.forecast_type.str.lower(), "forecast_qty": hist.forecast_qty,
                   "uplift_pct": hist.forecast_uplift_pct}))
conn.commit()
print("imported:", dict(warehouses=len(wh), products=len(p), suppliers=len(sd), purchase_orders=len(pos),
                        receipts=len(rc), incidents=len(qi), issues=len(c), forecasts=len(hist)))
