"""Schema specification for the enterprise suite.

Run `python tools/gen.py` to (re)generate:
  - migrations/001_schema.sql
  - src/entities/*.rs         (SeaORM entities + typed filters)
  - src/routes_generated.rs   (CRUD routers)
"""

SQL_TYPES = {"s": "text", "i": "integer", "bi": "bigint", "d": "numeric(18,2)", "f": "double precision",
             "b": "boolean", "dt": "date", "ts": "timestamptz", "j": "jsonb", "u": "uuid"}
RUST_TYPES = {"s": "String", "i": "i32", "bi": "i64", "d": "Decimal", "f": "f64", "b": "bool",
              "dt": "Date", "ts": "DateTimeWithTimeZone", "j": "Json", "u": "Uuid"}

SEQUENCES = {}


class Col:
    def __init__(self, name, ty, req=False, default=None, enum=None, fk=None, check=None,
                 unique=False, gen=None, hidden=False, since=1):
        self.since = since
        self.name, self.ty, self.req, self.default, self.enum = name, ty, req, default, enum
        self.fk, self.check, self.unique, self.gen, self.hidden = fk, check, unique, gen, hidden

    @property
    def not_null(self):
        return self.req or self.default is not None


def _mk(ty):
    def f(name, **kw):
        return Col(name, ty, **kw)
    return f


S, I, BI, D, F, B, DT, TS, J, U = (_mk(t) for t in ["s", "i", "bi", "d", "f", "b", "dt", "ts", "j", "u"])


def SEQ(seq, prefix):
    SEQUENCES[seq] = prefix
    return f"'{prefix}-' || lpad(nextval('{seq}')::text, 6, '0')"


class Table:
    def __init__(self, name, cols, uniques=(), checks=(), ops="lgcud", since=1):
        self.since = since
        self.name, self.cols, self.uniques, self.checks, self.ops = name, cols, list(uniques), list(checks), ops

    @property
    def module(self):
        return self.name.split("_", 1)[0]

    @property
    def resource(self):
        return self.name.split("_", 1)[1].replace("_", "-") if "_" in self.name else self.name


def own(name="owner_id"):
    return U(name, fk="users")


def money(name, default="0"):
    return D(name, default=default)


def rng(a, b):
    return f"({a} IS NULL OR {b} IS NULL OR {b} >= {a})"


