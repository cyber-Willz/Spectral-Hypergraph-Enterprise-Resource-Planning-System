//! JSON client for the Axum backend. The bearer token lives in localStorage.
use gloo_net::http::Request;
use gloo_storage::{LocalStorage, Storage};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const TOKEN_KEY: &str = "suite.token";
const USER_KEY: &str = "suite.user";

#[derive(Clone, Debug, PartialEq)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
}
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub email: String,
    pub role: String,
    #[serde(default)]
    pub full_name: String,
}
impl User {
    pub fn display_name(&self) -> &str {
        if self.full_name.is_empty() { &self.email } else { &self.full_name }
    }
}

pub fn token() -> Option<String> {
    LocalStorage::get::<String>(TOKEN_KEY).ok()
}
pub fn stored_user() -> Option<User> {
    token()?;
    LocalStorage::get::<User>(USER_KEY).ok()
}
pub fn save_session(token: &str, user: &User) {
    let _ = LocalStorage::set(TOKEN_KEY, token);
    let _ = LocalStorage::set(USER_KEY, user);
}
pub fn clear_session() {
    LocalStorage::delete(TOKEN_KEY);
    LocalStorage::delete(USER_KEY);
}

/// Pull `{ "error": "..." }` out of a failed response (falls back to the status line).
pub fn error_message(status: u16, body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(String::from))
        .unwrap_or_else(|| match status {
            401 => "Your session has expired. Please sign in again.".into(),
            403 => "Your role is not allowed to do that.".into(),
            404 => "Not found.".into(),
            _ => format!("The request failed ({status})."),
        })
}

async fn send_raw(method: &str, path: &str, body: Option<(String, &str)>) -> Result<Value, ApiError> {
    let mut rb = match method {
        "POST" => Request::post(path),
        "PUT" => Request::put(path),
        "PATCH" => Request::patch(path),
        "DELETE" => Request::delete(path),
        _ => Request::get(path),
    };
    if let Some(t) = token() {
        rb = rb.header("Authorization", &format!("Bearer {t}"));
    }
    let resp = match body {
        Some((text, ctype)) => rb.header("Content-Type", ctype).body(text).map_err(|e| ApiError { status: 0, message: e.to_string() })?.send().await,
        None => rb.send().await,
    }
    .map_err(|e| ApiError { status: 0, message: format!("Cannot reach the server: {e}") })?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if status == 401 && path != "/api/auth/login" {
        clear_session();
        if let Some(w) = web_sys::window() {
            let _ = w.location().reload();
        }
    }
    if !(200..300).contains(&status) {
        return Err(ApiError { status, message: error_message(status, &text) });
    }
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&text).map_err(|e| ApiError { status, message: format!("Unexpected response: {e}") })
}

pub async fn send(method: &str, path: &str, body: Option<&Value>) -> Result<Value, ApiError> {
    send_raw(method, path, body.map(|b| (b.to_string(), "application/json"))).await
}
pub async fn get(path: &str) -> Result<Value, ApiError> {
    send("GET", path, None).await
}
pub async fn post(path: &str, body: &Value) -> Result<Value, ApiError> {
    send("POST", path, Some(body)).await
}
#[allow(dead_code)]
pub async fn patch(path: &str, body: &Value) -> Result<Value, ApiError> {
    send("PATCH", path, Some(body)).await
}
#[allow(dead_code)]
pub async fn delete(path: &str) -> Result<Value, ApiError> {
    send("DELETE", path, None).await
}
/// POST a raw CSV file (the import endpoints take the file as the request body).
pub async fn post_csv(path: &str, text: String) -> Result<Value, ApiError> {
    send_raw("POST", path, Some((text, "text/csv"))).await
}

pub async fn login(email: &str, password: &str) -> Result<(String, User), ApiError> {
    let v = post("/api/auth/login", &json!({ "email": email, "password": password })).await?;
    let token = v.get("token").and_then(|t| t.as_str()).ok_or(ApiError { status: 0, message: "The server did not return a token.".into() })?.to_string();
    let user: User = serde_json::from_value(v.get("user").cloned().unwrap_or(Value::Null)).map_err(|e| ApiError { status: 0, message: format!("Unexpected login response: {e}") })?;
    Ok((token, user))
}

/// Ask the server who the stored token belongs to (also detects expiry on page load).
pub async fn whoami() -> Result<User, ApiError> {
    let v = get("/api/auth/me").await?;
    let mut u: User = serde_json::from_value(v).map_err(|e| ApiError { status: 0, message: e.to_string() })?;
    if let Some(old) = LocalStorage::get::<User>(USER_KEY).ok() {
        if u.full_name.is_empty() && old.id == u.id {
            u.full_name = old.full_name;
        }
    }
    Ok(u)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn error_bodies() {
        assert_eq!(error_message(422, r#"{"error":"stage required"}"#), "stage required");
        assert_eq!(error_message(403, "oops"), "Your role is not allowed to do that.");
        assert_eq!(error_message(500, ""), "The request failed (500).");
    }
}
