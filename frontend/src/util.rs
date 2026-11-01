//! Pure helpers (formatting, query strings, response normalisation). No browser APIs, so they are unit-tested natively.
use serde_json::Value;

/// `purchase_order_lines` / `purchase-order-lines` -> "Purchase order lines"
pub fn title_case(s: &str) -> String {
    let t = s.replace(['_', '-'], " ");
    let mut c = t.trim().chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// 1234567.891 -> "1,234,567.89"
pub fn money(v: f64) -> String {
    let neg = v < 0.0;
    let fixed = format!("{:.2}", v.abs());
    let (int, dec) = fixed.split_once('.').unwrap_or((&fixed, "00"));
    format!("{}{}.{dec}", if neg { "-" } else { "" }, group3(int))
}

pub fn integer(v: f64) -> String {
    let neg = v < 0.0;
    format!("{}{}", if neg { "-" } else { "" }, group3(&format!("{:.0}", v.abs())))
}

fn group3(int: &str) -> String {
    let mut out = String::new();
    for (i, ch) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// 1_250_000 -> "1.25M" (used on stat tiles and chart axes)
pub fn compact(v: f64) -> String {
    let a = v.abs();
    let (n, suffix) = if a >= 1e9 { (v / 1e9, "B") } else if a >= 1e6 { (v / 1e6, "M") } else if a >= 1e4 { (v / 1e3, "k") } else { return if v.fract() == 0.0 { integer(v) } else { format!("{v:.2}") } };
    let s = format!("{n:.2}");
    format!("{}{suffix}", s.trim_end_matches('0').trim_end_matches('.'))
}

pub fn is_money_key(k: &str) -> bool {
    ["amount", "cost", "value", "budget", "cash", "payroll", "receivable", "payable", "revenue", "price", "salary", "balance", "total", "pipeline", "benefit", "spend", "debit", "credit", "paid"]
        .iter()
        .any(|w| k.contains(w))
}

/// Format a JSON scalar for display in a table cell or tile. `key` hints at units (money vs counts).
pub fn display(key: &str, v: &Value) -> String {
    match v {
        Value::Null => "—".into(),
        Value::Bool(b) => if *b { "Yes".into() } else { "No".into() },
        Value::Number(n) => {
            let f = n.as_f64().unwrap_or(0.0);
            if n.is_i64() || n.is_u64() {
                if key.ends_with("_id") || key == "id" { n.to_string() } else { integer(f) }
            } else if key.contains("pct") || key.contains("rate") || key.contains("probability") || key.contains("ratio") {
                if f.abs() <= 1.0 && !key.contains("pct") { format!("{:.1}%", f * 100.0) } else { format!("{f:.1}%") }
            } else if is_money_key(key) { money(f) } else { format!("{f:.2}") }
        }
        Value::String(s) => {
            // exact-decimal strings from the API (numeric(18,2))
            if is_money_key(key) && s.parse::<f64>().is_ok() && !key.ends_with("_id") && !key.ends_with("_code") {
                return money(s.parse::<f64>().unwrap_or(0.0));
            }
            if s.len() >= 20 && s.as_bytes().get(10) == Some(&b'T') {
                return s.chars().take(16).collect::<String>().replace('T', " ");
            }
            s.chars().take(140).collect()
        }
        Value::Array(a) => format!("{} item{}", a.len(), if a.len() == 1 { "" } else { "s" }),
        Value::Object(_) => v.to_string().chars().take(100).collect(),
    }
}

pub fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// Short id for tables: first 8 chars of a UUID.
pub fn short_id(v: &Value) -> String {
    match v {
        Value::String(s) if s.len() > 12 => format!("{}…", &s[..8]),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The id of a row as it appears in a URL (UUID string or numeric id).
pub fn row_id(row: &Value, key: &str) -> Option<String> {
    match row.get(key)? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// A human label for a record, used by FK pickers and breadcrumbs.
pub fn row_label(row: &Value) -> String {
    for k in ["name", "title", "full_name", "email", "code", "sku", "subject", "last_name", "invoice_number", "po_number", "number", "label"] {
        if let Some(s) = row.get(k).and_then(|v| v.as_str()) {
            if !s.is_empty() {
                let extra = row.get("first_name").and_then(|v| v.as_str()).filter(|_| k == "last_name");
                return match extra { Some(f) => format!("{f} {s}"), None => s.to_string() };
            }
        }
    }
    row.get("id").map(short_id).unwrap_or_default()
}

/// Rows plus optional total from any list response shape the backend uses:
/// `{data:[..], total}` (generated CRUD), a bare array (finance, HR, supply chain), or an object wrapping one array.
pub fn rows_of(v: &Value) -> (Vec<Value>, Option<u64>) {
    match v {
        Value::Array(a) => (a.clone(), None),
        Value::Object(m) => {
            let total = m.get("total").and_then(|t| t.as_u64());
            if let Some(Value::Array(a)) = m.get("data").or_else(|| m.get("items")).or_else(|| m.get("rows")) {
                return (a.clone(), total);
            }
            let arrays: Vec<&Value> = m.values().filter(|x| x.is_array()).collect();
            if arrays.len() == 1 {
                if let Value::Array(a) = arrays[0] {
                    return (a.clone(), total);
                }
            }
            (vec![v.clone()], None)
        }
        _ => (vec![], None),
    }
}

/// Column names for an auto-generated table: union of keys in the first rows, id/noise last, capped.
pub fn auto_columns(rows: &[Value], max: usize) -> Vec<String> {
    let mut cols: Vec<String> = Vec::new();
    for r in rows.iter().take(25) {
        if let Value::Object(m) = r {
            for k in m.keys() {
                if !cols.contains(k) {
                    cols.push(k.clone());
                }
            }
        }
    }
    let noisy = |k: &str| matches!(k, "id" | "created_at" | "updated_at") || k.ends_with("_id") || k == "metadata";
    let simple = |k: &str| rows.iter().take(25).any(|r| r.get(k).map(|v| !v.is_object() && !v.is_array()).unwrap_or(false));
    let mut ordered: Vec<String> = cols.iter().filter(|c| !noisy(c) && simple(c)).cloned().collect();
    ordered.extend(cols.iter().filter(|c| (c.as_str() == "id" || c.ends_with("_id")) && c.as_str() != "id" && simple(c)).take(1).cloned());
    ordered.truncate(max);
    if ordered.is_empty() {
        ordered = cols.into_iter().take(max).collect();
    }
    ordered
}

/// Colour family for a status word: "ok" | "warn" | "err" | "info" | "" (neutral).
pub fn tone_for(value: &str) -> &'static str {
    let v = value.to_lowercase().replace(' ', "_");
    const OK: [&str; 18] = ["posted", "paid", "approved", "completed", "done", "closed_won", "verified", "active", "resolved", "received", "shipped", "delivered", "effective", "passed", "pass", "healthy", "converted", "reconciled"];
    const WARN: [&str; 14] = ["pending", "draft", "submitted", "in_progress", "investigating", "medium", "partially_received", "partially_shipped", "on_hold", "review", "proposed", "planned", "scheduled", "backordered"];
    const ERR: [&str; 15] = ["failed", "fail", "rejected", "cancelled", "void", "voided", "overdue", "critical", "high", "closed_lost", "blocked", "lost", "terminated", "disputed", "quarantined"];
    if OK.contains(&v.as_str()) { "ok" } else if WARN.contains(&v.as_str()) { "warn" } else if ERR.contains(&v.as_str()) { "err" } else if matches!(v.as_str(), "open" | "new" | "qualified" | "low" | "confirmed") { "info" } else { "" }
}

// ------------------------------------------------------------------ query strings

pub fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `/path` + params -> `/path?a=1&b=x%20y`; empty values are skipped.
pub fn with_query(path: &str, params: &[(String, String)]) -> String {
    let q: Vec<String> = params.iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v))).collect();
    if q.is_empty() {
        path.to_string()
    } else {
        format!("{path}{}{}", if path.contains('?') { '&' } else { '?' }, q.join("&"))
    }
}

// ------------------------------------------------------------------ roles

/// Mirror of the backend role policy (`auth::authorize`), used to hide controls the server would reject.
pub fn role_allows(role: &str, write: bool, path: &str) -> bool {
    let under = |p: &str| path == p || path.starts_with(&format!("{p}/"));
    enum A { Finance, Hr, Audit, Ops }
    let area = if under("/api/finance") || under("/api/risk") { A::Finance } else if under("/api/hr") { A::Hr } else if under("/api/sys") { A::Audit } else { A::Ops };
    let (r, w) = match (role, &area) {
        ("admin" | "manager", _) => (true, true),
        ("auditor", _) => (true, false),
        (_, A::Audit) => (false, false),
        ("user", A::Ops) => (true, true),
        ("user", _) => (false, false),
        ("finance", A::Finance) => (true, true),
        ("finance", A::Ops) => (true, false),
        ("hr", A::Hr) => (true, true),
        ("hr", A::Ops) => (true, false),
        _ => (false, false),
    };
    if write { w } else { r }
}

pub fn can_delete(role: &str, path: &str) -> bool {
    matches!(role, "admin" | "manager") || (role == "finance" && (path.starts_with("/api/finance") || path.starts_with("/api/risk"))) || (role == "hr" && path.starts_with("/api/hr"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn formats() {
        assert_eq!(title_case("purchase_order-lines"), "Purchase order lines");
        assert_eq!(money(1234567.891), "1,234,567.89");
        assert_eq!(money(-5.0), "-5.00");
        assert_eq!(integer(999.0), "999");
        assert_eq!(integer(1000.0), "1,000");
        assert_eq!(compact(1_250_000.0), "1.25M");
        assert_eq!(compact(42.0), "42");
        assert_eq!(compact(12_000.0), "12k");
    }

    #[test]
    fn display_values() {
        assert_eq!(display("budget", &json!("12500.00")), "12,500.00");
        assert_eq!(display("invoice_code", &json!("12500")), "12500");
        assert_eq!(display("is_active", &json!(true)), "Yes");
        assert_eq!(display("x", &Value::Null), "—");
        assert_eq!(display("created_at", &json!("2026-10-31T07:00:00+00:00")), "2026-10-31 07:00");
        assert_eq!(display("win_rate", &json!(0.627)), "62.7%");
        assert_eq!(display("items", &json!([1, 2])), "2 items");
    }

    #[test]
    fn list_shapes() {
        assert_eq!(rows_of(&json!([{"a":1}])).0.len(), 1);
        let (r, t) = rows_of(&json!({"data":[{"a":1},{"a":2}],"total":9}));
        assert_eq!((r.len(), t), (2, Some(9)));
        assert_eq!(rows_of(&json!({"things":[1,2,3]})).0.len(), 3);
        // a plain report object stays a single row
        assert_eq!(rows_of(&json!({"a":1,"b":2})).0.len(), 1);
    }

    #[test]
    fn auto_cols() {
        let rows = vec![json!({"id":"x","name":"A","status":"open","amount":"5","items":[1],"supplier_id":"s"})];
        let c = auto_columns(&rows, 6);
        let mut first: Vec<&str> = c[..3].iter().map(|s| s.as_str()).collect();
        first.sort();
        assert_eq!(first, ["amount", "name", "status"]);
        assert!(!c.contains(&"items".to_string()));
        assert!(!c.contains(&"id".to_string()));
    }

    #[test]
    fn tones() {
        assert_eq!(tone_for("closed_won"), "ok");
        assert_eq!(tone_for("In progress"), "warn");
        assert_eq!(tone_for("cancelled"), "err");
        assert_eq!(tone_for("open"), "info");
        assert_eq!(tone_for("whatever"), "");
    }

    #[test]
    fn queries() {
        assert_eq!(percent_encode("a b&c"), "a%20b%26c");
        let p = vec![("status".to_string(), "in progress".to_string()), ("x".to_string(), String::new())];
        assert_eq!(with_query("/api/a", &p), "/api/a?status=in%20progress");
        assert_eq!(with_query("/api/a?z=1", &p), "/api/a?z=1&status=in%20progress");
    }

    #[test]
    fn labels() {
        assert_eq!(row_label(&json!({"id":"abcdef12-3456","first_name":"Ada","last_name":"Lovelace"})), "Ada Lovelace");
        assert_eq!(row_label(&json!({"id":"abcdef12-3456-7890-aaaa-bbbbbbbbbbbb"})), "abcdef12…");
        assert_eq!(row_id(&json!({"id":7}), "id").as_deref(), Some("7"));
    }

    // Same matrix as `auth::policy_matrix` in the backend.
    #[test]
    fn role_policy_mirrors_backend() {
        assert!(role_allows("admin", true, "/api/finance/invoices"));
        assert!(!role_allows("auditor", true, "/api/crm/accounts"));
        assert!(role_allows("auditor", false, "/api/hr/employees"));
        assert!(!role_allows("user", false, "/api/finance/invoices"));
        assert!(role_allows("user", true, "/api/crm/accounts"));
        assert!(!role_allows("user", false, "/api/sys/audit-log"));
        assert!(role_allows("finance", true, "/api/risk/integrity-checks"));
        assert!(!role_allows("finance", true, "/api/crm/accounts"));
        assert!(!role_allows("finance", false, "/api/hr/employees"));
        assert!(role_allows("hr", true, "/api/hr/employees"));
        assert!(!role_allows("hr", false, "/api/finance/accounts"));
        assert!(can_delete("manager", "/api/crm/accounts") && !can_delete("user", "/api/crm/accounts"));
        assert!(can_delete("hr", "/api/hr/employees") && !can_delete("hr", "/api/finance/bills"));
    }
}
