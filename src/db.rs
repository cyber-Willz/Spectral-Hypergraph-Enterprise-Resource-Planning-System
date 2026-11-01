use crate::error::AppResult;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, FromQueryResult, JsonValue, Statement, Value};

const MIGRATIONS: &[(&str, &str)] = &[
    ("001_schema", include_str!("../migrations/001_schema.sql")),
    ("002_pm_portfolio", include_str!("../migrations/002_pm_portfolio.sql")),
    ("003_crm_datasets", include_str!("../migrations/003_crm_datasets.sql")),
    ("004_erp_core", include_str!("../migrations/004_erp_core.sql")),
    ("005_erp_risk", include_str!("../migrations/005_erp_risk.sql")),
    ("006_erp_hr_attributes", include_str!("../migrations/006_erp_hr_attributes.sql")),
    ("007_erp_integration", include_str!("../migrations/007_erp_integration.sql")),
    ("008_scm_core", include_str!("../migrations/008_scm_core.sql")),
    ("009_scm_ops_intelligence", include_str!("../migrations/009_scm_ops_intelligence.sql")),
    ("010_scm_closed_loop", include_str!("../migrations/010_scm_closed_loop.sql")),
    ("011_scm_dataset_ops", include_str!("../migrations/011_scm_dataset_ops.sql")),
    ("012_scm_cognitive", include_str!("../migrations/012_scm_cognitive.sql")),
    ("013_scm_integration", include_str!("../migrations/013_scm_integration.sql")),
];

pub async fn migrate(db: &DatabaseConnection) -> Result<(), sea_orm::DbErr> {
    db.execute_unprepared("CREATE TABLE IF NOT EXISTS _schema_migrations (version text PRIMARY KEY, applied_at timestamptz NOT NULL DEFAULT now())").await?;
    for (version, sql) in MIGRATIONS {
        let applied = JsonValue::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres, "SELECT version FROM _schema_migrations WHERE version = $1", [Value::from(*version)]))
            .one(db).await?;
        if applied.is_none() {
            tracing::info!("applying migration {version}");
            db.execute_unprepared(sql).await?;
            db.execute_unprepared(&format!("INSERT INTO _schema_migrations(version) VALUES ('{version}')")).await?;
        }
    }
    Ok(())
}

/// Run a SELECT and return rows as JSON objects. Cast numerics to float8 in SQL for JSON numbers.
pub async fn rows(db: &impl ConnectionTrait, sql: &str, vals: Vec<Value>) -> AppResult<Vec<JsonValue>> {
    Ok(JsonValue::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres, sql, vals)).all(db).await?)
}

pub async fn row(db: &impl ConnectionTrait, sql: &str, vals: Vec<Value>) -> AppResult<JsonValue> {
    Ok(JsonValue::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres, sql, vals))
        .one(db).await?.unwrap_or(JsonValue::Null))
}
