//! Platform-neutral locale classification shared by the native clients.
//!
//! The clients deliberately support only the languages for which Vibex ships
//! complete product copy.  Any other system language falls back to English.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Locale {
    En,
    ZhCn,
    ZhTw,
}

impl Locale {
    /// Select product copy without introducing a client-global locale dependency.
    pub const fn text(
        self,
        en: &'static str,
        zh_cn: &'static str,
        zh_tw: &'static str,
    ) -> &'static str {
        match self {
            Self::En => en,
            Self::ZhCn => zh_cn,
            Self::ZhTw => zh_tw,
        }
    }

    pub const fn tag(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::ZhCn => "zh-CN",
            Self::ZhTw => "zh-TW",
        }
    }

    /// The three slots a new-session sentence is drawn from, in the order the
    /// reader's language puts them: the words before the Agent's mark, the
    /// connector, and the words after it.
    ///
    /// English keeps the phrase whole and ends on the mark — a sentence as
    /// ordinary as "start something new" has no room for a conjunction on its
    /// left. Chinese reads the connector first, which is what lands the mark
    /// between it and the words it governs. Every native client splits the
    /// sentence here, so the Agent's mark falls in the same place in all of
    /// them rather than each client deciding again what its language means.
    pub const fn new_session_slogan(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::En => ("Start something new", "with", ""),
            Self::ZhCn => ("", "和", "开始新的工作"),
            Self::ZhTw => ("", "和", "開始新的工作"),
        }
    }

    /// The new-session sentence with the Agent's name where its mark stands.
    ///
    /// The mark is the one part of the line a screen reader cannot read, so the
    /// sentence is spoken with the name in its place. A page with no Agent
    /// chosen yet has nothing to put there and says the plain sentence instead.
    pub fn new_session_slogan_spoken(self, agent: Option<&str>) -> String {
        let (lead, connector, trail) = self.new_session_slogan();
        let agent = agent.map(str::trim).filter(|agent| !agent.is_empty());
        match agent {
            Some(agent) => [lead, connector, agent, trail]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" "),
            None => [lead, connector, trail]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" "),
        }
    }

    /// Resolve a BCP-47 or POSIX-style locale identifier.
    ///
    /// Chinese script/region variants are kept distinct; all non-Chinese
    /// languages intentionally use the English product copy.
    pub fn from_system_tag(tag: Option<&str>) -> Self {
        let tag = tag
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .replace('_', "-");
        if tag.starts_with("zh-tw")
            || tag.starts_with("zh-hk")
            || tag.starts_with("zh-mo")
            || tag == "yue"
            || tag.starts_with("yue-")
            || tag.split('-').any(|part| part == "hant")
        {
            Self::ZhTw
        } else if tag == "zh" || tag.starts_with("zh-") || tag == "cmn" || tag.starts_with("cmn-") {
            Self::ZhCn
        } else {
            Self::En
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Locale;

    #[test]
    fn resolves_supported_and_unknown_system_languages() {
        assert_eq!(Locale::from_system_tag(Some("zh_CN.UTF-8")), Locale::ZhCn);
        assert_eq!(Locale::from_system_tag(Some("zh-Hans-CN")), Locale::ZhCn);
        assert_eq!(Locale::from_system_tag(Some("zh-TW")), Locale::ZhTw);
        assert_eq!(Locale::from_system_tag(Some("zh-Hant")), Locale::ZhTw);
        assert_eq!(Locale::from_system_tag(Some("zh-HK")), Locale::ZhTw);
        assert_eq!(Locale::from_system_tag(Some("yue-Hant-HK")), Locale::ZhTw);
        assert_eq!(Locale::from_system_tag(Some("cmn-Hans-CN")), Locale::ZhCn);
        assert_eq!(Locale::from_system_tag(Some("en-US")), Locale::En);
        assert_eq!(Locale::from_system_tag(Some("fr-FR")), Locale::En);
        assert_eq!(Locale::from_system_tag(None), Locale::En);
    }

    /// The sentence is split by the client's language only, so the shared
    /// splitter is what puts the Agent's mark in the same place in every
    /// client. Chinese reads its connector first and English reads its phrase
    /// whole, and the spoken form is the sentence with the mark's Agent named.
    #[test]
    fn the_new_session_sentence_is_split_and_spoken_the_same_way() {
        assert_eq!(
            Locale::En.new_session_slogan(),
            ("Start something new", "with", "")
        );
        assert_eq!(
            Locale::ZhCn.new_session_slogan(),
            ("", "和", "开始新的工作")
        );
        assert_eq!(
            Locale::ZhTw.new_session_slogan(),
            ("", "和", "開始新的工作")
        );

        assert_eq!(
            Locale::En.new_session_slogan_spoken(Some("Codex")),
            "Start something new with Codex"
        );
        assert_eq!(
            Locale::ZhCn.new_session_slogan_spoken(Some("Codex")),
            "和 Codex 开始新的工作"
        );
        // A page with no Agent chosen yet says the sentence alone rather than
        // leaving a hole where the mark would stand.
        assert_eq!(
            Locale::ZhCn.new_session_slogan_spoken(None),
            "和 开始新的工作"
        );
        assert_eq!(
            Locale::En.new_session_slogan_spoken(Some("   ")),
            "Start something new with"
        );
    }
}
