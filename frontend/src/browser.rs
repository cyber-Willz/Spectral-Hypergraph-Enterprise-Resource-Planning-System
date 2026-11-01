//! Small browser helpers: file download, CSV export, reading an uploaded file.
use serde_json::Value;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

/// RFC 4180 CSV from rows (objects) and the chosen columns.
pub fn to_csv(rows: &[Value], cols: &[String]) -> String {
    let esc = |s: String| if s.contains([',', '"', '\n']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s };
    let mut out = cols.iter().map(|c| esc(c.clone())).collect::<Vec<_>>().join(",");
    for r in rows {
        out.push('\n');
        out.push_str(&cols.iter().map(|c| esc(match r.get(c) { Some(Value::String(s)) => s.clone(), Some(Value::Null) | None => String::new(), Some(v) => v.to_string() })).collect::<Vec<_>>().join(","));
    }
    out
}

pub fn download_text(name: &str, mime: &str, text: &str) -> Result<(), String> {
    let parts = js_sys::Array::of1(&text.into());
    let opts = web_sys::BlobPropertyBag::new();
    opts.set_type(mime);
    let blob = web_sys::Blob::new_with_str_sequence_and_options(&parts, &opts).map_err(|_| "could not build the file")?;
    let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(|_| "could not build the file")?;
    let doc = web_sys::window().and_then(|w| w.document()).ok_or("no document")?;
    let a: web_sys::HtmlAnchorElement = doc.create_element("a").map_err(|_| "no anchor")?.dyn_into().map_err(|_| "no anchor")?;
    a.set_href(&url);
    a.set_download(name);
    a.click();
    let _ = web_sys::Url::revoke_object_url(&url);
    Ok(())
}

pub async fn read_file_text(file: &web_sys::File) -> Result<String, String> {
    let v = JsFuture::from(file.text()).await.map_err(|_| "could not read the file".to_string())?;
    v.as_string().ok_or_else(|| "the file is not text".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn csv_escaping() {
        let rows = vec![json!({"a":"x,y","b":"say \"hi\"","c":5}), json!({"a":null})];
        let csv = to_csv(&rows, &["a".into(), "b".into(), "c".into()]);
        assert_eq!(csv, "a,b,c\n\"x,y\",\"say \"\"hi\"\"\",5\n,,");
    }
}
