//! Shared page-to-raw reference normalization.

/// Strip the raw directory prefix and optional passage fragment.
pub fn source_key(source: &str, raw_prefix: &str) -> Option<String> {
    let prefix = raw_prefix.trim_start_matches("./").trim_end_matches('/');
    let mut text = source.split_once('#').map_or(source, |(head, _)| head);
    while let Some(rest) = text.strip_prefix("./") {
        text = rest;
    }
    if text == prefix {
        return None;
    }
    if !prefix.is_empty() {
        if let Some(rest) = text
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_prefix('/'))
        {
            text = rest;
        }
    }
    if text.is_empty() {
        None
    } else {
        Some(text.into())
    }
}

/// Preserve citation order while dropping duplicate raw keys.
pub fn source_keys(sources: &[String], raw_prefix: &str) -> Vec<String> {
    let mut keys = Vec::new();
    for source in sources {
        if let Some(key) = source_key(source, raw_prefix) {
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
    }
    keys
}
