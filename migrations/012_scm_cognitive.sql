-- v5: cognitive layer (CIR-GNN) ------------------------------------------------------------------------

-- trained Connectome-Informed Recurrent GNNs (self-contained artifact: wiring, delays, weights, normalisation)
CREATE TABLE cognitive_models (
    id          UUID PRIMARY KEY,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    as_of       DATE NOT NULL,
    weeks       INTEGER NOT NULL,
    n_nodes     INTEGER NOT NULL,
    fingerprint TEXT NOT NULL,
    active      BOOLEAN NOT NULL DEFAULT TRUE,
    metrics     JSONB NOT NULL,
    artifact    JSONB NOT NULL
);
CREATE INDEX idx_cog_models_active ON cognitive_models(active, created_at DESC);

-- recurrent memory of every node after replaying its history, plus encoder scores
CREATE TABLE cognitive_node_state (
    model_id UUID NOT NULL REFERENCES cognitive_models(id) ON DELETE CASCADE,
    node_id  UUID NOT NULL,
    kind     TEXT NOT NULL,
    code     TEXT NOT NULL,
    state    JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (model_id, node_id)
);

-- every prediction is stored so it can be scored against what actually happens
CREATE TABLE cognitive_predictions (
    id          UUID PRIMARY KEY,
    model_id    UUID NOT NULL REFERENCES cognitive_models(id) ON DELETE CASCADE,
    node_id     UUID NOT NULL,
    kind        TEXT NOT NULL,
    channel     TEXT NOT NULL CHECK (channel IN ('volume','delay','quality','pressure')),
    horizon     INTEGER NOT NULL,
    target_week DATE NOT NULL,
    predicted   DOUBLE PRECISION NOT NULL,
    prob        DOUBLE PRECISION,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    actual      DOUBLE PRECISION,
    error       DOUBLE PRECISION,
    reconciled_at TIMESTAMPTZ,
    UNIQUE (model_id, node_id, channel, horizon, target_week)
);
CREATE INDEX idx_cog_pred_open ON cognitive_predictions(target_week) WHERE actual IS NULL;

-- autonomous-execution log: proposals, approvals and results (every action is auditable)
CREATE TABLE cognitive_actions (
    id          UUID PRIMARY KEY,
    kind        TEXT NOT NULL CHECK (kind IN ('create_po','transfer_stock','adjust_reorder_policy')),
    status      TEXT NOT NULL DEFAULT 'proposed'
                CHECK (status IN ('proposed','approved','executed','rejected','failed','blocked')),
    payload     JSONB NOT NULL,
    rationale   JSONB NOT NULL DEFAULT '{}'::jsonb,
    result      JSONB,
    model_id    UUID REFERENCES cognitive_models(id) ON DELETE SET NULL,
    created_by  TEXT NOT NULL DEFAULT 'cognitive',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    decided_at  TIMESTAMPTZ,
    executed_at TIMESTAMPTZ
);
CREATE INDEX idx_cog_actions_status ON cognitive_actions(status, created_at DESC);

-- autonomy level and guardrails per action kind; default is advisory (nothing runs without a person)
CREATE TABLE cognitive_policies (
    action_kind TEXT PRIMARY KEY CHECK (action_kind IN ('create_po','transfer_stock','adjust_reorder_policy','learning')),
    autonomy    TEXT NOT NULL DEFAULT 'advisory' CHECK (autonomy IN ('advisory','supervised','autonomous')),
    params      JSONB NOT NULL DEFAULT '{}'::jsonb,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
INSERT INTO cognitive_policies (action_kind, autonomy, params) VALUES
  ('create_po', 'advisory', '{"max_order_value": 50000, "max_supplier_risk": 0.5, "max_share_per_supplier": 0.7, "submit": false}'),
  ('transfer_stock', 'advisory', '{"max_units": 5000}'),
  ('adjust_reorder_policy', 'advisory', '{"max_change_pct": 50}'),
  ('learning', 'advisory', '{"service_z_mult": 1.0, "risk_penalty": 1.0}');

-- external signals (weather, markets, IoT, port congestion, ...) feeding risk and the digital twin
CREATE TABLE cognitive_signals (
    id          UUID PRIMARY KEY,
    signal_type TEXT NOT NULL,
    node_id     UUID,
    value       DOUBLE PRECISION NOT NULL,
    unit        TEXT,
    note        TEXT,
    observed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_cog_signals ON cognitive_signals(signal_type, observed_at DESC);

CREATE TABLE cognitive_learning_events (
    id         UUID PRIMARY KEY,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    model_id   UUID,
    metrics    JSONB NOT NULL,
    decision   TEXT NOT NULL
);