TABLES = [
    Table("users", [
        S("email", req=True, unique=True), S("full_name", req=True),
        S("password_hash", req=True, hidden=True),
        S("role", default="'user'", enum=["admin", "manager", "user", "auditor", "finance", "hr"]),
        B("is_active", default="true"), TS("last_login_at"),
    ], ops=""),
    Table("sys_audit_log", [
        U("user_id", fk="users"), S("email"), S("method", req=True), S("path", req=True), I("status_code"),
    ], ops="lg"),

    # ---------------------------------------------------------------- CRM
    Table("crm_accounts", [
        S("name", req=True), S("industry"), S("website"), S("phone"), S("email"), S("address"), S("city"), S("country"),
        S("account_type", default="'prospect'", enum=["prospect", "customer", "partner", "vendor"]),
        S("status", default="'active'", enum=["active", "inactive"]),
        D("annual_revenue"), I("employees", check="employees >= 0"), own(), S("notes"),
        # --- migration 003
        I("year_established", since=3), U("parent_account_id", fk="crm_accounts", since=3), S("external_ref", unique=True, since=3),
        BI("erp_customer_id", fk="customers", unique=True, since=4),
    ]),
    Table("crm_contacts", [
        U("account_id", fk="crm_accounts"), S("first_name", req=True), S("last_name", req=True), S("email"), S("phone"),
        S("job_title"), B("is_primary", default="false"), own(), S("notes"),
    ]),
    Table("crm_campaigns", [
        S("name", req=True),
        S("channel", default="'email'", enum=["email", "social", "event", "webinar", "ads", "direct_mail", "other"]),
        S("status", default="'planned'", enum=["planned", "active", "completed", "cancelled"]),
        DT("start_date"), DT("end_date"), money("budget"), money("actual_cost"), money("expected_revenue"),
        S("description"), own(),
    ], checks=[rng("start_date", "end_date")]),
    Table("crm_leads", [
        S("first_name"), S("last_name", req=True), S("company"), S("email"), S("phone"), S("job_title"),
        S("source", default="'web'", enum=["web", "referral", "event", "campaign", "cold_call", "partner", "other"]),
        S("status", default="'new'", enum=["new", "contacted", "qualified", "unqualified", "converted"]),
        I("score", default="0"), U("campaign_id", fk="crm_campaigns"), own(),
        U("converted_account_id", fk="crm_accounts"), U("converted_contact_id", fk="crm_contacts"),
        U("converted_opportunity_id"), TS("converted_at"), S("notes"),
    ]),
    Table("crm_opportunities", [
        U("account_id", fk="crm_accounts"), U("contact_id", fk="crm_contacts"), S("name", req=True),
        S("stage", default="'prospecting'",
          enum=["prospecting", "qualification", "proposal", "negotiation", "closed_won", "closed_lost"]),
        money("amount"), I("probability", check="probability BETWEEN 0 AND 100"), DT("expected_close_date"),
        TS("closed_at"), S("lost_reason"), U("campaign_id", fk="crm_campaigns"), own(), S("next_step"), S("description"),
        # --- migration 003 (sales-pipeline attributes)
        S("external_ref", unique=True, since=3), U("product_id", fk="crm_products", since=3),
        U("sales_agent_id", fk="crm_sales_agents", since=3), DT("engage_date", since=3), DT("close_date", since=3),
        D("close_value", since=3), S("source_stage", since=3), S("source_dataset", since=3), F("ml_win_probability", since=3),
        BI("erp_invoice_id", fk="invoices", unique=True, since=4),
    ]),
    Table("crm_activities", [
        S("kind", req=True, enum=["call", "email", "meeting", "task", "note"]), S("subject", req=True), S("description"),
        S("status", default="'open'", enum=["open", "done", "cancelled"]), TS("due_at"), TS("completed_at"),
        U("account_id", fk="crm_accounts"), U("contact_id", fk="crm_contacts"), U("lead_id", fk="crm_leads"),
        U("opportunity_id", fk="crm_opportunities"), own(),
    ]),
    Table("crm_cases", [
        S("case_number", default=SEQ("seq_case", "CS"), unique=True),
        U("account_id", fk="crm_accounts"), U("contact_id", fk="crm_contacts"), S("subject", req=True), S("description"),
        S("priority", default="'medium'", enum=["low", "medium", "high", "critical"]),
        S("status", default="'open'", enum=["open", "in_progress", "waiting", "resolved", "closed"]),
        S("channel", default="'email'", enum=["email", "phone", "web", "chat"]),
        U("assigned_to", fk="users"), TS("sla_due_at"), TS("first_response_at"), TS("resolved_at"),
        I("satisfaction_score", check="satisfaction_score BETWEEN 1 AND 5"), S("resolution"),
    ]),
    Table("crm_scoring_rules", [
        S("name", req=True), S("field", req=True),
        S("operator", default="'eq'", enum=["eq", "neq", "contains", "not_null", "is_null", "gte", "lte"]),
        S("value"), I("points", req=True), B("is_active", default="true"),
    ]),

    Table("crm_products", [
        S("product_code", req=True, unique=True), S("name", req=True), S("category"), S("series"), D("list_price"),
        S("supplier_code"), DT("launch_date"), S("source_dataset"),
    ], since=3),
    Table("crm_sales_agents", [
        S("name", req=True, unique=True), S("manager_name"), S("regional_office"), B("is_active", default="true"), S("source_dataset"),
    ], since=3),
    Table("crm_customers", [
        S("external_id", unique=True), S("kind", req=True, enum=["consumer", "donor"]),
        S("first_name"), S("last_name"), S("email"), S("phone"), S("city"), S("state"), S("postal_code"), S("region"),
        DT("signup_date"), S("gender", enum=["female", "male", "other"]), S("loyalty_tier", enum=["bronze", "silver", "gold"]),
        DT("last_activity_date"), I("gift_count", check="gift_count >= 0"), D("total_donated"),
        I("engagement_score", check="engagement_score BETWEEN 0 AND 100"), B("event_participation"), S("source_dataset"),
    ], since=3),
    Table("crm_orders", [
        S("order_code", req=True, unique=True), U("customer_id", req=True, fk="crm_customers"),
        U("product_id", req=True, fk="crm_products"), I("quantity", req=True, check="quantity > 0"),
        D("unit_price", req=True, check="unit_price >= 0"), D("discount_rate", check="discount_rate BETWEEN 0 AND 1"),
        DT("order_date"), S("delivery_status", enum=["delivered", "delayed", "cancelled"]),
        S("payment_method", enum=["credit_card", "paypal", "bank_transfer"]), S("region"),
        D("net_amount", req=True, gen="round(quantity * unit_price * (1 - COALESCE(discount_rate, 0)), 2)"),
        S("source_dataset"),
    ], since=3),
    Table("crm_import_batches", [
        S("kind", req=True), S("filename"), I("rows_total", default="0"), I("rows_inserted", default="0"),
        I("rows_updated", default="0"), I("rows_rejected", default="0"), J("corrections", default="'{}'::jsonb"),
        J("warnings", default="'[]'::jsonb"), U("created_by", fk="users"),
    ], ops="lg", since=3),
    Table("crm_import_rejects", [
        U("batch_id", req=True, fk="crm_import_batches"), I("line"), S("reason", req=True), J("raw"),
    ], ops="lg", since=3),
    Table("crm_models", [
        S("name", req=True), S("algorithm", req=True), TS("trained_at", default="now()"), B("is_active", default="false"),
        B("usable", default="false"), I("n_train"), I("n_test"), DT("cutoff_date"),
        J("metrics", default="'{}'::jsonb"), J("coefficients", default="'{}'::jsonb"),
    ], ops="lg", since=3),

    # ------------------------------------------------------------ Projects
    Table("pm_resources", [
        S("name", req=True), S("kind", default="'person'", enum=["person", "equipment", "contractor"]),
        S("email"), S("role"), money("hourly_cost"), money("hourly_bill_rate"),
        D("capacity_hours_per_week", default="40"), B("is_active", default="true"), U("user_id", fk="users"),
        BI("employee_id", fk="employees", unique=True, since=4),
    ]),
    Table("pm_projects", [
        S("code", req=True, unique=True), S("name", req=True), S("description"),
        U("account_id", fk="crm_accounts"), U("manager_id", fk="users"),
        S("status", default="'planning'", enum=["planning", "active", "on_hold", "completed", "cancelled"]),
        S("priority", default="'medium'", enum=["low", "medium", "high", "critical"]),
        DT("start_date"), DT("end_date"), money("budget"),
        S("billing_type", default="'time_and_material'", enum=["fixed_price", "time_and_material", "non_billable"]),
        money("contract_value"),
        # --- added in migration 002 (portfolio / business-case attributes)
        S("project_type", since=2), S("region", since=2), S("department", since=2),
        D("expected_benefit", since=2),
        S("complexity", enum=["low", "medium", "high"], since=2),
        I("phase_no", check="phase_no BETWEEN 1 AND 5", since=2), S("phase_name", since=2),
        I("percent_complete", check="percent_complete BETWEEN 0 AND 100", since=2),
        U("project_manager_id", fk="pm_resources", since=2), S("import_source", since=2),
    ], checks=[rng("start_date", "end_date")]),
    Table("pm_tasks", [
        U("project_id", req=True, fk="pm_projects"), U("parent_task_id", fk="pm_tasks"), S("name", req=True),
        S("description"),
        S("status", default="'not_started'", enum=["not_started", "in_progress", "blocked", "completed", "cancelled"]),
        S("priority", default="'medium'", enum=["low", "medium", "high", "critical"]),
        I("duration_days", default="1", check="duration_days >= 0"),
        DT("planned_start"), DT("planned_end"), DT("actual_start"), DT("actual_end"),
        I("percent_complete", default="0", check="percent_complete BETWEEN 0 AND 100"),
        money("estimated_hours"), money("planned_cost"), U("assignee_id", fk="pm_resources"),
        B("is_milestone", default="false"),
    ]),
    Table("pm_task_dependencies", [
        U("task_id", req=True, fk="pm_tasks"), U("depends_on_task_id", req=True, fk="pm_tasks"),
        S("dependency_type", default="'FS'", enum=["FS", "SS", "FF", "SF"]), I("lag_days", default="0"),
    ], uniques=[("task_id", "depends_on_task_id")], checks=["task_id <> depends_on_task_id"]),
    Table("pm_allocations", [
        U("resource_id", req=True, fk="pm_resources"), U("project_id", req=True, fk="pm_projects"),
        U("task_id", fk="pm_tasks"), DT("start_date", req=True), DT("end_date", req=True),
        D("hours_per_week", req=True, check="hours_per_week >= 0"),
    ], checks=["end_date >= start_date"]),
    Table("pm_time_entries", [
        U("project_id", req=True, fk="pm_projects"), U("task_id", fk="pm_tasks"),
        U("resource_id", req=True, fk="pm_resources"), DT("work_date", req=True, default="current_date"),
        D("hours", req=True, check="hours > 0"), B("billable", default="true"), S("description"),
        D("cost_rate"), D("bill_rate"), B("approved", default="false"),
    ]),
    Table("pm_cost_entries", [
        U("project_id", req=True, fk="pm_projects"), U("task_id", fk="pm_tasks"),
        S("category", req=True,
          enum=["labor", "materials", "equipment", "subcontractor", "travel", "overhead", "other"]),
        S("cost_type", default="'actual'", enum=["actual", "committed", "forecast"]), S("description"),
        D("amount", req=True), DT("incurred_on", default="current_date"), S("vendor"),
    ]),
    Table("pm_invoices", [
        U("project_id", req=True, fk="pm_projects"),
        S("invoice_number", default=SEQ("seq_invoice", "INV"), unique=True),
        DT("issue_date", default="current_date"), DT("due_date"), D("amount", req=True), money("tax"),
        S("status", default="'draft'", enum=["draft", "sent", "paid", "void"]), TS("paid_at"), S("notes"),
        BI("erp_invoice_id", fk="invoices", unique=True, since=4),
    ]),

    Table("pm_import_batches", [
        S("source_name", req=True), S("filename"), I("rows_total", default="0"), I("rows_inserted", default="0"),
        I("rows_updated", default="0"), I("rows_rejected", default="0"), J("errors", default="'[]'::jsonb"),
        U("created_by", fk="users"),
    ], ops="lg", since=2),

    # ----------------------------------------------------------------- GRC
    Table("grc_risks", [
        S("risk_code", default=SEQ("seq_risk", "RSK"), unique=True), S("title", req=True), S("description"),
        S("category", default="'operational'",
          enum=["strategic", "operational", "financial", "compliance", "technology", "safety", "reputational"]),
        own(), I("likelihood", default="3", check="likelihood BETWEEN 1 AND 5"),
        I("impact", default="3", check="impact BETWEEN 1 AND 5"),
        I("score", req=True, gen="likelihood * impact"),
        S("rating", req=True, gen="CASE WHEN likelihood * impact >= 15 THEN 'critical' WHEN likelihood * impact >= 10 "
                                  "THEN 'high' WHEN likelihood * impact >= 5 THEN 'medium' ELSE 'low' END"),
        I("residual_likelihood", check="residual_likelihood BETWEEN 1 AND 5"),
        I("residual_impact", check="residual_impact BETWEEN 1 AND 5"),
        I("residual_score", gen="residual_likelihood * residual_impact"),
        S("status", default="'open'", enum=["open", "mitigating", "accepted", "transferred", "closed"]),
        S("treatment", enum=["mitigate", "accept", "transfer", "avoid"]), S("mitigation_plan"),
        DT("review_date"), DT("identified_on", default="current_date"),
    ]),
    Table("grc_controls", [
        S("control_code", default=SEQ("seq_control", "CTL"), unique=True), S("name", req=True), S("description"),
        S("control_type", default="'preventive'", enum=["preventive", "detective", "corrective"]),
        S("frequency", default="'quarterly'",
          enum=["continuous", "daily", "weekly", "monthly", "quarterly", "annually", "ad_hoc"]),
        own(), S("status", default="'active'", enum=["draft", "active", "deprecated"]),
        S("effectiveness", default="'not_tested'",
          enum=["not_tested", "effective", "partially_effective", "ineffective"]),
        S("framework"), DT("last_tested_on"), DT("next_test_due"),
    ]),
    Table("grc_control_tests", [
        U("control_id", req=True, fk="grc_controls"), DT("tested_on", default="current_date"),
        U("tested_by", fk="users"), S("result", req=True, enum=["effective", "partially_effective", "ineffective"]),
        S("notes"), S("evidence_url"),
    ]),
    Table("grc_risk_controls", [
        U("risk_id", req=True, fk="grc_risks"), U("control_id", req=True, fk="grc_controls"),
    ], uniques=[("risk_id", "control_id")]),
    Table("grc_compliance_requirements", [
        S("framework", req=True), S("reference", req=True), S("title", req=True), S("description"),
        S("area", default="'general'", enum=["general", "quality", "environmental", "health_safety",
                                              "information_security", "financial", "privacy"]),
        own(), U("control_id", fk="grc_controls"),
        S("status", default="'not_assessed'",
          enum=["not_assessed", "compliant", "partially_compliant", "non_compliant", "not_applicable"]),
        DT("last_assessed_on"), DT("next_review_on"), S("evidence_url"),
    ], uniques=[("framework", "reference")]),
    Table("grc_audits", [
        S("audit_code", default=SEQ("seq_audit", "AUD"), unique=True), S("title", req=True),
        S("audit_type", default="'internal'", enum=["internal", "external", "regulatory", "supplier"]), S("scope"),
        S("status", default="'planned'", enum=["planned", "in_progress", "reporting", "completed", "cancelled"]),
        U("lead_auditor_id", fk="users"), DT("planned_start"), DT("planned_end"), DT("actual_start"),
        DT("actual_end"), S("summary"),
    ]),
    Table("grc_audit_findings", [
        U("audit_id", req=True, fk="grc_audits"), S("title", req=True), S("description"),
        S("severity", default="'minor'", enum=["observation", "minor", "major", "critical"]),
        S("status", default="'open'", enum=["open", "in_remediation", "verified", "closed"]),
        U("control_id", fk="grc_controls"), U("requirement_id", fk="grc_compliance_requirements"), own(),
        DT("due_date"), TS("closed_at"), S("remediation_plan"),
    ]),

    # --------------------------------------------------------------- Assets
    Table("am_assets", [
        S("asset_tag", req=True, unique=True), S("name", req=True), S("description"), S("category"),
        S("manufacturer"), S("model_name"), S("serial_number"), S("location"),
        S("status", default="'active'",
          enum=["planned", "active", "under_maintenance", "idle", "retired", "disposed"]),
        S("criticality", default="'medium'", enum=["low", "medium", "high", "critical"]),
        DT("purchase_date"), money("purchase_cost"), money("salvage_value"),
        I("useful_life_months", default="60", check="useful_life_months > 0"),
        DT("installed_on"), DT("warranty_expires"), DT("retired_on"), U("parent_asset_id", fk="am_assets"),
        U("assigned_to", fk="users"),
        BI("fixed_asset_id", fk="fixed_assets", unique=True, since=4),
    ]),
    Table("am_maintenance_plans", [
        U("asset_id", req=True, fk="am_assets"), S("name", req=True),
        S("plan_type", default="'preventive'", enum=["preventive", "predictive", "inspection", "calibration"]),
        I("frequency_days", req=True, check="frequency_days > 0"), DT("last_performed"), DT("next_due"),
        money("estimated_hours"), S("instructions"), U("assigned_to", fk="users"), B("is_active", default="true"),
    ]),
    Table("am_work_orders", [
        S("wo_number", default=SEQ("seq_wo", "WO"), unique=True), U("asset_id", req=True, fk="am_assets"),
        U("plan_id", fk="am_maintenance_plans"), S("title", req=True), S("description"),
        S("wo_type", default="'corrective'",
          enum=["preventive", "corrective", "predictive", "inspection", "calibration"]),
        S("priority", default="'medium'", enum=["low", "medium", "high", "critical"]),
        S("status", default="'open'",
          enum=["open", "scheduled", "in_progress", "on_hold", "completed", "cancelled"]),
        DT("scheduled_date"), TS("started_at"), TS("completed_at"), U("assigned_to", fk="users"),
        money("labor_hours"), money("labor_cost"), money("parts_cost"), money("downtime_hours"),
        S("failure_code"), S("notes"),
    ]),
    Table("am_meter_readings", [
        U("asset_id", req=True, fk="am_assets"), S("metric", req=True), F("value", req=True), S("unit"),
        TS("recorded_at", default="now()"),
    ]),
    Table("am_predictive_alerts", [
        U("asset_id", req=True, fk="am_assets"), S("metric", req=True),
        S("severity", req=True, enum=["warning", "critical"]),
        S("kind", default="'anomaly'", enum=["anomaly", "trend"]), S("message"),
        F("observed_value"), F("expected_value"), F("z_score"), F("days_to_threshold"),
        S("status", default="'open'", enum=["open", "acknowledged", "resolved"]),
        U("work_order_id", fk="am_work_orders"),
    ]),
    Table("am_lifecycle_events", [
        U("asset_id", req=True, fk="am_assets"),
        S("event_type", req=True, enum=["acquired", "commissioned", "transferred", "upgraded", "repaired",
                                         "revalued", "retired", "disposed"]),
        DT("event_date", default="current_date"), S("description"), D("cost"), U("performed_by", fk="users"),
    ]),

    # ------------------------------------------------------------------ EHS
    Table("ehs_incidents", [
        S("incident_number", default=SEQ("seq_incident", "INC"), unique=True),
        S("incident_type", req=True, enum=["injury", "illness", "near_miss", "property_damage",
                                            "environmental_release", "vehicle", "security", "other"]),
        S("severity", default="'minor'", enum=["minor", "moderate", "serious", "critical", "fatal"]),
        S("title", req=True), S("description"), TS("occurred_at", req=True), S("location"),
        U("reported_by", fk="users"), S("injured_person"), S("body_part"),
        I("lost_days", default="0"), I("restricted_days", default="0"),
        B("is_recordable", default="false"), B("is_reportable", default="false"),
        S("status", default="'reported'", enum=["reported", "investigating", "corrective_action", "closed"]),
        S("root_cause"), S("immediate_action"), U("asset_id", fk="am_assets"), TS("closed_at"),
    ]),
    Table("ehs_safety_audits", [
        S("audit_number", default=SEQ("seq_safety_audit", "SA"), unique=True), S("title", req=True),
        S("inspection_type", default="'routine'", enum=["routine", "regulatory", "contractor", "pre_task"]),
        S("location"), U("auditor_id", fk="users"), DT("conducted_on", req=True, default="current_date"),
        S("status", default="'planned'", enum=["planned", "in_progress", "completed"]),
        F("score"), S("summary"), DT("next_due"),
    ]),
    Table("ehs_safety_audit_items", [
        U("audit_id", req=True, fk="ehs_safety_audits"), S("category"), S("item", req=True),
        S("result", default="'pending'", enum=["pending", "conforming", "non_conforming", "not_applicable"]),
        S("comments"),
    ]),
    Table("ehs_environmental_readings", [
        S("site", req=True), S("metric", req=True), F("value", req=True), S("unit", req=True),
        DT("reading_date", req=True, default="current_date"), F("limit_value"), S("source"), S("notes"),
    ]),
    Table("ehs_work_hours", [
        S("site"), DT("period_start", req=True), DT("period_end", req=True), D("hours_worked", req=True),
    ], checks=["period_end >= period_start"]),

    # ------------------------------------------------------------------ QMS
    Table("qms_test_plans", [
        S("plan_code", default=SEQ("seq_test_plan", "TP"), unique=True), S("name", req=True), S("description"),
        S("product"), S("characteristic"), S("method"), S("unit"), F("lower_limit"), F("upper_limit"),
        S("frequency"), S("acceptance_criteria"), B("is_active", default="true"),
    ]),
    Table("qms_test_results", [
        U("test_plan_id", req=True, fk="qms_test_plans"), S("lot_number"), S("sample_id"), F("measured_value"),
        S("result", default="'pending'", enum=["pending", "pass", "fail"]), U("tested_by", fk="users"),
        TS("tested_at", default="now()"), S("notes"),
    ]),
    Table("qms_nonconformances", [
        S("ncr_number", default=SEQ("seq_ncr", "NCR"), unique=True), S("title", req=True), S("description"),
        S("source", default="'internal'", enum=["internal", "test", "customer", "supplier", "audit"]),
        S("severity", default="'minor'", enum=["minor", "major", "critical"]),
        S("status", default="'open'",
          enum=["open", "investigating", "containment", "corrective_action", "verification", "closed"]),
        DT("detected_on", default="current_date"), U("detected_by", fk="users"), S("product_or_process"),
        S("lot_number"), U("test_result_id", fk="qms_test_results"), U("audit_finding_id", fk="grc_audit_findings"),
        I("quantity_affected"), S("root_cause"),
        S("disposition", enum=["use_as_is", "rework", "repair", "scrap", "return_to_supplier"]),
        TS("closed_at"), own(),
    ]),
    Table("qms_corrective_actions", [
        S("capa_number", default=SEQ("seq_capa", "CAPA"), unique=True),
        U("nonconformance_id", fk="qms_nonconformances"), U("incident_id", fk="ehs_incidents"),
        U("finding_id", fk="grc_audit_findings"),
        S("action_type", default="'corrective'", enum=["containment", "corrective", "preventive"]),
        S("description", req=True), own(), DT("due_date"),
        S("status", default="'open'", enum=["open", "in_progress", "implemented", "verified", "closed", "cancelled"]),
        DT("completed_on"), U("verified_by", fk="users"), DT("verified_on"), S("effectiveness_check"),
    ]),

    # ------------------------------------------------------------ Documents
    Table("doc_folders", [S("name", req=True), U("parent_id", fk="doc_folders"), S("description")]),
    Table("doc_retention_policies", [
        S("name", req=True, unique=True), S("description"),
        I("retention_years", req=True, check="retention_years >= 0"),
        S("disposition_action", default="'destroy'", enum=["destroy", "archive", "review"]),
    ]),
    Table("doc_documents", [
        S("doc_number", default=SEQ("seq_doc", "DOC"), unique=True), S("title", req=True), S("description"),
        S("doc_type", default="'general'",
          enum=["policy", "procedure", "work_instruction", "form", "record", "contract", "report", "drawing",
                "general"]),
        U("folder_id", fk="doc_folders"), own(),
        S("status", default="'draft'", enum=["draft", "in_review", "approved", "effective", "obsolete", "archived"]),
        S("confidentiality", default="'internal'", enum=["public", "internal", "confidential", "restricted"]),
        I("current_version", default="0"), J("tags", default="'[]'::jsonb"),
        U("retention_policy_id", fk="doc_retention_policies"), B("is_record", default="false"),
        B("legal_hold", default="false"), DT("effective_date"), DT("review_due"), TS("disposed_at"),
    ]),
    Table("doc_versions", [
        U("document_id", req=True, fk="doc_documents"), I("version_number", req=True), S("change_summary"),
        S("file_name"), S("mime_type"), BI("size_bytes"), S("content_hash"), S("content_text"), S("storage_uri"),
        U("created_by", fk="users"),
    ], uniques=[("document_id", "version_number")], ops="lg"),
    Table("doc_workflows", [
        S("name", req=True, unique=True), S("doc_type"), S("description"), B("is_active", default="true"),
        J("steps", req=True, default="'[]'::jsonb"),
    ]),
    Table("doc_workflow_instances", [
        U("workflow_id", req=True, fk="doc_workflows!"), U("document_id", req=True, fk="doc_documents"),
        I("version_number"), S("status", default="'in_progress'",
                                enum=["in_progress", "approved", "rejected", "cancelled"]),
        I("current_step", default="1"), U("started_by", fk="users"), TS("completed_at"),
    ], ops="lg"),
    Table("doc_workflow_tasks", [
        U("instance_id", req=True, fk="doc_workflow_instances"), I("step_order", req=True),
        S("step_name", req=True), S("assigned_role"), U("assignee_id", fk="users"),
        S("status", default="'pending'", enum=["pending", "approved", "rejected", "skipped"]),
        U("decided_by", fk="users"), TS("decided_at"), S("comments"),
    ], ops="lg"),
]

