-- Risk module: counterparty credit profiles (customers and vendors).
CREATE TABLE IF NOT EXISTS counterparty_risk_profiles (
    id          BIGSERIAL PRIMARY KEY,
    party_kind  VARCHAR(10) NOT NULL CHECK (party_kind IN ('customer','vendor')),
    party_id    BIGINT      NOT NULL,
    credit_score INT        NOT NULL CHECK (credit_score BETWEEN 300 AND 850),
    debt_ratio  NUMERIC(8,3) NOT NULL CHECK (debt_ratio >= 0),
    liquidity   NUMERIC(8,3) NOT NULL CHECK (liquidity  >= 0),
    legal_cases INT         NOT NULL DEFAULT 0 CHECK (legal_cases >= 0),   -- informational; not scored
    is_bankrupt BOOLEAN     NOT NULL DEFAULT FALSE,
    assessed_on DATE        NOT NULL DEFAULT CURRENT_DATE,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (party_kind, party_id)
);
