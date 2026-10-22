//! Tags and categories: the two ways files are classified.
//!
//! Tags are free-form and made by uploaders; categories are a short list an
//! administrator curates. Both count the live files filed under them.

use crate::ids::{CategoryId, TagId};

/// The longest tag slug, matching the column.
pub const MAX_TAG_LEN: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub id: TagId,
    /// As first typed.
    pub name: String,
    /// The identity of the tag: see [`tag_slug`].
    pub slug: String,
    pub file_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Category {
    pub id: CategoryId,
    pub name: String,
    pub url_key: String,
    pub description: Option<String>,
    pub file_count: u32,
    pub sort_order: i32,
}

/// The canonical form of a tag, which decides whether two spellings are the
/// same tag: lowercased, runs of whitespace and separators turned into one
/// `-`, anything else that is not a letter, digit, `-` or `_` dropped, and
/// cut to [`MAX_TAG_LEN`] characters. `None` if nothing is left.
///
/// Letters in any script are kept. Transliterating to ASCII would collapse
/// distinct non-Latin tags into the same slug, or into nothing.
pub fn tag_slug(name: &str) -> Option<String> {
    let mut slug = String::new();
    let mut pending_dash = false;
    for ch in name.chars().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() || ch == '_' {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.push(ch);
        } else if ch.is_whitespace() || ch == '-' || ch == '.' || ch == '/' {
            pending_dash = true;
        }
    }
    let slug: String = slug.chars().take(MAX_TAG_LEN).collect();
    let slug = slug.trim_end_matches('-').to_string();
    (!slug.is_empty()).then_some(slug)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_fold_case_and_separators() {
        assert_eq!(tag_slug("Cats").as_deref(), Some("cats"));
        assert_eq!(
            tag_slug("  Black   and white ").as_deref(),
            Some("black-and-white")
        );
        assert_eq!(tag_slug("sci-fi").as_deref(), Some("sci-fi"));
        assert_eq!(tag_slug("sci - - fi").as_deref(), Some("sci-fi"));
        assert_eq!(tag_slug("v1.2/beta").as_deref(), Some("v1-2-beta"));
        assert_eq!(tag_slug("snake_case").as_deref(), Some("snake_case"));
    }

    #[test]
    fn markup_and_punctuation_are_dropped() {
        assert_eq!(
            tag_slug("<script>alert(1)</script>").as_deref(),
            Some("scriptalert1-script")
        );
        assert_eq!(tag_slug("#nofilter!").as_deref(), Some("nofilter"));
    }

    #[test]
    fn other_scripts_survive() {
        assert_eq!(tag_slug("Ünïcödé").as_deref(), Some("ünïcödé"));
        assert_eq!(tag_slug("猫 写真").as_deref(), Some("猫-写真"));
        assert_eq!(tag_slug("Кошки").as_deref(), Some("кошки"));
    }

    #[test]
    fn empty_results_are_none() {
        for input in ["", "   ", "!!!", "- - -", "#"] {
            assert_eq!(tag_slug(input), None, "{input:?}");
        }
    }

    #[test]
    fn slugs_are_capped_without_a_trailing_dash() {
        let long = "a".repeat(99) + " b";
        let slug = tag_slug(&long).unwrap();
        assert_eq!(slug.chars().count(), 99, "{slug}");
        assert!(!slug.ends_with('-'));
        assert_eq!(
            tag_slug(&"é".repeat(300)).unwrap().chars().count(),
            MAX_TAG_LEN
        );
    }
}
