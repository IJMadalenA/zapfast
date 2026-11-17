//! Spell checking for the composer, using the platform's own checker.

use std::collections::HashSet;
use std::ops::Range;

/// The platform spell checker, behind a small trait so tests can inject words.
trait Engine {
    fn check(&self, text: &str) -> Vec<Range<usize>>;
    fn suggest(&self, word: &str) -> Vec<String>;
}

impl Engine for spellkit::Checker {
    fn check(&self, text: &str) -> Vec<Range<usize>> {
        spellkit::Checker::check(self, text)
            .map(|error| error.range())
            .collect()
    }

    fn suggest(&self, word: &str) -> Vec<String> {
        spellkit::Checker::suggest(self, word)
    }
}

/// The composer's spell checker. It owns the platform checker for the selected
/// language and caches the misspelled ranges for the current text.
#[derive(Default)]
pub struct SpellCheck {
    tag: Option<String>,
    engine: Option<Box<dyn Engine>>,
    ignored: HashSet<String>,
    text: String,
    personal: Vec<String>,
    misspelled: Vec<Range<usize>>,
    error: Option<String>,
}

impl SpellCheck {
    /// Builds a checker for `tag`, such as `en_US` or `es_ES`.
    pub fn new(tag: Option<&str>) -> Self {
        let mut spell = Self::default();
        spell.set_tag(tag);
        spell
    }

    /// Changes the language, dropping the old checker. The new one loads on
    /// first use, so startup never waits for a dictionary.
    pub fn set_tag(&mut self, tag: Option<&str>) {
        if self.tag.as_deref() == tag {
            return;
        }
        self.tag = tag.map(str::to_owned);
        self.engine = None;
        self.error = None;
        self.invalidate();
    }

    /// Loads the platform checker for the current tag, once. A missing
    /// dictionary leaves the feature off and records the error for diagnostics.
    pub fn load(&mut self) {
        if self.engine.is_some() {
            return;
        }
        let Some(tag) = self.tag.clone() else {
            return;
        };
        self.error = None;
        match spellkit::Checker::with_locale(&tag) {
            Ok(checker) => self.engine = Some(Box::new(checker)),
            Err(error) => {
                log::warn!("spell check unavailable for {tag}: {error}");
                self.error = Some(error.to_string());
            }
        }
    }

    /// The misspelled byte ranges in `text`, cached until the text, the
    /// personal dictionary, or the ignored words change.
    pub fn check(&mut self, text: &str, personal: &[String]) -> Vec<Range<usize>> {
        self.load();
        if self.text != text || self.personal != personal {
            self.text.clear();
            self.text.push_str(text);
            self.personal = personal.to_vec();
            self.misspelled.clear();
            if let Some(engine) = &self.engine {
                let personal: HashSet<String> =
                    personal.iter().map(|word| word.to_lowercase()).collect();
                for range in engine.check(text) {
                    let Some((start, end)) = trim_range(text, range) else {
                        continue;
                    };
                    if !should_check(text, start, end) {
                        continue;
                    }
                    let word = text[start..end].to_lowercase();
                    if personal.contains(&word) || self.ignored.contains(&word) {
                        continue;
                    }
                    self.misspelled.push(start..end);
                }
            }
        }
        self.misspelled.clone()
    }

    /// Up to five suggestions, without duplicates or the word itself.
    pub fn suggest(&self, word: &str) -> Vec<String> {
        let Some(engine) = &self.engine else {
            return Vec::new();
        };
        let mut suggestions = Vec::new();
        for suggestion in engine.suggest(word) {
            if suggestion.is_empty() || suggestion == word {
                continue;
            }
            if !suggestions.contains(&suggestion) {
                suggestions.push(suggestion);
            }
            if suggestions.len() == 5 {
                break;
            }
        }
        suggestions
    }

    /// Ignores a word for this session only.
    pub fn ignore(&mut self, word: &str) {
        if self.ignored.insert(word.to_lowercase()) {
            self.invalidate();
        }
    }

    /// Drops the cached ranges after the text or a dictionary changes.
    pub fn invalidate(&mut self) {
        self.text.clear();
        self.personal.clear();
        self.misspelled.clear();
    }

    /// Whether a platform dictionary is loaded.
    pub fn is_active(&self) -> bool {
        self.engine.is_some()
    }

    /// The platform checker's failure, for diagnostics.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

/// Trims punctuation the platform checker may include around a word.
fn trim_range(text: &str, range: Range<usize>) -> Option<(usize, usize)> {
    let mut start = range.start;
    let mut end = range.end.min(text.len());
    if start >= end || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
        return None;
    }
    while start < end {
        let character = text[start..].chars().next()?;
        if character.is_alphabetic() {
            break;
        }
        start += character.len_utf8();
    }
    while end > start {
        let character = text[..end].chars().next_back()?;
        if character.is_alphabetic() {
            break;
        }
        end -= character.len_utf8();
    }
    (start < end).then_some((start, end))
}

