//! UI language: Japanese, English and Simplified Chinese.
//!
//! Each label is written where it is used as `lang.t(ja, en, zh)`, so the three
//! translations sit next to the widget they name and a missing one is a compile
//! error, not a runtime fallback.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Ja,
    En,
    Zh,
}

impl Lang {
    pub const ALL: [Lang; 3] = [Lang::Ja, Lang::En, Lang::Zh];

    /// Each language names itself, so the selector is readable whatever is shown.
    pub fn name(self) -> &'static str {
        match self {
            Lang::Ja => "日本語",
            Lang::En => "English",
            Lang::Zh => "简体中文",
        }
    }

    pub fn t(self, ja: &'static str, en: &'static str, zh: &'static str) -> &'static str {
        match self {
            Lang::Ja => ja,
            Lang::En => en,
            Lang::Zh => zh,
        }
    }

    /// `HYPERDU_LANG` (`ja`, `en`, `zh`) if set, else the OS display language:
    /// Japanese and Chinese map to themselves, everything else to English.
    pub fn detect() -> Self {
        std::env::var("HYPERDU_LANG")
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(os_language)
            .map_or(Lang::En, |tag| Self::from_tag(&tag))
    }

    // ponytail: Traditional Chinese (zh-TW, zh-HK) also gets Simplified, the
    // only Chinese the UI has; add a Lang variant if that is wanted.
    fn from_tag(tag: &str) -> Self {
        let tag = tag.to_ascii_lowercase();
        if tag.starts_with("ja") {
            Lang::Ja
        } else if tag.starts_with("zh") {
            Lang::Zh
        } else {
            Lang::En
        }
    }
}

/// The user's UI language rather than the regional format: someone reading an
/// English Windows with Japanese date formats wants English labels.
#[cfg(windows)]
fn os_language() -> Option<String> {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetUserDefaultUILanguage() -> u16;
    }
    // SAFETY: takes no arguments and only returns a LANGID.
    let primary = unsafe { GetUserDefaultUILanguage() } & 0x3ff;
    Some(
        match primary {
            0x11 => "ja",
            0x04 => "zh",
            _ => "en",
        }
        .to_string(),
    )
}

#[cfg(not(windows))]
fn os_language() -> Option<String> {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|key| std::env::var(key).ok())
        .find(|value| !value.is_empty() && value != "C" && value != "POSIX")
}

#[cfg(test)]
mod tests {
    use super::Lang;

    #[test]
    fn locale_tags_map_to_the_three_languages() {
        assert_eq!(Lang::from_tag("ja_JP.UTF-8"), Lang::Ja);
        assert_eq!(Lang::from_tag("zh_CN.UTF-8"), Lang::Zh);
        assert_eq!(Lang::from_tag("zh-TW"), Lang::Zh);
        assert_eq!(Lang::from_tag("en_US.UTF-8"), Lang::En);
        assert_eq!(Lang::from_tag("de_DE"), Lang::En);
    }
}
