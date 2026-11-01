-- Master data -----------------------------------------------------------
CREATE TABLE warehouses (
    id          UUID PRIMARY KEY,
    code        TEXT NOT NULL UNIQUE,
    name        TEXT NOT NULL,
    address     TEXT,
    city        TEXT,
    country     TEXT,
    active      BOOLEAN NOT NULL DEFAULT TRUE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE locations (
    id           UUID PRIMARY KEY,
    warehouse_id UUID NOT NULL REFERENCES warehouses(id),
    code         TEXT NOT NULL,
    zone         TEXT,
    capacity     BIGINT CHECK (capacity IS NULL OR capacity >= 0),
    active       BOOLEAN NOT NULL DEFAULT TRUE,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (warehouse_id, code)
);

CREATE TABLE products (
    id              UUID PRIMARY KEY,
    sku             TEXT NOT NULL UNIQUE,
    name            TEXT NOT NULL,
    description     TEXT,
    unit_of_measure TEXT NOT NULL DEFAULT 'each',
    unit_cost       NUMERIC(14,4) NOT NULL DEFAULT 0 CHECK (unit_cost >= 0),
    list_price      NUMERIC(14,4) NOT NULL DEFAULT 0 CHECK (list_price >= 0),
    reorder_point   BIGINT NOT NULL DEFAULT 0 CHECK (reorder_point >= 0),
    reorder_qty     BIGINT NOT NULL DEFAULT 0 CHECK (reorder_qty >= 0),
    active          BOOLEAN NOT NULL DEFAULT TRUE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE suppliers (
    id             UUID PRIMARY KEY,
    code           TEXT NOT NULL UNIQUE,
    name           TEXT NOT NULL,
    contact_email  TEXT,
    phone          TEXT,
    lead_time_days INTEGER NOT NULL DEFAULT 7 CHECK (lead_time_days >= 0),
    active         BOOLEAN NOT NULL DEFAULT TRUE,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE scm_customers (
    id         UUID PRIMARY KEY,
    code       TEXT NOT NULL UNIQUE,
    name       TEXT NOT NULL,
    email      TEXT,
    address    TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE carriers (
    id                    UUID PRIMARY KEY,
    code                  TEXT NOT NULL UNIQUE,
    name                  TEXT NOT NULL,
    tracking_url_template TEXT,
    active                BOOLEAN NOT NULL DEFAULT TRUE
);

-- Inventory -------------------------------------------------------------
CREATE TABLE stock_levels (
    id          UUID PRIMARY KEY,
    product_id  UUID NOT NULL REFERENCES products(id),
    location_id UUID NOT NULL REFERENCES locations(id),
    lot_number  TEXT NOT NULL DEFAULT '',
    quantity    BIGINT NOT NULL DEFAULT 0 CHECK (quantity >= 0),
    reserved    BIGINT NOT NULL DEFAULT 0 CHECK (reserved >= 0),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (product_id, location_id, lot_number),
    CHECK (reserved <= quantity)
);
CREATE INDEX idx_stock_levels_location ON stock_levels(location_id);

-- Append-only ledger of every on-hand change
CREATE TABLE stock_movements (
    id             UUID PRIMARY KEY,
    product_id     UUID NOT NULL REFERENCES products(id),
    location_id    UUID NOT NULL REFERENCES locations(id),
    lot_number     TEXT NOT NULL DEFAULT '',
    movement_type  TEXT NOT NULL,
    quantity_delta BIGINT NOT NULL CHECK (quantity_delta <> 0),
    reference_type TEXT,
    reference_id   UUID,
    note           TEXT,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_stock_movements_product ON stock_movements(product_id, created_at DESC);
CREATE INDEX idx_stock_movements_location ON stock_movements(location_id, created_at DESC);
CREATE INDEX idx_stock_movements_ref ON stock_movements(reference_type, reference_id);

-- Procurement -----------------------------------------------------------
CREATE TABLE purchase_orders (
    id            UUID PRIMARY KEY,
    po_number     TEXT NOT NULL UNIQUE,
    supplier_id   UUID NOT NULL REFERENCES suppliers(id),
    warehouse_id  UUID NOT NULL REFERENCES warehouses(id),
    status        TEXT NOT NULL DEFAULT 'draft'
                  CHECK (status IN ('draft','submitted','partially_received','received','cancelled')),
    expected_date DATE,
    notes         TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_po_status ON purchase_orders(status);

CREATE TABLE purchase_order_lines (
    id                UUID PRIMARY KEY,
    purchase_order_id UUID NOT NULL REFERENCES purchase_orders(id) ON DELETE CASCADE,
    product_id        UUID NOT NULL REFERENCES products(id),
    quantity_ordered  BIGINT NOT NULL CHECK (quantity_ordered > 0),
    quantity_received BIGINT NOT NULL DEFAULT 0 CHECK (quantity_received >= 0),
    unit_cost         NUMERIC(14,4) NOT NULL CHECK (unit_cost >= 0),
    CHECK (quantity_received <= quantity_ordered)
);
CREATE INDEX idx_pol_order ON purchase_order_lines(purchase_order_id);

-- Fulfilment ------------------------------------------------------------
CREATE TABLE sales_orders (
    id           UUID PRIMARY KEY,
    so_number    TEXT NOT NULL UNIQUE,
    customer_id  UUID NOT NULL REFERENCES scm_customers(id),
    warehouse_id UUID NOT NULL REFERENCES warehouses(id),
    status       TEXT NOT NULL DEFAULT 'draft'
                 CHECK (status IN ('draft','confirmed','partially_allocated','backordered',
                                   'partially_shipped','shipped','cancelled')),
    ship_to      TEXT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_so_status ON sales_orders(status);

CREATE TABLE sales_order_lines (
    id                 UUID PRIMARY KEY,
    sales_order_id     UUID NOT NULL REFERENCES sales_orders(id) ON DELETE CASCADE,
    product_id         UUID NOT NULL REFERENCES products(id),
    quantity           BIGINT NOT NULL CHECK (quantity > 0),
    quantity_allocated BIGINT NOT NULL DEFAULT 0 CHECK (quantity_allocated >= 0),
    quantity_shipped   BIGINT NOT NULL DEFAULT 0 CHECK (quantity_shipped >= 0),
    unit_price         NUMERIC(14,4) NOT NULL CHECK (unit_price >= 0),
    CHECK (quantity_allocated + quantity_shipped <= quantity)
);
CREATE INDEX idx_sol_order ON sales_order_lines(sales_order_id);

CREATE TABLE shipments (
    id              UUID PRIMARY KEY,
    shipment_number TEXT NOT NULL UNIQUE,
    sales_order_id  UUID NOT NULL REFERENCES sales_orders(id),
    carrier_id      UUID REFERENCES carriers(id),
    tracking_number TEXT,
    status          TEXT NOT NULL DEFAULT 'created'
                    CHECK (status IN ('created','in_transit','out_for_delivery','delivered','exception')),
    shipped_at      TIMESTAMPTZ,
    delivered_at    TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_shipments_order ON shipments(sales_order_id);
CREATE INDEX idx_shipments_tracking ON shipments(tracking_number);

-- Stock reserved for a sales-order line, tied to the exact stock level (bin + lot)
CREATE TABLE allocations (
    id                  UUID PRIMARY KEY,
    sales_order_line_id UUID NOT NULL REFERENCES sales_order_lines(id) ON DELETE CASCADE,
    stock_level_id      UUID NOT NULL REFERENCES stock_levels(id),
    quantity            BIGINT NOT NULL CHECK (quantity > 0),
    status              TEXT NOT NULL DEFAULT 'reserved' CHECK (status IN ('reserved','shipped','released')),
    shipment_id         UUID REFERENCES shipments(id),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_alloc_line ON allocations(sales_order_line_id, status);
CREATE INDEX idx_alloc_shipment ON allocations(shipment_id);

CREATE TABLE shipment_events (
    id          UUID PRIMARY KEY,
    shipment_id UUID NOT NULL REFERENCES shipments(id) ON DELETE CASCADE,
    status      TEXT NOT NULL,
    location    TEXT,
    description TEXT,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_shipment_events ON shipment_events(shipment_id, occurred_at);
