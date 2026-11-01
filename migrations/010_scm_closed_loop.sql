-- v3: closed-loop operations ---------------------------------------------------------------------

-- statistical forecasts produced by the built-in engine sit next to baseline/adjusted ones
ALTER TABLE demand_forecasts DROP CONSTRAINT demand_forecasts_forecast_type_check;
ALTER TABLE demand_forecasts ADD CONSTRAINT demand_forecasts_forecast_type_check
  CHECK (forecast_type IN ('baseline','adjusted','statistical'));

-- Qualified / approved sources per product (feeds auto-replenishment and resilience analysis)
CREATE TABLE product_suppliers (
    id              UUID PRIMARY KEY,
    product_id      UUID NOT NULL REFERENCES products(id) ON DELETE CASCADE,
    supplier_id     UUID NOT NULL REFERENCES suppliers(id),
    is_preferred    BOOLEAN NOT NULL DEFAULT FALSE,
    unit_cost       NUMERIC(14,4) CHECK (unit_cost IS NULL OR unit_cost >= 0),
    lead_time_days  INTEGER CHECK (lead_time_days IS NULL OR lead_time_days >= 0),
    -- target share of volume (0-1); NULL = unspecified
    allocation_share DOUBLE PRECISION CHECK (allocation_share IS NULL OR allocation_share BETWEEN 0 AND 1),
    active          BOOLEAN NOT NULL DEFAULT TRUE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (product_id, supplier_id)
);
CREATE INDEX idx_ps_supplier ON product_suppliers(supplier_id);

-- Orders raised by the replenishment engine are marked for audit
ALTER TABLE purchase_orders ADD COLUMN auto_generated BOOLEAN NOT NULL DEFAULT FALSE;

-- Pick -> pack -> stage before shipping
ALTER TABLE sales_orders
  ADD COLUMN fulfillment_stage TEXT NOT NULL DEFAULT 'none'
      CHECK (fulfillment_stage IN ('none','picked','packed','staged'));

-- Inspection at receipt: rejected units are quarantined straight away
ALTER TABLE goods_receipts
  ADD COLUMN rejected_qty BIGINT NOT NULL DEFAULT 0 CHECK (rejected_qty >= 0),
  ADD COLUMN quality_incident_id UUID REFERENCES quality_incidents(id),
  ADD CONSTRAINT goods_receipts_rejected_check CHECK (rejected_qty <= quantity);

-- Rework: quarantined units repaired and returned to available stock
ALTER TABLE quality_incidents
  ADD COLUMN reworked_qty BIGINT NOT NULL DEFAULT 0 CHECK (reworked_qty >= 0);
ALTER TABLE quality_incidents DROP CONSTRAINT quality_incidents_check;
ALTER TABLE quality_incidents ADD CONSTRAINT quality_incidents_resolution_check
  CHECK (released_qty + scrapped_qty + reworked_qty <= quarantined_qty);