/// Skips URLs, mail addresses, mentions, emoji shortcodes, digits, acronyms,
/// single letters, and words outside the Latin script.
fn should_check(text: &str, start: usize, end: usize) -> bool {
    let word = &text[start..end];
    let mut letters = 0;
    let mut all_upper = true;
    for character in word.chars() {
        if character.is_alphabetic() {
            letters += 1;
            if !(character.is_ascii_alphabetic() || ('\u{00C0}'..='\u{024F}').contains(&character))
            {
                return false;
            }
            if character.is_lowercase() {
                all_upper = false;
            }
        } else if character != '\'' && character != '’' && character != '-' {
            return false;
        }
    }
    if letters < 2 || all_upper || word.chars().any(|character| character.is_ascii_digit()) {
        return false;
    }
    let token_start = text[..start]
        .rfind(char::is_whitespace)
        .map_or(0, |at| at + 1);
    let token_end = text[end..]
        .find(char::is_whitespace)
        .map_or(text.len(), |at| end + at);
    let token = &text[token_start..token_end];
    if token.starts_with('@') || token.starts_with('#') {
        return false;
    }
    if token.starts_with(':') && token.ends_with(':') {
        return false;
    }
    let lower = token.to_ascii_lowercase();
    if lower.contains("://") || lower.starts_with("www.") {
        return false;
    }
    if token.contains('@') && token.contains('.') {
        return false;
    }
    if token.contains('/') && token.contains('.') {
        return false;
    }
    let host = token
        .trim_matches(|character: char| {
            !character.is_alphanumeric() && !matches!(character, '.' | '-' | '_')
        })
        .split('/')
        .next()
        .unwrap_or(token);
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() >= 2
        && labels.iter().all(|label| {
            !label.is_empty()
                && label
                    .chars()
                    .all(|character| character.is_alphanumeric() || character == '-')
        })
    {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        ranges: Vec<Range<usize>>,
        suggestions: Vec<String>,
    }

    impl Engine for Fake {
        fn check(&self, _text: &str) -> Vec<Range<usize>> {
            self.ranges.clone()
        }

        fn suggest(&self, _word: &str) -> Vec<String> {
            self.suggestions.clone()
        }
    }

    fn spell(ranges: Vec<Range<usize>>, suggestions: Vec<String>) -> SpellCheck {
        SpellCheck {
            engine: Some(Box::new(Fake {
                ranges,
                suggestions,
            })),
            ..SpellCheck::default()
        }
    }

    #[test]
    fn trim_range_drops_surrounding_punctuation() {
        assert_eq!(trim_range("\"helo\",", 0..7), Some((1, 5)));
        assert_eq!(trim_range("helo", 0..4), Some((0, 4)));
        assert_eq!(trim_range("!!!", 0..3), None);
    }

    #[test]
    fn should_check_skips_links_mentions_emoji_and_acronyms() {
        let text = "helo https://example.com @foo :smile: HELLO 123 café";
        for word in ["helo", "café"] {
            let start = text.find(word).unwrap();
            assert!(should_check(text, start, start + word.len()), "{word}");
        }
        for word in ["https", "example", "foo", "smile", "HELLO", "123"] {
            let start = text.find(word).unwrap();
            assert!(!should_check(text, start, start + word.len()), "{word}");
        }
        let bare = "example.com";
        assert!(!should_check(bare, 0, 7));
        assert!(!should_check(bare, 8, 11));
    }

    #[test]
    fn check_filters_personal_and_ignored_words() {
        let text = "helo wrld";
        let mut spell = spell(vec![0..4, 5..9], Vec::new());
        assert_eq!(spell.check(text, &[]), vec![0..4, 5..9]);
        assert_eq!(spell.check(text, &["helo".to_owned()]), vec![5..9]);
        spell.ignore("wrld");
        assert!(spell.check(text, &["helo".to_owned()]).is_empty());
    }

    #[test]
    fn native_checker_finds_a_misspelling_when_a_dictionary_is_installed() {
        let mut spell = SpellCheck::new(Some("en_US"));
        spell.load();
        if !spell.is_active() {
            return;
        }
        assert_eq!(spell.check("helo world", &[]), vec![0..4]);
    }

    #[test]
    fn suggest_deduplicates_and_limits_to_five() {
        let spell = spell(
            Vec::new(),
            vec![
                "hello".to_owned(),
                "hello".to_owned(),
                "help".to_owned(),
                "held".to_owned(),
                "hell".to_owned(),
                "helm".to_owned(),
                "hero".to_owned(),
            ],
        );
        assert_eq!(
            spell.suggest("helo"),
            vec!["hello", "help", "held", "hell", "helm"]
        );
    }
}
