use std::sync::LazyLock;

use super::word_list::WordList;

struct NameParts {
    nouns: WordList,
    adjectives: WordList,
    adverbs: WordList,
}

pub struct NameConfig {
    pub parts: u8,
}

static NAME_PARTS: LazyLock<NameParts> = LazyLock::new(|| NameParts {
    nouns: WordList::new(include_str!("../../names/pets/nouns.txt")),
    adjectives: WordList::new(include_str!("../../names/pets/adjectives.txt")),
    adverbs: WordList::new(include_str!("../../names/pets/adverbs.txt")),
});

pub fn pet_name(config: NameConfig) -> Vec<String> {
    let name_parts = &*NAME_PARTS;

    (0..config.parts)
        .rev()
        .map(|i| {
            match i {
                0 => name_parts.nouns.random_choice(),
                1 => name_parts.adjectives.random_choice(),
                _ => name_parts.adverbs.random_choice(),
            }
            .to_string()
        })
        .collect()
}
