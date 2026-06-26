use std::collections::HashMap;
use std::sync::OnceLock;

// Locale files are embedded at compile time so the binary is self-contained.
const EN: &str = include_str!("../i18n/en.toml");
const HI: &str = include_str!("../i18n/hi.toml");
const TA: &str = include_str!("../i18n/ta.toml");
const BN: &str = include_str!("../i18n/bn.toml");
const TE: &str = include_str!("../i18n/te.toml");

static MESSAGES: OnceLock<HashMap<String, String>> = OnceLock::new();

/// Initialise the locale from the `LANG` environment variable.
/// Falls back to English when the variable is not set or the locale is unknown.
/// Must be called once at program start before any `t()` calls.
pub fn init() {
    let lang = std::env::var("LANG").unwrap_or_default().to_lowercase();
    let raw = if lang.starts_with("hi") {
        HI
    } else if lang.starts_with("ta") {
        TA
    } else if lang.starts_with("bn") {
        BN
    } else if lang.starts_with("te") {
        TE
    } else {
        EN
    };

    let map = parse_toml_flat(raw);
    let _ = MESSAGES.set(map);
}

/// Look up a message key.  Returns the key itself when the key is not found
/// (so a missing translation is always visible rather than silently empty).
pub fn t(key: &str) -> String {
    MESSAGES
        .get()
        .and_then(|m| m.get(key))
        .cloned()
        .unwrap_or_else(|| key.to_string())
}

/// Simple flat TOML parser: only handles `key = "value"` lines.
/// Sufficient for our locale files which are deliberately kept simple.
fn parse_toml_flat(content: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(eq_pos) = trimmed.find('=') {
            let key = trimmed[..eq_pos].trim().to_string();
            let raw_val = trimmed[eq_pos + 1..].trim();
            // Strip surrounding quotes.
            let value = if raw_val.starts_with('"') && raw_val.ends_with('"') {
                raw_val[1..raw_val.len() - 1].to_string()
            } else {
                raw_val.to_string()
            };
            map.insert(key, value);
        }
    }
    map
}
