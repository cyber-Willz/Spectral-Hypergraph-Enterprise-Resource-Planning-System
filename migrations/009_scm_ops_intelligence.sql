-- v2: dataset-driven improvements ---------------------------------------------------------------

-- Products: criticality (aerospace A/B/C), storage class (pharma cold chain), shelf life,
-- review cadence (pharma weekly/monthly/quarterly restocking), planning parameters.
ALTER TABLE products
  ADD COLUMN criticality_class TEXT NOT NULL DEFAULT 'C' CHECK (criticality_class IN ('A','B','C')),
  ADD COLUMN storage_class TEXT NOT NULL DEFAULT 'ambient'
      CHECK (storage_class IN ('ambient','cold','frozen','controlled')),
  ADD COLUMN shelf_life_days INTEGER CHECK (shelf_life_days IS NULL OR shelf_life_days > 0),
  ADD COLUMN is_repairable BOOLEAN NOT NULL DEFAULT FALSE,
  ADD COLUMN review_period_days INTEGER NOT NULL DEFAULT 7 CHECK (review_period_days > 0),
  ADD COLUMN lead_time_days INTEGER NOT NULL DEFAULT 14 CHECK (lead_time_days >= 0),
  ADD COLUMN holding_cost_rate NUMERIC(6,4) NOT NULL DEFAULT 0.25 CHECK (holding_cost_rate >= 0),
  ADD COLUMN stockout_cost NUMERIC(14,4) CHECK (stockout_cost IS NULL OR stockout_cost >= 0);

ALTER TABLE locations
  ADD COLUMN storage_class TEXT NOT NULL DEFAULT 'ambient'
      CHECK (storage_class IN ('ambient','cold','frozen','controlled'));

-- Suppliers: inputs of the structural risk model + commercial attributes
ALTER TABLE suppliers
  ADD COLUMN country TEXT,
  ADD COLUMN region TEXT,
  ADD COLUMN tier INTEGER CHECK (tier IS NULL OR tier BETWEEN 1 AND 3),
  ADD COLUMN esg_score DOUBLE PRECISION CHECK (esg_score IS NULL OR esg_score BETWEEN 0 AND 100),
  ADD COLUMN financial_stability_score DOUBLE PRECISION
      CHECK (financial_stability_score IS NULL OR financial_stability_score BETWEEN 0 AND 100),
  ADD COLUMN geopolitical_risk_index DOUBLE PRECISION
      CHECK (geopolitical_risk_index IS NULL OR geopolitical_risk_index BETWEEN 0 AND 100),
  ADD COLUMN alternatives_available INTEGER CHECK (alternatives_available IS NULL OR alternatives_available >= 0),
  ADD COLUMN environmental_compliance DOUBLE PRECISION
      CHECK (environmental_compliance IS NULL OR environmental_compliance BETWEEN 0 AND 100),
  ADD COLUMN previous_disruptions INTEGER CHECK (previous_disruptions IS NULL OR previous_disruptions >= 0),
  ADD COLUMN payment_terms_days INTEGER NOT NULL DEFAULT 30 CHECK (payment_terms_days >= 0);

-- Stock: quarantine (blocked) quantity and lot expiry
ALTER TABLE stock_levels
  ADD COLUMN blocked BIGINT NOT NULL DEFAULT 0 CHECK (blocked >= 0),
  ADD COLUMN expiry_date DATE,
  ADD CONSTRAINT stock_levels_reserved_blocked_check CHECK (reserved + blocked <= quantity);
CREATE INDEX idx_stock_levels_expiry ON stock_levels(expiry_date) WHERE expiry_date IS NOT NULL;

-- Contracts ----------------------------------------------------------------------------------------
CREATE TABLE contracts (
    id              UUID PRIMARY KEY,
    supplier_id     UUID NOT NULL REFERENCES suppliers(id),
    contract_number TEXT NOT NULL UNIQUE,
    contract_type   TEXT NOT NULL DEFAULT 'framework'
                    CHECK (contract_type IN ('framework','long_term','master_supply','spot')),
    start_date      DATE NOT NULL,
    end_date        DATE NOT NULL,
    currency        TEXT NOT NULL DEFAULT 'USD',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (end_date >= start_date)
);

-- Purchase orders ---------------------------------------------------------------------------------------
ALTER TABLE purchase_orders
  ADD COLUMN order_date DATE NOT NULL DEFAULT CURRENT_DATE,
  ADD COLUMN po_type TEXT NOT NULL DEFAULT 'standard'
      CHECK (po_type IN ('standard','blanket','contract','emergency')),
  ADD COLUMN currency TEXT NOT NULL DEFAULT 'USD',
  ADD COLUMN payment_terms_days INTEGER NOT NULL DEFAULT 30 CHECK (payment_terms_days >= 0),
  ADD COLUMN contract_id UUID REFERENCES contracts(id);
