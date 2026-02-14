use std::sync::LazyLock;

use super::word_list::WordList;

struct NameParts {
    praenomen: WordList,
    nomen: WordList,
    cognomen: WordList,
}

pub struct NameConfig {
    pub praenomen: bool,
}

static NAME_PARTS: LazyLock<NameParts> = LazyLock::new(|| NameParts {
    praenomen: WordList::new(include_str!("../../names/romans/praenomen.txt")),
    nomen: WordList::new(include_str!("../../names/romans/nomen.txt")),
    cognomen: WordList::new(include_str!("../../names/romans/cognomen.txt")),
});

pub fn roman_name(config: &NameConfig) -> Vec<String> {
    let name_parts = &*NAME_PARTS;
    let nomen = name_parts.nomen.random_choice();
    let cognomen = name_parts.cognomen.random_choice();

    if config.praenomen {
        let praenomen = name_parts.praenomen.random_choice();
        vec![
            praenomen.to_string(),
            nomen.to_string(),
            cognomen.to_string(),
        ]
    } else {
        vec![nomen.to_string(), cognomen.to_string()]
    }
}
