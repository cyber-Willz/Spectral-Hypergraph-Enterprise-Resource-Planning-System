//! SeaORM entities. Status/type columns are stored as TEXT and guarded by CHECK constraints.
#![allow(clippy::derive_partial_eq_without_eq)]

macro_rules! no_relations {
    () => {
        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}
        impl ActiveModelBehavior for ActiveModel {}
    };
}

pub mod warehouse {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "warehouses")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub code: String,
        pub name: String,
        pub address: Option<String>,
        pub city: Option<String>,
        pub country: Option<String>,
        pub active: bool,
        pub created_at: DateTimeUtc,
    }
    no_relations!();
}

pub mod location {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "locations")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub warehouse_id: Uuid,
        pub code: String,
        pub zone: Option<String>,
        pub capacity: Option<i64>,
        pub storage_class: String,
        pub active: bool,
        pub created_at: DateTimeUtc,
    }
    no_relations!();
}

pub mod product {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "products")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub sku: String,
        pub name: String,
        pub description: Option<String>,
        pub unit_of_measure: String,
        pub unit_cost: Decimal,
        pub list_price: Decimal,
        pub reorder_point: i64,
        pub reorder_qty: i64,
        pub criticality_class: String,
        pub storage_class: String,
        pub shelf_life_days: Option<i32>,
        pub is_repairable: bool,
        pub review_period_days: i32,
        pub lead_time_days: i32,
        pub holding_cost_rate: Decimal,
        pub stockout_cost: Option<Decimal>,
        pub aql_defect_pct: Option<f64>,
        pub active: bool,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }
    no_relations!();
}

pub mod supplier {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "suppliers")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub code: String,
        pub name: String,
        pub contact_email: Option<String>,
        pub phone: Option<String>,
        pub lead_time_days: i32,
        pub country: Option<String>,
        pub region: Option<String>,
        pub tier: Option<i32>,
        pub esg_score: Option<f64>,
        pub financial_stability_score: Option<f64>,
        pub geopolitical_risk_index: Option<f64>,
        pub alternatives_available: Option<i32>,
        pub environmental_compliance: Option<f64>,
        pub previous_disruptions: Option<i32>,
        pub payment_terms_days: i32,
        pub active: bool,
        pub created_at: DateTimeUtc,
    }
    no_relations!();
}

pub mod customer {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "scm_customers")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub code: String,
        pub name: String,
        pub email: Option<String>,
        pub address: Option<String>,
        pub created_at: DateTimeUtc,
    }
    no_relations!();
}

pub mod carrier {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "carriers")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub code: String,
        pub name: String,
        pub tracking_url_template: Option<String>,
        pub active: bool,
    }
    no_relations!();
}

pub mod stock_level {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "stock_levels")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub product_id: Uuid,
        pub location_id: Uuid,
        pub lot_number: String,
        pub quantity: i64,
        pub reserved: i64,
        pub blocked: i64,
        pub expiry_date: Option<Date>,
        pub updated_at: DateTimeUtc,
    }
    no_relations!();
}

pub mod stock_movement {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "stock_movements")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub product_id: Uuid,
        pub location_id: Uuid,
        pub lot_number: String,
        pub movement_type: String,
        pub quantity_delta: i64,
        pub reference_type: Option<String>,
        pub reference_id: Option<Uuid>,
        pub note: Option<String>,
        pub created_at: DateTimeUtc,
    }
    no_relations!();
}

pub mod purchase_order {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "purchase_orders")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub po_number: String,
        pub supplier_id: Uuid,
        pub warehouse_id: Uuid,
        pub status: String,
        pub expected_date: Option<Date>,
        pub notes: Option<String>,
        pub order_date: Date,
        pub po_type: String,
        pub currency: String,
        pub payment_terms_days: i32,
        pub contract_id: Option<Uuid>,
        pub auto_generated: bool,
        pub incoterm: Option<String>,
        pub transport_mode: Option<String>,
        pub freight_cost: Decimal,
        pub insurance_cost: Decimal,
        pub freight_included: bool,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }
    no_relations!();
}

pub mod purchase_order_line {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "purchase_order_lines")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub purchase_order_id: Uuid,
        pub product_id: Uuid,
        pub quantity_ordered: i64,
        pub quantity_received: i64,
        pub unit_cost: Decimal,
        pub promised_date: Option<Date>,
        pub budget_unit_cost: Option<Decimal>,
    }
    no_relations!();
}

