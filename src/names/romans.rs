use std::sync::LazyLock;

use super::word_list::WordList;

/// Word lists for the three components of a Roman name (tria nomina).
struct NameParts {
    praenomen: WordList,
    nomen: WordList,
    cognomen: WordList,
}

/// Configuration for Roman name generation.
pub struct NameConfig {
    /// Whether to include the praenomen (personal/first name).
    /// When `false`, only the nomen and cognomen are used.
    pub praenomen: bool,
}

static NAME_PARTS: LazyLock<NameParts> = LazyLock::new(|| NameParts {
    praenomen: WordList::new(include_str!("../../names/romans/praenomen.txt")),
    nomen: WordList::new(include_str!("../../names/romans/nomen.txt")),
    cognomen: WordList::new(include_str!("../../names/romans/cognomen.txt")),
});

/// Generates a random Roman-style name.
///
/// Returns a vector of name parts: `[praenomen, nomen, cognomen]` if
/// `config.praenomen` is `true`, otherwise `[nomen, cognomen]`.
pub fn roman_name(config: &NameConfig) -> Vec<&'static str> {
    let name_parts = &*NAME_PARTS;
    let nomen = name_parts.nomen.random_choice();
    let cognomen = name_parts.cognomen.random_choice();

    if config.praenomen {
        let praenomen = name_parts.praenomen.random_choice();
        vec![praenomen, nomen, cognomen]
    } else {
        vec![nomen, cognomen]
    }
}
