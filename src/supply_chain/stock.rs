//! Low-level stock primitives. All of them are meant to run inside a transaction and rely on
//! single-statement atomic updates (plus CHECK constraints) so concurrent requests cannot oversell.
use chrono::{NaiveDate, Utc};
use sea_orm::{ActiveModelTrait, ConnectionTrait, DbBackend, DbErr, Set, Statement};
use uuid::Uuid;

use crate::supply_chain::{
    entities::{location, product, stock_movement},
    error::AppError,
};

/// Add `qty` units to a (product, location, lot) bucket, creating it when missing.
pub async fn add_stock<C: ConnectionTrait>(
    db: &C,
    product_id: Uuid,
    location_id: Uuid,
    lot: &str,
    qty: i64,
    expiry: Option<NaiveDate>,
) -> Result<(), DbErr> {
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO stock_levels (id, product_id, location_id, lot_number, quantity, reserved, expiry_date, updated_at) \
         VALUES ($1, $2, $3, $4, $5, 0, $6, now()) \
         ON CONFLICT (product_id, location_id, lot_number) \
         DO UPDATE SET quantity = stock_levels.quantity + EXCLUDED.quantity, \
                       expiry_date = LEAST(stock_levels.expiry_date, EXCLUDED.expiry_date), \
                       updated_at = now()",
        [
            Uuid::new_v4().into(),
            product_id.into(),
            location_id.into(),
            lot.into(),
            qty.into(),
            expiry.into(),
        ],
    ))
    .await?;
    Ok(())
}

/// Remove `qty` *available* (on-hand minus reserved minus quarantined) units. Returns false when not enough is available.
pub async fn remove_stock<C: ConnectionTrait>(
    db: &C,
    product_id: Uuid,
    location_id: Uuid,
    lot: &str,
    qty: i64,
) -> Result<bool, DbErr> {
    let res = db
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE stock_levels SET quantity = quantity - $1, updated_at = now() \
             WHERE product_id = $2 AND location_id = $3 AND lot_number = $4 \
               AND quantity - reserved - blocked >= $1",
            [qty.into(), product_id.into(), location_id.into(), lot.into()],
        ))
        .await?;
    Ok(res.rows_affected() == 1)
}

#[allow(clippy::too_many_arguments)]
pub async fn record_movement<C: ConnectionTrait>(
    db: &C,
    product_id: Uuid,
    location_id: Uuid,
    lot: &str,
    movement_type: &str,
    delta: i64,
    reference_type: Option<&str>,
    reference_id: Option<Uuid>,
    note: Option<String>,
) -> Result<(), DbErr> {
    stock_movement::ActiveModel {
        id: Set(Uuid::new_v4()),
        product_id: Set(product_id),
        location_id: Set(location_id),
        lot_number: Set(lot.to_string()),
        movement_type: Set(movement_type.to_string()),
        quantity_delta: Set(delta),
        reference_type: Set(reference_type.map(str::to_string)),
        reference_id: Set(reference_id),
        note: Set(note),
        created_at: Set(Utc::now()),
    }
    .insert(db)
    .await?;
    Ok(())
}

/// Cold-chain / controlled-storage rule: a product may only be put away in a bin of its own class.
pub fn ensure_storage_compatible(p: &product::Model, loc: &location::Model) -> Result<(), AppError> {
    if p.storage_class != loc.storage_class {
        return Err(AppError::conflict(format!(
            "product {} requires '{}' storage but location {} is '{}'",
            p.sku, p.storage_class, loc.code, loc.storage_class
        )));
    }
    Ok(())
}

/// Default expiry for shelf-life products when the caller does not supply one.
pub fn default_expiry(p: &product::Model, from: NaiveDate, given: Option<NaiveDate>) -> Option<NaiveDate> {
    given.or_else(|| {
        p.shelf_life_days
            .and_then(|d| from.checked_add_signed(chrono::Duration::days(d as i64)))
    })
}
