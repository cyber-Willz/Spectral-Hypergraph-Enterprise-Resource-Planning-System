use enterprise_suite::{auth, db, modules, state::AppState};
use rust_decimal::Decimal;
use sea_orm::{ConnectOptions, Database};
use std::{sync::Arc, time::Duration};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,sqlx=warn".into()))
        .init();

    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let secret = std::env::var("JWT_SECRET").expect("JWT_SECRET must be set");
    if secret.len() < 32 {
        panic!("JWT_SECRET must be at least 32 characters");
    }
    let bind = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into());

    let mut opt = ConnectOptions::new(url);
    opt.max_connections(20).connect_timeout(Duration::from_secs(10)).sqlx_logging(false);
    let db = Database::connect(opt).await?;
    db::migrate(&db).await?;

    let payroll_tax_rate: Decimal = std::env::var("PAYROLL_TAX_RATE").unwrap_or_else(|_| "0.10".into()).parse()
        .expect("PAYROLL_TAX_RATE must be a decimal such as 0.10");
    if payroll_tax_rate < Decimal::ZERO || payroll_tax_rate >= Decimal::ONE { panic!("PAYROLL_TAX_RATE must be in [0, 1)"); }
    let st = AppState { db, jwt_secret: Arc::new(secret), payroll_tax_rate };
    let admin_email = std::env::var("ADMIN_EMAIL").unwrap_or_else(|_| "admin@example.com".into());
    let admin_pw = std::env::var("ADMIN_PASSWORD").unwrap_or_else(|_| "ChangeMe123!".into());
    if let Err(e) = auth::bootstrap_admin(&st, &admin_email, &admin_pw).await {
        tracing::warn!("bootstrap admin skipped: {e:?}");
    }

    if let Ok(path) = std::env::var("IMPORT_PM_CSV") {
        match std::fs::read_to_string(&path) {
            Ok(text) => match modules::portfolio::import_csv(&st.db, &text, &path, None, false).await {
                Ok(r) => tracing::info!("startup import of {path}: {} inserted, {} updated, {} rejected", r["inserted"], r["updated"], r["rejected"]),
                Err(e) => tracing::warn!("startup import failed: {e:?}"),
            },
            Err(e) => tracing::warn!("IMPORT_PM_CSV {path} unreadable: {e}"),
        }
    }

    let app = enterprise_suite::build_app(st);

    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!("listening on {bind}");
    axum::serve(listener, app).await?;
    Ok(())
}