ALTER TABLE purchase_orders DROP CONSTRAINT purchase_orders_status_check;
ALTER TABLE purchase_orders ADD CONSTRAINT purchase_orders_status_check
  CHECK (status IN ('draft','submitted','partially_received','received','closed_short','cancelled'));

ALTER TABLE purchase_order_lines
  ADD COLUMN promised_date DATE,
  ADD COLUMN budget_unit_cost NUMERIC(14,4) CHECK (budget_unit_cost IS NULL OR budget_unit_cost >= 0);

-- One row per physical receipt: enables on-time / in-full / lead-time statistics
CREATE TABLE goods_receipts (
    id                    UUID PRIMARY KEY,
    purchase_order_id     UUID NOT NULL REFERENCES purchase_orders(id),
    purchase_order_line_id UUID NOT NULL REFERENCES purchase_order_lines(id),
    product_id            UUID NOT NULL REFERENCES products(id),
    location_id           UUID NOT NULL REFERENCES locations(id),
    lot_number            TEXT NOT NULL DEFAULT '',
    quantity              BIGINT NOT NULL CHECK (quantity > 0),
    received_on           DATE NOT NULL DEFAULT CURRENT_DATE,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_gr_line ON goods_receipts(purchase_order_line_id);
CREATE INDEX idx_gr_product ON goods_receipts(product_id);

-- Quality ----------------------------------------------------------------------------------------------------
CREATE TABLE quality_incidents (
    id                UUID PRIMARY KEY,
    supplier_id       UUID REFERENCES suppliers(id),
    product_id        UUID NOT NULL REFERENCES products(id),
    purchase_order_id UUID REFERENCES purchase_orders(id),
    stock_level_id    UUID REFERENCES stock_levels(id),
    severity          TEXT NOT NULL CHECK (severity IN ('minor','major','critical')),
    defect_type       TEXT,
    quarantined_qty   BIGINT NOT NULL DEFAULT 0 CHECK (quarantined_qty >= 0),
    released_qty      BIGINT NOT NULL DEFAULT 0 CHECK (released_qty >= 0),
    scrapped_qty      BIGINT NOT NULL DEFAULT 0 CHECK (scrapped_qty >= 0),
    status            TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open','closed')),
    note              TEXT,
    occurred_on       DATE NOT NULL DEFAULT CURRENT_DATE,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (released_qty + scrapped_qty <= quarantined_qty)
);
CREATE INDEX idx_qi_supplier ON quality_incidents(supplier_id);

-- Forecasts --------------------------------------------------------------------------------------------------
CREATE TABLE demand_forecasts (
    id            UUID PRIMARY KEY,
    warehouse_id  UUID NOT NULL REFERENCES warehouses(id),
    product_id    UUID NOT NULL REFERENCES products(id),
    period_start  DATE NOT NULL,
    period_days   INTEGER NOT NULL DEFAULT 7 CHECK (period_days > 0),
    forecast_type TEXT NOT NULL DEFAULT 'baseline' CHECK (forecast_type IN ('baseline','adjusted')),
    forecast_qty  DOUBLE PRECISION NOT NULL CHECK (forecast_qty >= 0),
    uplift_pct    DOUBLE PRECISION,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (warehouse_id, product_id, period_start, forecast_type)
);

-- Supplier invoices (3-way match) ----------------------------------------------------------------------
CREATE TABLE supplier_invoices (
    id                UUID PRIMARY KEY,
    purchase_order_id UUID NOT NULL REFERENCES purchase_orders(id),
    invoice_number    TEXT NOT NULL,
    invoice_date      DATE NOT NULL DEFAULT CURRENT_DATE,
    due_date          DATE NOT NULL,
    currency          TEXT NOT NULL,
    total             NUMERIC(16,4) NOT NULL CHECK (total >= 0),
    status            TEXT NOT NULL CHECK (status IN ('matched','disputed','paid')),
    match_notes       TEXT,
    paid_on           DATE,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (purchase_order_id, invoice_number)
);
CREATE TABLE supplier_invoice_lines (
    id                     UUID PRIMARY KEY,
    invoice_id             UUID NOT NULL REFERENCES supplier_invoices(id) ON DELETE CASCADE,
    purchase_order_line_id UUID NOT NULL REFERENCES purchase_order_lines(id),
    quantity               BIGINT NOT NULL CHECK (quantity > 0),
    unit_price             NUMERIC(14,4) NOT NULL CHECK (unit_price >= 0)
);
CREATE INDEX idx_sil_line ON supplier_invoice_lines(purchase_order_line_id);
