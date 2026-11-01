-- v4: dataset-driven operations (MOQ, landed cost, shipment risk, inspection, production) -------------

-- Supplier order rules (ERP data: 86% of orders sit exactly at the MOQ)
ALTER TABLE product_suppliers
  ADD COLUMN min_order_qty BIGINT NOT NULL DEFAULT 1 CHECK (min_order_qty >= 1),
  ADD COLUMN order_multiple BIGINT NOT NULL DEFAULT 1 CHECK (order_multiple >= 1);

-- Acceptable quality level for incoming inspection (percent defective)
ALTER TABLE products
  ADD COLUMN aql_defect_pct DOUBLE PRECISION CHECK (aql_defect_pct IS NULL OR aql_defect_pct BETWEEN 0 AND 100);
ALTER TABLE goods_receipts
  ADD COLUMN defect_rate_pct DOUBLE PRECISION CHECK (defect_rate_pct IS NULL OR defect_rate_pct BETWEEN 0 AND 100);

-- Freight, insurance and incoterm on inbound orders (SCMS data: freight is booked per shipment, not per line)
ALTER TABLE purchase_orders
  ADD COLUMN incoterm TEXT CHECK (incoterm IS NULL OR incoterm IN
      ('EXW','FCA','FAS','FOB','CFR','CIF','CPT','CIP','DAP','DPU','DDP','DDU')),
  ADD COLUMN transport_mode TEXT CHECK (transport_mode IS NULL OR transport_mode IN
      ('air','air_charter','sea','rail','road','courier','multimodal')),
  ADD COLUMN freight_cost NUMERIC(14,4) NOT NULL DEFAULT 0 CHECK (freight_cost >= 0),
  ADD COLUMN insurance_cost NUMERIC(14,4) NOT NULL DEFAULT 0 CHECK (insurance_cost >= 0),
  ADD COLUMN freight_included BOOLEAN NOT NULL DEFAULT FALSE;

-- Outbound shipments: promised date, cost and predicted risk
ALTER TABLE shipments
  ADD COLUMN expected_delivery_date DATE,
  ADD COLUMN freight_cost NUMERIC(14,4) NOT NULL DEFAULT 0 CHECK (freight_cost >= 0),
  ADD COLUMN delay_risk DOUBLE PRECISION CHECK (delay_risk IS NULL OR delay_risk BETWEEN 0 AND 1),
  ADD COLUMN disruption_risk DOUBLE PRECISION CHECK (disruption_risk IS NULL OR disruption_risk BETWEEN 0 AND 1),
  ADD COLUMN risk_context JSONB;
CREATE INDEX idx_shipments_carrier ON shipments(carrier_id);

-- Manufacturing runs: good output enters stock, scrap is recorded (Cacau Prime data)
CREATE TABLE production_runs (
    id               UUID PRIMARY KEY,
    product_id       UUID NOT NULL REFERENCES products(id),
    location_id      UUID NOT NULL REFERENCES locations(id),
    lot_number       TEXT NOT NULL DEFAULT '',
    quantity_started BIGINT NOT NULL CHECK (quantity_started > 0),
    quantity_good    BIGINT NOT NULL CHECK (quantity_good >= 0),
    quantity_scrap   BIGINT NOT NULL CHECK (quantity_scrap >= 0),
    hours            DOUBLE PRECISION NOT NULL DEFAULT 0 CHECK (hours >= 0),
    cost             NUMERIC(14,4) NOT NULL DEFAULT 0 CHECK (cost >= 0),
    produced_on      DATE NOT NULL DEFAULT CURRENT_DATE,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (quantity_good + quantity_scrap = quantity_started)
);
CREATE INDEX idx_prod_runs_product ON production_runs(product_id, produced_on);
