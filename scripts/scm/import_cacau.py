#!/usr/bin/env python3
"""Load the Cacau Prime Foods (2023-2024) dataset into PostgreSQL.

    DATABASE_URL=postgres://... python3 scripts/import_cacau.py /path/to/cacauprime_dataset_2023_2024

Production runs (scrap, hours, cost vs the product's standard cost), purchases as POs with receipts
(supplier master lead time kept for the accuracy report) and deliveries as shipments with carriers.
"""
import io, os, sys, uuid
import pandas as pd, psycopg2

src = sys.argv[1]
conn = psycopg2.connect(os.environ["DATABASE_URL"]); cur = conn.cursor()
U = lambda: str(uuid.uuid4())
def copy(table, df):
    buf = io.StringIO(); df.to_csv(buf, index=False, header=False, na_rep="\\N"); buf.seek(0)
    cur.copy_expert(f"COPY {table} ({','.join(df.columns)}) FROM STDIN WITH (FORMAT csv, NULL '\\N')", buf)
rd = lambda f: pd.read_csv(f"{src}/{f}.csv")
dt = lambda s: pd.to_datetime(s.astype(str), format="%Y%m%d")

pr, lg, cp, dp, fo, fi = rd("fato_producao"), rd("fato_logistica"), rd("fato_compras"), rd("dim_produto"), rd("dim_fornecedor"), rd("dim_filial")
wh = {f: U() for f in fi.id_filial}; loc = {f: U() for f in fi.id_filial}
copy("warehouses", pd.DataFrame({"id": fi.id_filial.map(wh), "code": fi.id_filial, "name": fi.nome_filial}))
copy("locations", pd.DataFrame({"id": fi.id_filial.map(loc), "warehouse_id": fi.id_filial.map(wh), "code": "MAIN"}))
pid = {p: U() for p in dp.id_produto}
copy("products", pd.DataFrame({"id": dp.id_produto.map(pid), "sku": dp.id_produto, "name": dp.nome_produto, "unit_cost": dp.custo_medio.round(4), "list_price": dp.preco_base.round(4)}))
sid = {s: U() for s in fo.id_fornecedor}
copy("suppliers", pd.DataFrame({"id": fo.id_fornecedor.map(sid), "code": fo.id_fornecedor, "name": fo.nome_fornecedor, "country": fo.pais, "lead_time_days": fo.prazo_medio_dias.astype(int)}))

# production runs: quantity_started = qtde_produzida, scrap from the scrap percentage.
# Runs with zero quantity (a data-quality issue in the source) are skipped.
zero = int((pr.qtde_produzida <= 0).sum()); pr = pr[pr.qtde_produzida > 0].reset_index(drop=True)
scrap = (pr.qtde_produzida * pr.taxa_sucata_pct / 100).round().astype(int)
copy("production_runs", pd.DataFrame({"id": [U() for _ in range(len(pr))], "product_id": pr.id_produto.map(pid), "location_id": pr.id_filial.map(loc),
     "quantity_started": pr.qtde_produzida, "quantity_good": pr.qtde_produzida - scrap, "quantity_scrap": scrap,
     "hours": pr.tempo_producao_horas, "cost": pr.custo_producao.round(4), "produced_on": dt(pr.id_tempo).dt.date}))

# purchases: raw materials become products; the receipt date is order date + delivery days
raw = sorted(cp.item_comprado.unique()); rid = {r: U() for r in raw}
copy("products", pd.DataFrame({"id": [rid[r] for r in raw], "sku": ["RM-" + r.upper().replace(" ", "_") for r in raw], "name": raw}))
cp["po"], cp["ln"] = [U() for _ in range(len(cp))], [U() for _ in range(len(cp))]
od = dt(cp.id_tempo); cp["rcv"] = od + pd.to_timedelta(cp.prazo_entrega_dias, unit="D")
copy("purchase_orders", pd.DataFrame({"id": cp.po, "po_number": "CP-" + cp.id_compra.astype(str), "supplier_id": cp.id_fornecedor.map(sid),
     "warehouse_id": cp.id_filial.map(wh), "status": "received", "order_date": od.dt.date}))
copy("purchase_order_lines", pd.DataFrame({"id": cp.ln, "purchase_order_id": cp.po, "product_id": cp.item_comprado.map(rid), "quantity_ordered": cp.quantidade.astype(int),
     "quantity_received": cp.quantidade.astype(int), "unit_cost": (cp.valor_compra / cp.quantidade).round(4)}))
copy("goods_receipts", pd.DataFrame({"id": [U() for _ in range(len(cp))], "purchase_order_id": cp.po, "purchase_order_line_id": cp.ln, "product_id": cp.item_comprado.map(rid),
     "location_id": cp.id_filial.map(loc), "quantity": cp.quantidade.astype(int), "received_on": cp.rcv.dt.date}))

# deliveries -> shipments under one placeholder order
cust, so = U(), U()
copy("scm_customers", pd.DataFrame({"id": [cust], "code": ["CACAU"], "name": ["All customers"]}))
copy("sales_orders", pd.DataFrame({"id": [so], "so_number": ["CACAU-DELIVERIES"], "customer_id": [cust], "warehouse_id": [wh[fi.id_filial.iloc[0]]], "status": ["shipped"]}))
cars = sorted(lg.transportadora.unique()); cid = {c: U() for c in cars}
copy("carriers", pd.DataFrame({"id": [cid[c] for c in cars], "code": [c.upper().replace(" ", "_") for c in cars], "name": cars}))
sd = dt(lg.id_tempo)
copy("shipments", pd.DataFrame({"id": [U() for _ in range(len(lg))], "shipment_number": "CD-" + lg.id_entrega.astype(str), "sales_order_id": so,
     "carrier_id": lg.transportadora.map(cid), "status": "delivered", "shipped_at": sd.dt.strftime("%Y-%m-%d") + " 08:00:00+00",
     "delivered_at": (sd + pd.to_timedelta(lg.prazo_real_dias, unit="D")).dt.strftime("%Y-%m-%d") + " 08:00:00+00",
     "expected_delivery_date": (sd + pd.to_timedelta(lg.prazo_previsto_dias, unit="D")).dt.date, "freight_cost": lg.custo_transporte.round(4)}))
conn.commit()
print("skipped", zero, "zero-quantity production runs; imported", len(pr), "production runs,", len(cp), "purchases,", len(lg), "deliveries,", len(cars), "carriers")
