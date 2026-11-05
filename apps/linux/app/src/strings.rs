//! The strings in the user's language (`inputweave_app_core::text`).

use std::sync::OnceLock;

use inputweave_app_core::text::{Catalog, Language, UiText};

static LANGUAGE: OnceLock<Language> = OnceLock::new();

/// Chooses the language once, before anything is shown: `--language`, or
/// the locale's.
pub fn choose(language: Option<Language>) {
    let _ = LANGUAGE.set(language.unwrap_or_else(Language::from_env));
}

pub fn language() -> Language {
    *LANGUAGE.get_or_init(Language::from_env)
}

fn catalog() -> &'static Catalog {
    Catalog::for_language(language())
}

/// The string for a key, with no placeholders filled in.
pub fn t(key: &str) -> String {
    catalog().get(key).to_owned()
}

pub fn tr(text: &UiText) -> String {
    catalog().render(text)
}