pub mod sales_order {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "sales_orders")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub so_number: String,
        pub customer_id: Uuid,
        pub warehouse_id: Uuid,
        pub status: String,
        pub ship_to: Option<String>,
        pub fulfillment_stage: String,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }
    no_relations!();
}

pub mod sales_order_line {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "sales_order_lines")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub sales_order_id: Uuid,
        pub product_id: Uuid,
        pub quantity: i64,
        pub quantity_allocated: i64,
        pub quantity_shipped: i64,
        pub unit_price: Decimal,
    }
    no_relations!();
}

pub mod allocation {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "allocations")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub sales_order_line_id: Uuid,
        pub stock_level_id: Uuid,
        pub quantity: i64,
        pub status: String,
        pub shipment_id: Option<Uuid>,
        pub created_at: DateTimeUtc,
    }
    no_relations!();
}

pub mod shipment {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "shipments")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub shipment_number: String,
        pub sales_order_id: Uuid,
        pub carrier_id: Option<Uuid>,
        pub tracking_number: Option<String>,
        pub status: String,
        pub shipped_at: Option<DateTimeUtc>,
        pub delivered_at: Option<DateTimeUtc>,
        pub expected_delivery_date: Option<Date>,
        pub freight_cost: Decimal,
        pub delay_risk: Option<f64>,
        pub disruption_risk: Option<f64>,
        pub risk_context: Option<Json>,
        pub created_at: DateTimeUtc,
    }
    no_relations!();
}

pub mod shipment_event {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "shipment_events")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub shipment_id: Uuid,
        pub status: String,
        pub location: Option<String>,
        pub description: Option<String>,
        pub occurred_at: DateTimeUtc,
    }
    no_relations!();
}

pub mod contract {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "contracts")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub supplier_id: Uuid,
        pub contract_number: String,
        pub contract_type: String,
        pub start_date: Date,
        pub end_date: Date,
        pub currency: String,
        pub created_at: DateTimeUtc,
    }
    no_relations!();
}

pub mod goods_receipt {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "goods_receipts")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub purchase_order_id: Uuid,
        pub purchase_order_line_id: Uuid,
        pub product_id: Uuid,
        pub location_id: Uuid,
        pub lot_number: String,
        pub quantity: i64,
        pub received_on: Date,
        pub rejected_qty: i64,
        pub quality_incident_id: Option<Uuid>,
        pub defect_rate_pct: Option<f64>,
        pub created_at: DateTimeUtc,
    }
    no_relations!();
}

pub mod quality_incident {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "quality_incidents")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub supplier_id: Option<Uuid>,
        pub product_id: Uuid,
        pub purchase_order_id: Option<Uuid>,
        pub stock_level_id: Option<Uuid>,
        pub severity: String,
        pub defect_type: Option<String>,
        pub quarantined_qty: i64,
        pub released_qty: i64,
        pub scrapped_qty: i64,
        pub reworked_qty: i64,
        pub status: String,
        pub note: Option<String>,
        pub occurred_on: Date,
        pub created_at: DateTimeUtc,
    }
    no_relations!();
}

pub mod supplier_invoice {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "supplier_invoices")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub purchase_order_id: Uuid,
        pub invoice_number: String,
        pub invoice_date: Date,
        pub due_date: Date,
        pub currency: String,
        pub total: Decimal,
        pub status: String,
        pub match_notes: Option<String>,
        pub paid_on: Option<Date>,
        pub created_at: DateTimeUtc,
    }
    no_relations!();
}

pub mod supplier_invoice_line {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "supplier_invoice_lines")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub invoice_id: Uuid,
        pub purchase_order_line_id: Uuid,
        pub quantity: i64,
        pub unit_price: Decimal,
    }
    no_relations!();
}

pub mod production_run {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "production_runs")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub product_id: Uuid,
        pub location_id: Uuid,
        pub lot_number: String,
        pub quantity_started: i64,
        pub quantity_good: i64,
        pub quantity_scrap: i64,
        pub hours: f64,
        pub cost: Decimal,
        pub produced_on: Date,
        pub created_at: DateTimeUtc,
    }
    no_relations!();
}