API_MODULE = {"doc": "docs"}

SQL_PRE = """
CREATE EXTENSION IF NOT EXISTS pgcrypto;
"""

SQL_POST = r"""
-- =====================================================================
-- Business rules enforced in the database
-- =====================================================================

-- CRM: case SLA + lifecycle timestamps
CREATE OR REPLACE FUNCTION crm_case_defaults() RETURNS trigger AS $$
BEGIN
  IF TG_OP = 'INSERT' AND NEW.sla_due_at IS NULL THEN
    NEW.sla_due_at := now() + CASE NEW.priority
      WHEN 'critical' THEN interval '4 hours' WHEN 'high' THEN interval '8 hours'
      WHEN 'medium' THEN interval '24 hours' ELSE interval '72 hours' END;
  END IF;
  IF NEW.status IN ('resolved', 'closed') THEN
    IF NEW.resolved_at IS NULL THEN NEW.resolved_at := now(); END IF;
  ELSE
    NEW.resolved_at := NULL;
  END IF;
  IF NEW.status <> 'open' AND NEW.first_response_at IS NULL THEN NEW.first_response_at := now(); END IF;
  RETURN NEW;
END $$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS trg_crm_cases_defaults ON crm_cases;
CREATE TRIGGER trg_crm_cases_defaults BEFORE INSERT OR UPDATE ON crm_cases
  FOR EACH ROW EXECUTE FUNCTION crm_case_defaults();

-- CRM: opportunity probability follows the stage unless set explicitly
CREATE OR REPLACE FUNCTION crm_opportunity_defaults() RETURNS trigger AS $$
DECLARE stage_prob integer;
BEGIN
  stage_prob := CASE NEW.stage WHEN 'prospecting' THEN 10 WHEN 'qualification' THEN 25 WHEN 'proposal' THEN 50
                WHEN 'negotiation' THEN 75 WHEN 'closed_won' THEN 100 ELSE 0 END;
  IF TG_OP = 'INSERT' THEN
    NEW.probability := COALESCE(NEW.probability, stage_prob);
  ELSIF NEW.stage IS DISTINCT FROM OLD.stage AND NEW.probability IS NOT DISTINCT FROM OLD.probability THEN
    NEW.probability := stage_prob;
  END IF;
  IF NEW.stage IN ('closed_won', 'closed_lost') THEN
    IF NEW.closed_at IS NULL THEN NEW.closed_at := now(); END IF;
    NEW.probability := stage_prob;
  ELSE
    NEW.closed_at := NULL;
  END IF;
  RETURN NEW;
END $$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS trg_crm_opportunities_defaults ON crm_opportunities;
CREATE TRIGGER trg_crm_opportunities_defaults BEFORE INSERT OR UPDATE ON crm_opportunities
  FOR EACH ROW EXECUTE FUNCTION crm_opportunity_defaults();

-- PM: snapshot rates onto time entries
CREATE OR REPLACE FUNCTION pm_time_entry_rates() RETURNS trigger AS $$
DECLARE v_cost numeric; v_bill numeric;
BEGIN
  IF NEW.cost_rate IS NULL OR NEW.bill_rate IS NULL THEN
    SELECT hourly_cost, hourly_bill_rate INTO v_cost, v_bill FROM pm_resources WHERE id = NEW.resource_id;
    NEW.cost_rate := COALESCE(NEW.cost_rate, v_cost);
    NEW.bill_rate := COALESCE(NEW.bill_rate, v_bill);
  END IF;
  RETURN NEW;
END $$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS trg_pm_time_entries_rates ON pm_time_entries;
CREATE TRIGGER trg_pm_time_entries_rates BEFORE INSERT ON pm_time_entries
  FOR EACH ROW EXECUTE FUNCTION pm_time_entry_rates();

-- PM: task status drives progress and actual dates
CREATE OR REPLACE FUNCTION pm_task_defaults() RETURNS trigger AS $$
BEGIN
  IF NEW.status = 'completed' THEN
    NEW.percent_complete := 100;
    IF NEW.actual_start IS NULL THEN NEW.actual_start := COALESCE(NEW.planned_start, current_date); END IF;
    IF NEW.actual_end IS NULL THEN NEW.actual_end := current_date; END IF;
  ELSIF NEW.status = 'in_progress' AND NEW.actual_start IS NULL THEN
    NEW.actual_start := current_date;
  END IF;
  RETURN NEW;
END $$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS trg_pm_tasks_defaults ON pm_tasks;
CREATE TRIGGER trg_pm_tasks_defaults BEFORE INSERT OR UPDATE ON pm_tasks
  FOR EACH ROW EXECUTE FUNCTION pm_task_defaults();

-- GRC: audit finding closure timestamp
CREATE OR REPLACE FUNCTION grc_finding_defaults() RETURNS trigger AS $$
BEGIN
  IF NEW.status = 'closed' THEN
    IF NEW.closed_at IS NULL THEN NEW.closed_at := now(); END IF;
  ELSE NEW.closed_at := NULL; END IF;
  RETURN NEW;
END $$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS trg_grc_findings_defaults ON grc_audit_findings;
CREATE TRIGGER trg_grc_findings_defaults BEFORE INSERT OR UPDATE ON grc_audit_findings
  FOR EACH ROW EXECUTE FUNCTION grc_finding_defaults();

-- Assets: maintenance plan next-due date
CREATE OR REPLACE FUNCTION am_plan_defaults() RETURNS trigger AS $$
BEGIN
  IF NEW.next_due IS NULL THEN
    NEW.next_due := COALESCE(NEW.last_performed, current_date) + NEW.frequency_days;
  END IF;
  RETURN NEW;
END $$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS trg_am_plans_defaults ON am_maintenance_plans;
CREATE TRIGGER trg_am_plans_defaults BEFORE INSERT OR UPDATE ON am_maintenance_plans
  FOR EACH ROW EXECUTE FUNCTION am_plan_defaults();

-- Assets: work order lifecycle timestamps
CREATE OR REPLACE FUNCTION am_wo_defaults() RETURNS trigger AS $$
BEGIN
  IF NEW.status = 'in_progress' AND NEW.started_at IS NULL THEN NEW.started_at := now(); END IF;
  IF NEW.status = 'completed' THEN
    IF NEW.started_at IS NULL THEN NEW.started_at := now(); END IF;
    IF NEW.completed_at IS NULL THEN NEW.completed_at := now(); END IF;
  END IF;
  RETURN NEW;
END $$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS trg_am_wo_defaults ON am_work_orders;
CREATE TRIGGER trg_am_wo_defaults BEFORE INSERT OR UPDATE ON am_work_orders
  FOR EACH ROW EXECUTE FUNCTION am_wo_defaults();

-- EHS: incident closure timestamp
CREATE OR REPLACE FUNCTION ehs_incident_defaults() RETURNS trigger AS $$
BEGIN
  IF NEW.status = 'closed' THEN
    IF NEW.closed_at IS NULL THEN NEW.closed_at := now(); END IF;
  ELSE NEW.closed_at := NULL; END IF;
  RETURN NEW;
END $$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS trg_ehs_incidents_defaults ON ehs_incidents;
CREATE TRIGGER trg_ehs_incidents_defaults BEFORE INSERT OR UPDATE ON ehs_incidents
  FOR EACH ROW EXECUTE FUNCTION ehs_incident_defaults();

-- QMS: evaluate test results against plan limits
CREATE OR REPLACE FUNCTION qms_test_result_eval() RETURNS trigger AS $$
DECLARE p qms_test_plans%ROWTYPE;
BEGIN
  IF NEW.result = 'pending' AND NEW.measured_value IS NOT NULL THEN
    SELECT * INTO p FROM qms_test_plans WHERE id = NEW.test_plan_id;
    IF p.lower_limit IS NOT NULL OR p.upper_limit IS NOT NULL THEN
      IF (p.lower_limit IS NOT NULL AND NEW.measured_value < p.lower_limit)
         OR (p.upper_limit IS NOT NULL AND NEW.measured_value > p.upper_limit) THEN
        NEW.result := 'fail';
      ELSE
        NEW.result := 'pass';
      END IF;
    END IF;
  END IF;
  RETURN NEW;
END $$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS trg_qms_test_results_eval ON qms_test_results;
CREATE TRIGGER trg_qms_test_results_eval BEFORE INSERT OR UPDATE ON qms_test_results
  FOR EACH ROW EXECUTE FUNCTION qms_test_result_eval();

-- QMS: a failed test automatically raises a non-conformance
CREATE OR REPLACE FUNCTION qms_test_result_ncr() RETURNS trigger AS $$
DECLARE p_name text;
BEGIN
  IF NEW.result = 'fail' AND NOT EXISTS (SELECT 1 FROM qms_nonconformances WHERE test_result_id = NEW.id) THEN
    SELECT name INTO p_name FROM qms_test_plans WHERE id = NEW.test_plan_id;
    INSERT INTO qms_nonconformances (title, description, source, severity, lot_number, test_result_id, product_or_process)
    VALUES ('Failed test: ' || p_name,
            'Auto-raised from failed test result ' || NEW.id || COALESCE(' (measured value ' || NEW.measured_value::text || ')', ''),
            'test', 'minor', NEW.lot_number, NEW.id, p_name);
  END IF;
  RETURN NEW;
END $$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS trg_qms_test_results_ncr ON qms_test_results;
CREATE TRIGGER trg_qms_test_results_ncr AFTER INSERT OR UPDATE OF result ON qms_test_results
  FOR EACH ROW EXECUTE FUNCTION qms_test_result_ncr();

-- QMS: a non-conformance cannot be closed while corrective actions are open
CREATE OR REPLACE FUNCTION qms_ncr_guard() RETURNS trigger AS $$
BEGIN
  IF NEW.status = 'closed' THEN
    IF EXISTS (SELECT 1 FROM qms_corrective_actions
               WHERE nonconformance_id = NEW.id AND status NOT IN ('verified', 'closed', 'cancelled')) THEN
      RAISE EXCEPTION 'Cannot close nonconformance %: corrective actions are still open', NEW.ncr_number
        USING ERRCODE = '23514';
    END IF;
    IF NEW.closed_at IS NULL THEN NEW.closed_at := now(); END IF;
  ELSE
    NEW.closed_at := NULL;
  END IF;
  RETURN NEW;
END $$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS trg_qms_ncr_guard ON qms_nonconformances;
CREATE TRIGGER trg_qms_ncr_guard BEFORE INSERT OR UPDATE ON qms_nonconformances
  FOR EACH ROW EXECUTE FUNCTION qms_ncr_guard();

-- Documents: versions are immutable
CREATE OR REPLACE FUNCTION doc_version_immutable() RETURNS trigger AS $$
BEGIN
  RAISE EXCEPTION 'Document versions are immutable' USING ERRCODE = '23514';
END $$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS trg_doc_versions_immutable ON doc_versions;
CREATE TRIGGER trg_doc_versions_immutable BEFORE UPDATE ON doc_versions
  FOR EACH ROW EXECUTE FUNCTION doc_version_immutable();

-- Documents: records and legal holds cannot be deleted
CREATE OR REPLACE FUNCTION doc_delete_guard() RETURNS trigger AS $$
BEGIN
  IF OLD.legal_hold THEN
    RAISE EXCEPTION 'Document % is under legal hold and cannot be deleted', OLD.doc_number USING ERRCODE = '23514';
  END IF;
  IF OLD.is_record THEN
    RAISE EXCEPTION 'Document % is a controlled record; use the disposition process', OLD.doc_number USING ERRCODE = '23514';
  END IF;
  RETURN OLD;
END $$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS trg_doc_delete_guard ON doc_documents;
CREATE TRIGGER trg_doc_delete_guard BEFORE DELETE ON doc_documents
  FOR EACH ROW EXECUTE FUNCTION doc_delete_guard();

-- =====================================================================
-- Reference data (only when tables are empty)
-- =====================================================================
INSERT INTO doc_workflows (name, description, steps)
SELECT 'Standard Review & Approval', 'Manager review followed by admin approval',
       '[{"name":"Review","role":"manager"},{"name":"Approval","role":"admin"}]'::jsonb
WHERE NOT EXISTS (SELECT 1 FROM doc_workflows);

INSERT INTO doc_retention_policies (name, description, retention_years, disposition_action)
SELECT v.name, v.description, v.years, v.action FROM (VALUES
  ('General business records', 'Default retention for business records', 7, 'archive'),
  ('Quality records', 'ISO 9001 quality records', 10, 'review'),
  ('Safety & incident records', 'Occupational health and safety records', 30, 'archive'),
  ('Transient documents', 'Drafts and working copies', 1, 'destroy')
) AS v(name, description, years, action)
WHERE NOT EXISTS (SELECT 1 FROM doc_retention_policies);

INSERT INTO crm_scoring_rules (name, field, operator, value, points)
SELECT v.name, v.field, v.operator, v.value, v.points FROM (VALUES
  ('Referral source', 'source', 'eq', 'referral'::text, 20),
  ('Event source', 'source', 'eq', 'event'::text, 15),
  ('Has email', 'email', 'not_null', NULL::text, 10),
  ('Has phone', 'phone', 'not_null', NULL::text, 10),
  ('Has company', 'company', 'not_null', NULL::text, 10),
  ('Qualified status', 'status', 'eq', 'qualified'::text, 25)
) AS v(name, field, operator, value, points)
WHERE NOT EXISTS (SELECT 1 FROM crm_scoring_rules);
"""


