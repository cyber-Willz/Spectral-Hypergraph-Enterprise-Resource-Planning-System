-- ERP core schema. Idempotent: safe to run on every startup.

-- ───────────────────────────── General Ledger ─────────────────────────────
CREATE TABLE IF NOT EXISTS accounts (
    id          BIGSERIAL PRIMARY KEY,
    code        VARCHAR(20)  NOT NULL UNIQUE,
    name        VARCHAR(200) NOT NULL,
    account_type VARCHAR(20) NOT NULL
        CHECK (account_type IN ('asset','liability','equity','revenue','expense')),
    parent_id   BIGINT REFERENCES accounts(id),
    is_active   BOOLEAN      NOT NULL DEFAULT TRUE,
    created_at  TIMESTAMPTZ  NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS journal_entries (
    id                BIGSERIAL PRIMARY KEY,
    entry_date        DATE         NOT NULL,
    memo              TEXT         NOT NULL,
    source            VARCHAR(30)  NOT NULL DEFAULT 'manual',
    source_id         BIGINT,
    reverses_entry_id BIGINT REFERENCES journal_entries(id),
    created_at        TIMESTAMPTZ  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_journal_entries_reversal
    ON journal_entries (reverses_entry_id) WHERE reverses_entry_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS ix_journal_entries_date ON journal_entries (entry_date);

CREATE TABLE IF NOT EXISTS journal_lines (
    id         BIGSERIAL PRIMARY KEY,
    entry_id   BIGINT NOT NULL REFERENCES journal_entries(id),
    account_id BIGINT NOT NULL REFERENCES accounts(id),
    debit      NUMERIC(18,2) NOT NULL DEFAULT 0 CHECK (debit  >= 0),
    credit     NUMERIC(18,2) NOT NULL DEFAULT 0 CHECK (credit >= 0),
    memo       TEXT,
    CHECK ((debit = 0) <> (credit = 0))
);
CREATE INDEX IF NOT EXISTS ix_journal_lines_entry   ON journal_lines (entry_id);
CREATE INDEX IF NOT EXISTS ix_journal_lines_account ON journal_lines (account_id);

-- The ledger is append-only: corrections are made with reversing entries.
CREATE OR REPLACE FUNCTION forbid_ledger_mutation() RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION 'ledger records are immutable; post a reversing entry instead';
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_journal_entries_immutable ON journal_entries;
CREATE TRIGGER trg_journal_entries_immutable
    BEFORE UPDATE OR DELETE ON journal_entries
    FOR EACH ROW EXECUTE FUNCTION forbid_ledger_mutation();

DROP TRIGGER IF EXISTS trg_journal_lines_immutable ON journal_lines;
CREATE TRIGGER trg_journal_lines_immutable
    BEFORE UPDATE OR DELETE ON journal_lines
    FOR EACH ROW EXECUTE FUNCTION forbid_ledger_mutation();

-- ───────────────────────────── Cash Management ─────────────────────────────
CREATE TABLE IF NOT EXISTS bank_accounts (
    id            BIGSERIAL PRIMARY KEY,
    name          VARCHAR(200) NOT NULL UNIQUE,
    account_mask  VARCHAR(8),               -- last digits only; never store full account numbers
    gl_account_id BIGINT NOT NULL REFERENCES accounts(id),
    currency      CHAR(3) NOT NULL DEFAULT 'USD',
    is_active     BOOLEAN NOT NULL DEFAULT TRUE,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS cash_transactions (
    id               BIGSERIAL PRIMARY KEY,
    bank_account_id  BIGINT NOT NULL REFERENCES bank_accounts(id),
    txn_date         DATE NOT NULL,
    amount           NUMERIC(18,2) NOT NULL CHECK (amount <> 0),   -- signed: + deposit, - withdrawal
    description      TEXT NOT NULL,
    source           VARCHAR(30) NOT NULL DEFAULT 'manual',
    source_id        BIGINT,
    journal_entry_id BIGINT REFERENCES journal_entries(id),
    reconciled       BOOLEAN NOT NULL DEFAULT FALSE,
    reconciled_on    DATE,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS ix_cash_transactions_bank ON cash_transactions (bank_account_id, txn_date);

-- ───────────────────────────── Accounts Payable ─────────────────────────────
CREATE TABLE IF NOT EXISTS vendors (
    id                 BIGSERIAL PRIMARY KEY,
    name               VARCHAR(200) NOT NULL UNIQUE,
    email              VARCHAR(255),
    phone              VARCHAR(50),
    payment_terms_days INT NOT NULL DEFAULT 30 CHECK (payment_terms_days BETWEEN 0 AND 365),
    is_active          BOOLEAN NOT NULL DEFAULT TRUE,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS bills (
    id                 BIGSERIAL PRIMARY KEY,
    vendor_id          BIGINT NOT NULL REFERENCES vendors(id),
    bill_number        VARCHAR(60) NOT NULL,
    bill_date          DATE NOT NULL,
    due_date           DATE NOT NULL,
    amount             NUMERIC(18,2) NOT NULL CHECK (amount > 0),
    amount_paid        NUMERIC(18,2) NOT NULL DEFAULT 0 CHECK (amount_paid >= 0),
    expense_account_id BIGINT NOT NULL REFERENCES accounts(id),
    description        TEXT,
    status             VARCHAR(10) NOT NULL DEFAULT 'open' CHECK (status IN ('open','paid','void')),
    journal_entry_id   BIGINT REFERENCES journal_entries(id),
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (vendor_id, bill_number),
    CHECK (amount_paid <= amount),
    CHECK (due_date >= bill_date)
);
CREATE INDEX IF NOT EXISTS ix_bills_status_due ON bills (status, due_date);

CREATE TABLE IF NOT EXISTS bill_payments (
    id               BIGSERIAL PRIMARY KEY,
    bill_id          BIGINT NOT NULL REFERENCES bills(id),
    payment_date     DATE NOT NULL,
    amount           NUMERIC(18,2) NOT NULL CHECK (amount > 0),
    bank_account_id  BIGINT NOT NULL REFERENCES bank_accounts(id),
    journal_entry_id BIGINT REFERENCES journal_entries(id),
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ───────────────────────────── Accounts Receivable ─────────────────────────────
CREATE TABLE IF NOT EXISTS customers (
    id                 BIGSERIAL PRIMARY KEY,
    name               VARCHAR(200) NOT NULL UNIQUE,
    email              VARCHAR(255),
    phone              VARCHAR(50),
    payment_terms_days INT NOT NULL DEFAULT 30 CHECK (payment_terms_days BETWEEN 0 AND 365),
    is_active          BOOLEAN NOT NULL DEFAULT TRUE,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS invoices (
    id                 BIGSERIAL PRIMARY KEY,
    customer_id        BIGINT NOT NULL REFERENCES customers(id),
    invoice_number     VARCHAR(60) NOT NULL UNIQUE,
    invoice_date       DATE NOT NULL,
    due_date           DATE NOT NULL,
    amount             NUMERIC(18,2) NOT NULL CHECK (amount > 0),
    amount_paid        NUMERIC(18,2) NOT NULL DEFAULT 0 CHECK (amount_paid >= 0),
    revenue_account_id BIGINT NOT NULL REFERENCES accounts(id),
    description        TEXT,
    status             VARCHAR(10) NOT NULL DEFAULT 'open' CHECK (status IN ('open','paid','void')),
    journal_entry_id   BIGINT REFERENCES journal_entries(id),
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (amount_paid <= amount),
    CHECK (due_date >= invoice_date)
);
CREATE INDEX IF NOT EXISTS ix_invoices_status_due ON invoices (status, due_date);

CREATE TABLE IF NOT EXISTS invoice_payments (
    id               BIGSERIAL PRIMARY KEY,
    invoice_id       BIGINT NOT NULL REFERENCES invoices(id),
    payment_date     DATE NOT NULL,
    amount           NUMERIC(18,2) NOT NULL CHECK (amount > 0),
    bank_account_id  BIGINT NOT NULL REFERENCES bank_accounts(id),
    journal_entry_id BIGINT REFERENCES journal_entries(id),
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ───────────────────────────── Fixed Assets ─────────────────────────────
CREATE TABLE IF NOT EXISTS fixed_assets (
    id                                BIGSERIAL PRIMARY KEY,
    asset_tag                         VARCHAR(40)  NOT NULL UNIQUE,
    name                              VARCHAR(200) NOT NULL,
    asset_account_id                  BIGINT NOT NULL REFERENCES accounts(id),
    accumulated_depreciation_account_id BIGINT NOT NULL REFERENCES accounts(id),
    depreciation_expense_account_id   BIGINT NOT NULL REFERENCES accounts(id),
    acquisition_date                  DATE NOT NULL,
    cost                              NUMERIC(18,2) NOT NULL CHECK (cost > 0),
    salvage_value                     NUMERIC(18,2) NOT NULL DEFAULT 0 CHECK (salvage_value >= 0),
    useful_life_months                INT NOT NULL CHECK (useful_life_months > 0),
    accumulated_depreciation          NUMERIC(18,2) NOT NULL DEFAULT 0 CHECK (accumulated_depreciation >= 0),
    status                            VARCHAR(20) NOT NULL DEFAULT 'active'
        CHECK (status IN ('active','fully_depreciated')),
    acquisition_entry_id              BIGINT REFERENCES journal_entries(id),
    created_at                        TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (salvage_value <= cost),
    CHECK (accumulated_depreciation <= cost - salvage_value)
);

CREATE TABLE IF NOT EXISTS depreciation_entries (
    id               BIGSERIAL PRIMARY KEY,
    asset_id         BIGINT NOT NULL REFERENCES fixed_assets(id),
    period_end       DATE NOT NULL,
    amount           NUMERIC(18,2) NOT NULL CHECK (amount > 0),
    journal_entry_id BIGINT NOT NULL REFERENCES journal_entries(id),
    UNIQUE (asset_id, period_end)
);

-- ───────────────────────────── Budgeting ─────────────────────────────
CREATE TABLE IF NOT EXISTS budgets (
    id          BIGSERIAL PRIMARY KEY,
    name        VARCHAR(200) NOT NULL,
    fiscal_year INT NOT NULL CHECK (fiscal_year BETWEEN 1990 AND 2200),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (name, fiscal_year)
);

CREATE TABLE IF NOT EXISTS budget_lines (
    id         BIGSERIAL PRIMARY KEY,
    budget_id  BIGINT NOT NULL REFERENCES budgets(id) ON DELETE CASCADE,
    account_id BIGINT NOT NULL REFERENCES accounts(id),
    month      SMALLINT NOT NULL CHECK (month BETWEEN 1 AND 12),
    amount     NUMERIC(18,2) NOT NULL CHECK (amount >= 0),
    UNIQUE (budget_id, account_id, month)
);

-- ───────────────────────────── HR: core ─────────────────────────────
CREATE TABLE IF NOT EXISTS departments (
    id         BIGSERIAL PRIMARY KEY,
    name       VARCHAR(120) NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS employees (
    id               BIGSERIAL PRIMARY KEY,
    employee_number  VARCHAR(30)  NOT NULL UNIQUE,
    first_name       VARCHAR(100) NOT NULL,
    last_name        VARCHAR(100) NOT NULL,
    email            VARCHAR(255) NOT NULL UNIQUE,
    department_id    BIGINT REFERENCES departments(id),
    job_title        VARCHAR(150) NOT NULL,
    hire_date        DATE NOT NULL,
    termination_date DATE,
    status           VARCHAR(12) NOT NULL DEFAULT 'active' CHECK (status IN ('active','terminated')),
    base_salary      NUMERIC(18,2) NOT NULL CHECK (base_salary >= 0),   -- annual
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (termination_date IS NULL OR termination_date >= hire_date),
    CHECK ((status = 'terminated') = (termination_date IS NOT NULL))
);
CREATE INDEX IF NOT EXISTS ix_employees_department ON employees (department_id);

-- ───────────────────────────── HR: recruitment ─────────────────────────────
CREATE TABLE IF NOT EXISTS job_postings (
    id            BIGSERIAL PRIMARY KEY,
    title         VARCHAR(150) NOT NULL,
    department_id BIGINT REFERENCES departments(id),
    description   TEXT,
    status        VARCHAR(10) NOT NULL DEFAULT 'open' CHECK (status IN ('open','closed')),
    opened_on     DATE NOT NULL DEFAULT CURRENT_DATE,
    closed_on     DATE,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS applications (
    id         BIGSERIAL PRIMARY KEY,
    posting_id BIGINT NOT NULL REFERENCES job_postings(id),
    first_name VARCHAR(100) NOT NULL,
    last_name  VARCHAR(100) NOT NULL,
    email      VARCHAR(255) NOT NULL,
    phone      VARCHAR(50),
    stage      VARCHAR(12) NOT NULL DEFAULT 'applied'
        CHECK (stage IN ('applied','screening','interview','offer','hired','rejected')),
    notes      TEXT,
    applied_on DATE NOT NULL DEFAULT CURRENT_DATE,
    employee_id BIGINT REFERENCES employees(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (posting_id, email)
);

-- ───────────────────────────── HR: benefits ─────────────────────────────
CREATE TABLE IF NOT EXISTS benefit_plans (
    id            BIGSERIAL PRIMARY KEY,
    name          VARCHAR(150) NOT NULL UNIQUE,
    plan_type     VARCHAR(40)  NOT NULL,
    employee_cost NUMERIC(18,2) NOT NULL DEFAULT 0 CHECK (employee_cost >= 0),  -- per payroll run
    employer_cost NUMERIC(18,2) NOT NULL DEFAULT 0 CHECK (employer_cost >= 0),  -- per payroll run
    is_active     BOOLEAN NOT NULL DEFAULT TRUE,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS employee_benefits (
    id             BIGSERIAL PRIMARY KEY,
    employee_id    BIGINT NOT NULL REFERENCES employees(id),
    plan_id        BIGINT NOT NULL REFERENCES benefit_plans(id),
    effective_date DATE NOT NULL,
    end_date       DATE,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (end_date IS NULL OR end_date >= effective_date)
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_employee_benefits_active
    ON employee_benefits (employee_id, plan_id) WHERE end_date IS NULL;

-- ───────────────────────────── HR: payroll ─────────────────────────────
CREATE TABLE IF NOT EXISTS payroll_runs (
    id               BIGSERIAL PRIMARY KEY,
    period_start     DATE NOT NULL,
    period_end       DATE NOT NULL,
    pay_date         DATE NOT NULL,
    status           VARCHAR(10) NOT NULL DEFAULT 'draft' CHECK (status IN ('draft','approved','posted')),
    journal_entry_id BIGINT REFERENCES journal_entries(id),
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (period_start, period_end),
    CHECK (period_end >= period_start)
);

CREATE TABLE IF NOT EXISTS payslips (
    id                          BIGSERIAL PRIMARY KEY,
    run_id                      BIGINT NOT NULL REFERENCES payroll_runs(id),
    employee_id                 BIGINT NOT NULL REFERENCES employees(id),
    gross_pay                   NUMERIC(18,2) NOT NULL CHECK (gross_pay >= 0),
    tax_withheld                NUMERIC(18,2) NOT NULL CHECK (tax_withheld >= 0),
    employee_benefit_deductions NUMERIC(18,2) NOT NULL CHECK (employee_benefit_deductions >= 0),
    employer_benefit_cost       NUMERIC(18,2) NOT NULL CHECK (employer_benefit_cost >= 0),
    net_pay                     NUMERIC(18,2) NOT NULL CHECK (net_pay >= 0),
    UNIQUE (run_id, employee_id)
);

-- ───────────────────────────── HR: training ─────────────────────────────
CREATE TABLE IF NOT EXISTS training_courses (
    id             BIGSERIAL PRIMARY KEY,
    title          VARCHAR(200) NOT NULL UNIQUE,
    description    TEXT,
    duration_hours NUMERIC(6,1) NOT NULL DEFAULT 0 CHECK (duration_hours >= 0),
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS training_enrollments (
    id           BIGSERIAL PRIMARY KEY,
    employee_id  BIGINT NOT NULL REFERENCES employees(id),
    course_id    BIGINT NOT NULL REFERENCES training_courses(id),
    status       VARCHAR(10) NOT NULL DEFAULT 'enrolled' CHECK (status IN ('enrolled','completed')),
    enrolled_on  DATE NOT NULL DEFAULT CURRENT_DATE,
    completed_on DATE,
    score        SMALLINT CHECK (score BETWEEN 0 AND 100),
    UNIQUE (employee_id, course_id)
);

-- ───────────────────────────── HR: performance ─────────────────────────────
CREATE TABLE IF NOT EXISTS performance_reviews (
    id           BIGSERIAL PRIMARY KEY,
    employee_id  BIGINT NOT NULL REFERENCES employees(id),
    reviewer_id  BIGINT NOT NULL REFERENCES employees(id),
    period_label VARCHAR(60) NOT NULL,
    period_start DATE NOT NULL,
    period_end   DATE NOT NULL,
    rating       SMALLINT NOT NULL CHECK (rating BETWEEN 1 AND 5),
    strengths    TEXT,
    improvements TEXT,
    comments     TEXT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (employee_id <> reviewer_id),
    CHECK (period_end >= period_start),
    UNIQUE (employee_id, period_label)
);

-- ───────────────────────────── Seed chart of accounts ─────────────────────────────
INSERT INTO accounts (code, name, account_type) VALUES
    ('1000', 'Cash and Bank',                   'asset'),
    ('1100', 'Accounts Receivable',             'asset'),
    ('1500', 'Fixed Assets',                    'asset'),
    ('1590', 'Accumulated Depreciation',        'asset'),
    ('2000', 'Accounts Payable',                'liability'),
    ('2100', 'Payroll Liabilities',             'liability'),
    ('3000', 'Owner''s Equity',                 'equity'),
    ('4000', 'Sales Revenue',                   'revenue'),
    ('5000', 'Salaries and Wages Expense',      'expense'),
    ('5100', 'Employee Benefits Expense',       'expense'),
    ('5200', 'Depreciation Expense',            'expense'),
    ('5900', 'General and Administrative',      'expense')
ON CONFLICT (code) DO NOTHING;
