mod catalog;

use eframe::egui;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Language {
    #[default]
    English,
    TraditionalChinese,
    Japanese,
}

impl Language {
    pub const ALL: [Self; 3] = [Self::English, Self::TraditionalChinese, Self::Japanese];

    pub fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::TraditionalChinese => "zh-TW",
            Self::Japanese => "ja",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::TraditionalChinese => "繁體中文",
            Self::Japanese => "日本語",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|language| language.code().eq_ignore_ascii_case(value.trim()))
    }

    pub fn text(self, source: &str) -> &str {
        if self == Self::English {
            return source;
        }
        catalog::MESSAGES.iter().find(|message| message.0 == source).map_or(source, |message| match self {
            Self::English => message.0,
            Self::TraditionalChinese => message.1,
            Self::Japanese => message.2,
        })
    }

    pub fn format(self, source: &str, values: &[(&str, &str)]) -> String {
        let mut remaining = self.text(source);
        let mut output = String::with_capacity(remaining.len());
        while let Some(start) = remaining.find('{') {
            output.push_str(&remaining[..start]);
            remaining = &remaining[start..];
            let Some(end) = remaining.find('}') else { break };
            let key = &remaining[1..end];
            if let Some((_, value)) = values.iter().find(|(name, _)| *name == key) {
                output.push_str(value);
            } else {
                output.push_str(&remaining[..=end]);
            }
            remaining = &remaining[end + 1..];
        }
        output.push_str(remaining);
        output
    }

    pub fn install(self, ctx: &egui::Context) {
        ctx.data_mut(|data| data.insert_temp(egui::Id::new("interface_language"), self));
    }

    pub fn from_context(ctx: &egui::Context) -> Self {
        ctx.data(|data| data.get_temp(egui::Id::new("interface_language"))).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn placeholders(text: &str) -> BTreeSet<&str> {
        text.split('{').skip(1).filter_map(|part| part.split_once('}').map(|pair| pair.0)).collect()
    }

    #[test]
    fn translations_are_complete_unique_and_keep_placeholders() {
        let mut keys = BTreeSet::new();
        for &(source, chinese, japanese) in catalog::MESSAGES {
            assert!(keys.insert(source), "Duplicate: {source}");
            for translated in [chinese, japanese] {
                assert!(!translated.trim().is_empty(), "Missing: {source}");
                assert_eq!(placeholders(source), placeholders(translated), "{source}");
            }
        }
    }

    #[test]
    fn formats_without_reinterpreting_user_values() {
        let result = Language::TraditionalChinese.format(
            "Delete remote {kind} \"{name}\"? This cannot be undone.",
            &[("kind", "檔案"), ("name", "{kind}資料.txt")],
        );
        assert_eq!(result, "確定刪除遠端檔案「{kind}資料.txt」？此操作無法復原。");
        assert_eq!(Language::Japanese.text("server-specific error"), "server-specific error");
    }

    #[test]
    fn languages_roundtrip_and_contexts_are_independent() {
        for language in Language::ALL {
            assert_eq!(Language::parse(language.code()), Some(language));
        }
        assert_eq!(Language::parse("ZH-tw"), Some(Language::TraditionalChinese));
        assert_eq!(Language::parse("unknown"), None);
        let first = egui::Context::default();
        let second = egui::Context::default();
        Language::Japanese.install(&first);
        assert_eq!(Language::from_context(&first), Language::Japanese);
        assert_eq!(Language::from_context(&second), Language::English);
    }
}