# Raw SQL appended to upgrade scripts (keeps upgraded databases identical to fresh installs)
EXTRA_UPGRADE_SQL = {3: """
-- open prospects may not have an account yet; deleting an account must not destroy sales history
ALTER TABLE crm_opportunities ALTER COLUMN account_id DROP NOT NULL;
ALTER TABLE crm_opportunities DROP CONSTRAINT IF EXISTS crm_opportunities_account_id_fkey;
ALTER TABLE crm_opportunities ADD CONSTRAINT crm_opportunities_account_id_fkey
  FOREIGN KEY (account_id) REFERENCES crm_accounts(id) ON DELETE SET NULL;
"""}

EXTRA_UPGRADE_SQL[4] = """
-- roles: add finance and hr
ALTER TABLE users DROP CONSTRAINT IF EXISTS users_role_check;
ALTER TABLE users ADD CONSTRAINT users_role_check CHECK (role IN ('admin', 'manager', 'user', 'auditor', 'finance', 'hr'));

-- sales tax payable (used when project invoices carrying tax are posted to the ledger)
INSERT INTO accounts (code, name, account_type) VALUES ('2200', 'Sales Tax Payable', 'liability') ON CONFLICT (code) DO NOTHING;

-- keep project invoices in step with the receivable they were posted as
CREATE OR REPLACE FUNCTION sync_pm_invoice_from_ledger() RETURNS trigger AS $$
BEGIN
  IF NEW.status IS DISTINCT FROM OLD.status THEN
    UPDATE pm_invoices SET
      status = CASE NEW.status WHEN 'paid' THEN 'paid' WHEN 'void' THEN 'void' ELSE status END,
      paid_at = CASE WHEN NEW.status = 'paid' THEN now() ELSE paid_at END
    WHERE erp_invoice_id = NEW.id AND status <> 'void';
  END IF;
  RETURN NEW;
END $$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS trg_invoices_sync_pm ON invoices;
CREATE TRIGGER trg_invoices_sync_pm AFTER UPDATE OF status ON invoices FOR EACH ROW EXECUTE FUNCTION sync_pm_invoice_from_ledger();
"""
