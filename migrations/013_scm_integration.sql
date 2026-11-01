-- Upgrade 008: links between the supply chain module and the ERP core / CRM / QMS / GRC.
-- Every link is nullable and UNIQUE so each bridge flow is idempotent.

-- ledger accounts used by the supply chain bridges
INSERT INTO accounts (code, name, account_type) VALUES
    ('1300', 'Inventory',               'asset'),
    ('5300', 'Cost of Goods Sold',      'expense'),
    ('5400', 'Freight and Logistics',   'expense')
ON CONFLICT (code) DO NOTHING;

-- supplier -> AP vendor
ALTER TABLE suppliers ADD COLUMN IF NOT EXISTS erp_vendor_id bigint UNIQUE REFERENCES vendors(id) ON DELETE SET NULL;
CREATE INDEX IF NOT EXISTS idx_suppliers_erp_vendor_id ON suppliers(erp_vendor_id);

-- supply chain customer -> AR customer and CRM account
ALTER TABLE scm_customers ADD COLUMN IF NOT EXISTS erp_customer_id bigint UNIQUE REFERENCES customers(id) ON DELETE SET NULL;
ALTER TABLE scm_customers ADD COLUMN IF NOT EXISTS crm_account_id uuid UNIQUE REFERENCES crm_accounts(id) ON DELETE SET NULL;
CREATE INDEX IF NOT EXISTS idx_scm_customers_erp_customer_id ON scm_customers(erp_customer_id);

-- supplier invoice -> AP bill
ALTER TABLE supplier_invoices ADD COLUMN IF NOT EXISTS erp_bill_id bigint UNIQUE REFERENCES bills(id) ON DELETE SET NULL;
CREATE INDEX IF NOT EXISTS idx_supplier_invoices_erp_bill_id ON supplier_invoices(erp_bill_id);

-- sales order -> AR invoice and the cost-of-goods-sold journal entry
ALTER TABLE sales_orders ADD COLUMN IF NOT EXISTS erp_invoice_id bigint UNIQUE REFERENCES invoices(id) ON DELETE SET NULL;
ALTER TABLE sales_orders ADD COLUMN IF NOT EXISTS cogs_entry_id bigint UNIQUE REFERENCES journal_entries(id) ON DELETE SET NULL;
CREATE INDEX IF NOT EXISTS idx_sales_orders_erp_invoice_id ON sales_orders(erp_invoice_id);

-- quality incident -> QMS nonconformance
ALTER TABLE quality_incidents ADD COLUMN IF NOT EXISTS qms_nonconformance_id uuid UNIQUE REFERENCES qms_nonconformances(id) ON DELETE SET NULL;
CREATE INDEX IF NOT EXISTS idx_quality_incidents_qms_nc ON quality_incidents(qms_nonconformance_id);

-- supplier -> GRC risk raised from its scorecard
ALTER TABLE suppliers ADD COLUMN IF NOT EXISTS grc_risk_id uuid UNIQUE REFERENCES grc_risks(id) ON DELETE SET NULL;
