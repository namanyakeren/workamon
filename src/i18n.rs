use std::sync::OnceLock;

use axum::http::{header, HeaderMap};
use serde_json::Value;

/// Nama cookie yang menyimpan pilihan bahasa.
pub const COOKIE: &str = "workamon_lang";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Id,
    En,
}

impl Lang {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "id" | "in" | "id-id" => Some(Self::Id),
            "en" | "en-us" | "en-gb" => Some(Self::En),
            _ => None,
        }
    }

    pub fn from_headers(headers: &HeaderMap) -> Self {
        if let Some(raw) = headers
            .get(header::COOKIE)
            .and_then(|value| value.to_str().ok())
        {
            for part in raw.split(';') {
                let part = part.trim();
                if let Some((key, value)) = part.split_once('=') {
                    if key.trim() == COOKIE {
                        if let Some(lang) = Self::parse(value) {
                            return lang;
                        }
                    }
                }
            }
        }
        Self::Id
    }

    pub fn code(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::En => "en",
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            Self::Id => "ID",
            Self::En => "EN",
        }
    }

    pub fn other(self) -> Self {
        match self {
            Self::Id => Self::En,
            Self::En => Self::Id,
        }
    }
}

fn translations() -> &'static Value {
    static TRANSLATIONS: OnceLock<Value> = OnceLock::new();
    TRANSLATIONS.get_or_init(|| {
        serde_json::from_str(include_str!("../assets/i18n.json"))
            .expect("assets/i18n.json harus berisi JSON yang valid")
    })
}

/// Kamus datar untuk template MiniJinja. Template memakai `{{ t.key }}`.
pub fn dict(lang: Lang) -> Value {
    translations()
        .get(lang.code())
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()))
}

pub fn t(lang: Lang, key: &str) -> String {
    translations()
        .get(lang.code())
        .and_then(|dictionary| dictionary.get(key))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| key.to_string())
}

/// Terjemahan dengan placeholder `{nama}`.
pub fn tf(lang: Lang, key: &str, args: &[(&str, String)]) -> String {
    let mut text = t(lang, key);
    for (name, value) in args {
        text = text.replace(&format!("{{{name}}}"), value);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictionary_has_both_languages() {
        for key in ["nav_dashboard", "dashboard_title", "images_title"] {
            assert!(!t(Lang::Id, key).is_empty());
            assert!(!t(Lang::En, key).is_empty());
        }
    }

    #[test]
    fn placeholders_are_replaced() {
        let text = tf(Lang::En, "msg_container_create_ok", &[("name", "demo".into())]);
        assert!(text.contains("demo"));
        assert!(!text.contains("{name}"));
    }
}
