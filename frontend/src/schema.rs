//! Resource metadata (generated from `tools/spec.py`) and the pure helpers built on it.
use serde_json::{json, Value};

#[derive(Debug, PartialEq)]
pub struct ColDef {
    pub name: &'static str,
    /// s text · i int · bi bigint · d decimal · f float · b bool · dt date · ts timestamp · j json · u uuid
    pub ty: &'static str,
    pub req: bool,
    pub readonly: bool,
    pub choices: &'static [&'static str],
    pub fk: &'static str,
}

#[derive(Debug, PartialEq)]
pub struct ResDef {
    pub module: &'static str,
    pub slug: &'static str,
    pub table: &'static str,
    pub ops: &'static str,
    pub cols: &'static [ColDef],
}

pub use crate::schema_generated::RESOURCES;

pub fn find(module: &str, slug: &str) -> Option<&'static ResDef> {
    RESOURCES.iter().find(|r| r.module == module && r.slug == slug)
}
pub fn find_table(table: &str) -> Option<&'static ResDef> {
    RESOURCES.iter().find(|r| r.table == table)
}
pub fn in_module(module: &str) -> impl Iterator<Item = &'static ResDef> + '_ {
    RESOURCES.iter().filter(move |r| r.module == module)
}

/// Display names for the API modules.
pub fn module_title(m: &str) -> &'static str {
    match m {
        "crm" => "CRM",
        "pm" => "Projects",
        "grc" => "Risk & Compliance",
        "am" => "Assets",
        "ehs" => "Health & Safety",
        "qms" => "Quality",
        "docs" => "Documents",
        "sys" => "System",
        "finance" => "Finance",
        "hr" => "People",
        "risk" => "Financial Risk",
        "scm" => "Supply Chain",
        _ => "Other",
    }
}

impl ResDef {
    pub fn path(&self) -> String {
        format!("/api/{}/{}", self.module, self.slug)
    }
    pub fn title(&self) -> String {
        crate::util::title_case(self.slug)
    }
    pub fn can(&self, op: char) -> bool {
        self.ops.contains(op)
    }
    pub fn col(&self, name: &str) -> Option<&'static ColDef> {
        self.cols.iter().find(|c| c.name == name)
    }
    /// Columns shown in the list view: identifying text first, then status/enum, amounts and dates.
    pub fn list_columns(&self) -> Vec<&'static ColDef> {
        const NAMEISH: [&str; 12] = ["name", "title", "code", "full_name", "subject", "email", "last_name", "sku", "number", "label", "description", "path"];
        let mut out: Vec<&ColDef> = Vec::new();
        for n in NAMEISH {
            if let Some(c) = self.col(n) {
                if c.ty == "s" && out.len() < 2 {
                    out.push(c);
                }
            }
        }
        for c in self.cols {
            if out.len() >= 8 {
                break;
            }
            if out.iter().any(|o| o.name == c.name) || c.ty == "j" || c.fk != "" {
                continue;
            }
            let interesting = !c.choices.is_empty() || matches!(c.ty, "d" | "dt" | "i" | "b" | "f") || c.name.ends_with("_code");
            if interesting {
                out.push(c);
            }
        }
        if out.is_empty() {
            out.extend(self.cols.iter().filter(|c| c.ty != "j").take(5));
        }
        out
    }
    /// Columns the user may fill in on create/edit.
    pub fn editable(&self) -> impl Iterator<Item = &'static ColDef> + '_ {
        self.cols.iter().filter(|c| !c.readonly)
    }
    pub fn filterable(&self) -> Vec<&'static ColDef> {
        self.cols.iter().filter(|c| c.ty != "j").collect()
    }
}

/// Turn the raw text of a form field into the JSON value the API expects.
/// `Ok(None)` means "leave the field out" (blank optional field).
pub fn coerce(ty: &str, raw: &str) -> Result<Option<Value>, String> {
    let t = raw.trim();
    if t.is_empty() {
        return Ok(None);
    }
    Ok(Some(match ty {
        "i" | "bi" => json!(t.parse::<i64>().map_err(|_| format!("'{t}' is not a whole number"))?),
        "f" => json!(t.parse::<f64>().map_err(|_| format!("'{t}' is not a number"))?),
        // money stays a string so the backend parses it as an exact decimal
        "d" => {
            t.parse::<f64>().map_err(|_| format!("'{t}' is not an amount"))?;
            json!(t)
        }
        "b" => json!(matches!(t, "true" | "yes" | "on" | "1")),
        "j" => serde_json::from_str::<Value>(t).map_err(|e| format!("invalid JSON: {e}"))?,
        _ => json!(t),
    }))
}

/// Build a request body from `(column, raw text)` pairs, validating required fields.
/// On update (`partial`), blank fields are sent as `null` so they can be cleared.
pub fn build_body(def: &ResDef, values: &[(String, String)], partial: bool) -> Result<Value, String> {
    let mut m = serde_json::Map::new();
    for c in def.editable() {
        let raw = values.iter().find(|(k, _)| k == c.name).map(|(_, v)| v.as_str()).unwrap_or("");
        match coerce(c.ty, raw).map_err(|e| format!("{}: {e}", crate::util::title_case(c.name)))? {
            Some(v) => {
                m.insert(c.name.into(), v);
            }
            None if c.req => return Err(format!("{} is required", crate::util::title_case(c.name))),
            None if partial => {
                m.insert(c.name.into(), Value::Null);
            }
            None => {}
        }
    }
    Ok(Value::Object(m))
}

/// Initial form text for an existing record.
pub fn initial_values(def: &ResDef, rec: Option<&Value>) -> Vec<(String, String)> {
    def.editable()
        .map(|c| {
            let v = rec.and_then(|r| r.get(c.name)).map(|v| match v {
                Value::Null => String::new(),
                Value::String(s) if c.ty == "ts" => s.chars().take(10).collect(),
                Value::String(s) => s.clone(),
                Value::Object(_) | Value::Array(_) => v.to_string(),
                other => other.to_string(),
            });
            (c.name.to_string(), v.unwrap_or_default())
        })
        .collect()
}
