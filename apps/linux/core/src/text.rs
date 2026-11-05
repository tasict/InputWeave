//! The app's words: a key into the string catalog and what goes into its
//! placeholders, so the decisions about which text to show stay testable
//! without the UI. English is the source language; Traditional Chinese
//! (Taiwan) uses Taiwan's terms.

use std::collections::HashMap;
use std::fmt;
use std::sync::OnceLock;

pub mod keys;

const EN: &str = include_str!("../strings/en.txt");
const ZH_TW: &str = include_str!("../strings/zh-TW.txt");

/// A string the UI shows, as a catalog key and the values for its
/// placeholders (`{0}`, `{1}`…). A value is plain text or another
/// `UiText`, looked up first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiText {
    pub key: &'static str,
    pub args: Vec<Arg>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Arg {
    Plain(String),
    Text(UiText),
}

impl From<String> for Arg {
    fn from(value: String) -> Self {
        Arg::Plain(value)
    }
}

impl From<&str> for Arg {
    fn from(value: &str) -> Self {
        Arg::Plain(value.to_owned())
    }
}

impl From<UiText> for Arg {
    fn from(value: UiText) -> Self {
        Arg::Text(value)
    }
}

impl UiText {
    pub fn new(key: &'static str) -> Self {
        Self {
            key,
            args: Vec::new(),
        }
    }

    pub fn with(key: &'static str, args: impl IntoIterator<Item = Arg>) -> Self {
        Self {
            key,
            args: args.into_iter().collect(),
        }
    }

    /// Text shown as it is, such as a device's name.
    pub fn verbatim(text: impl Into<String>) -> Self {
        Self::with(keys::VERBATIM, [Arg::Plain(text.into())])
    }
}

/// Builds a [`UiText`] from a key and its arguments.
#[macro_export]
macro_rules! ui {
    ($key:expr) => {
        $crate::text::UiText::new($key)
    };
    ($key:expr, $($arg:expr),+ $(,)?) => {
        $crate::text::UiText::with($key, [$($crate::text::Arg::from($arg)),+])
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    English,
    ChineseTaiwan,
}

impl Language {
    /// The language of the user's locale, as gettext reads it: `LANGUAGE`
    /// (a list), then `LC_ALL`, `LC_MESSAGES` and `LANG`. Chinese other than
    /// Simplified gets Traditional Chinese; everything else English.
    pub fn from_env() -> Self {
        Self::from_vars(|name| std::env::var(name).ok())
    }

    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Self {
        let set = |name: &str| var(name).filter(|v| !v.is_empty());
        // LANGUAGE counts only with a locale set (gettext ignores it under C).
        let locale = ["LC_ALL", "LC_MESSAGES", "LANG"].into_iter().find_map(set);
        if locale.as_deref().is_some_and(|l| l != "C" && l != "POSIX")
            && let Some(list) = set("LANGUAGE")
        {
            for tag in list.split(':') {
                if let Some(language) = Self::from_tag(tag) {
                    return language;
                }
            }
        }
        locale
            .as_deref()
            .and_then(Self::from_tag)
            .unwrap_or(Language::English)
    }

    /// `zh_TW.UTF-8`, `zh-Hant`, `en_US` and the like; `None` for one the
    /// app has no strings in.
    pub fn from_tag(tag: &str) -> Option<Self> {
        let tag = tag.split(['.', '@']).next().unwrap_or("").replace('-', "_");
        let mut parts = tag.split('_');
        match parts.next()? {
            "en" => Some(Language::English),
            "zh" => {
                let rest: Vec<&str> = parts.collect();
                let simplified = rest
                    .iter()
                    .any(|p| matches!(*p, "CN" | "SG" | "Hans" | "hans"));
                (!simplified).then_some(Language::ChineseTaiwan)
            }
            _ => None,
        }
    }

    /// For `--language`: "en", "zh-TW" and the forms `from_tag` takes.
    pub fn parse(name: &str) -> Option<Self> {
        Self::from_tag(name)
    }
}

/// The strings of one language.
pub struct Catalog {
    strings: HashMap<&'static str, String>,
}

impl Catalog {
    pub fn for_language(language: Language) -> &'static Catalog {
        static EN_CATALOG: OnceLock<Catalog> = OnceLock::new();
        static ZH_CATALOG: OnceLock<Catalog> = OnceLock::new();
        match language {
            Language::English => EN_CATALOG.get_or_init(|| Catalog::parse(EN)),
            Language::ChineseTaiwan => ZH_CATALOG.get_or_init(|| Catalog::parse(ZH_TW)),
        }
    }

    fn parse(source: &'static str) -> Catalog {
        let mut strings = HashMap::new();
        for line in source.lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let Some((key, value)) = line.split_once(" = ") else {
                continue;
            };
            strings.insert(key.trim(), unescape(value));
        }
        Catalog { strings }
    }

    /// The string for `key` as it is, placeholders and all; the key itself
    /// when the catalog has none (the tests make sure it always has).
    pub fn get<'a>(&'a self, key: &'a str) -> &'a str {
        self.strings.get(key).map_or(key, String::as_str)
    }

    pub fn render(&self, text: &UiText) -> String {
        let args: Vec<String> = text
            .args
            .iter()
            .map(|arg| match arg {
                Arg::Plain(s) => s.clone(),
                Arg::Text(t) => self.render(t),
            })
            .collect();
        fill(self.get(text.key), &args)
    }

    #[cfg(test)]
    fn keys(&self) -> impl Iterator<Item = &&'static str> {
        self.strings.keys()
    }
}

fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('s') => out.push(' '),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// Replaces `{0}`, `{1}`… in `template` with `args`.
fn fill(template: &str, args: &[String]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) if after[..close].chars().all(|c| c.is_ascii_digit()) && close > 0 => {
                let index: usize = after[..close].parse().unwrap_or(usize::MAX);
                match args.get(index) {
                    Some(arg) => out.push_str(arg),
                    None => out.push_str(&rest[open..open + close + 2]),
                }
                rest = &after[close + 1..];
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

impl fmt::Display for UiText {
    /// The English text, for logs and tests.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&Catalog::for_language(Language::English).render(self))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn placeholders(s: &str) -> BTreeSet<String> {
        let mut found = BTreeSet::new();
        let mut rest = s;
        while let Some(open) = rest.find('{') {
            let after = &rest[open + 1..];
            if let Some(close) = after.find('}') {
                found.insert(after[..close].to_owned());
                rest = &after[close + 1..];
            } else {
                break;
            }
        }
        found
    }

    #[test]
    fn both_languages_have_the_same_keys_and_placeholders() {
        let en = Catalog::for_language(Language::English);
        let zh = Catalog::for_language(Language::ChineseTaiwan);
        let en_keys: BTreeSet<_> = en.keys().collect();
        let zh_keys: BTreeSet<_> = zh.keys().collect();
        assert_eq!(en_keys, zh_keys);
        for key in en_keys {
            assert_eq!(
                placeholders(en.get(key)),
                placeholders(zh.get(key)),
                "{key}"
            );
        }
    }

    #[test]
    fn every_key_the_code_uses_is_in_the_catalogs() {
        for language in [Language::English, Language::ChineseTaiwan] {
            let catalog = Catalog::for_language(language);
            for key in keys::ALL {
                assert!(catalog.strings.contains_key(key), "{key} in {language:?}");
            }
        }
    }

    #[test]
    fn no_key_is_listed_twice() {
        for source in [EN, ZH_TW] {
            let mut seen = BTreeSet::new();
            for line in source.lines().filter(|l| !l.starts_with('#')) {
                if let Some((key, _)) = line.split_once(" = ") {
                    assert!(seen.insert(key), "{key} twice");
                }
            }
        }
    }

    #[test]
    fn placeholders_are_filled_and_nested_texts_looked_up() {
        let en = Catalog::for_language(Language::English);
        let text = ui!(keys::NOTIFY_FAILED_ONE, ui!(keys::ITEMS_FILE_MANY, "3"));
        assert_eq!(en.render(&text), "3 files; 1 file could not be sent");
        assert_eq!(
            fill("{0} and {1} {2}", &["a".into(), "b".into()]),
            "a and b {2}"
        );
        assert_eq!(fill("{x} {", &[]), "{x} {");
        let zh = Catalog::for_language(Language::ChineseTaiwan);
        assert_eq!(
            zh.render(&ui!(keys::TITLE_CONTROLLING, "Mac mini")),
            "正在控制 Mac mini"
        );
    }

    #[test]
    fn escapes_are_read() {
        let en = Catalog::for_language(Language::English);
        assert_eq!(en.get(keys::NOTIFY_LIST_SEPARATOR), ", ");
        assert_eq!(unescape(r"a\nb\\c\s"), "a\nb\\c ");
    }

    #[test]
    fn the_language_comes_from_the_locale() {
        let vars = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        assert_eq!(
            Language::from_vars(vars(&[("LANG", "zh_TW.UTF-8")])),
            Language::ChineseTaiwan
        );
        assert_eq!(
            Language::from_vars(vars(&[("LANG", "zh_CN.UTF-8")])),
            Language::English
        );
        assert_eq!(
            Language::from_vars(vars(&[("LANG", "en_US.UTF-8"), ("LC_ALL", "zh_HK.UTF-8")])),
            Language::ChineseTaiwan
        );
        assert_eq!(
            Language::from_vars(vars(&[
                ("LANG", "de_DE.UTF-8"),
                ("LANGUAGE", "fr:zh_TW:en")
            ])),
            Language::ChineseTaiwan
        );
        // LANGUAGE does not count under the C locale.
        assert_eq!(
            Language::from_vars(vars(&[("LANG", "C"), ("LANGUAGE", "zh_TW")])),
            Language::English
        );
        assert_eq!(Language::from_vars(vars(&[])), Language::English);
        assert_eq!(Language::parse("zh-TW"), Some(Language::ChineseTaiwan));
        assert_eq!(Language::parse("en"), Some(Language::English));
    }
}
