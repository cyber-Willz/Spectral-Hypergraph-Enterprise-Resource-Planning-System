-- Run after restoring data from a standalone erp_system database: re-aligns every BIGSERIAL sequence
-- of the ERP tables with the restored rows.
DO $$
DECLARE r record;
BEGIN
  FOR r IN SELECT c.table_name, c.column_name, pg_get_serial_sequence(c.table_name, c.column_name) AS seq
           FROM information_schema.columns c
           WHERE c.table_schema='public' AND c.column_default LIKE 'nextval%'
             AND c.table_name IN ('accounts','journal_entries','journal_lines','vendors','bills','bill_payments','customers','invoices','invoice_payments','fixed_assets','depreciation_entries','bank_accounts','cash_transactions','budgets','budget_lines','departments','employees','job_postings','applications','payroll_runs','payslips','benefit_plans','employee_benefits','training_courses','training_enrollments','performance_reviews','counterparty_risk_profiles')
  LOOP
    EXECUTE format('SELECT setval(%L, COALESCE((SELECT max(%I) FROM %I),0)+1, false)', r.seq, r.column_name, r.table_name);
  END LOOP;
END $$;
