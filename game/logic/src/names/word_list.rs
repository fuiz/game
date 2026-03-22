/// A list of words loaded from a static string, supporting random selection.
pub struct WordList {
    words: Vec<&'static str>,
}

impl WordList {
    /// Creates a new [`WordList`] by splitting `data` into lines.
    pub fn new(data: &'static str) -> Self {
        Self {
            words: data.lines().collect(),
        }
    }

    /// Returns a randomly chosen word from the list.
    ///
    /// # Panics
    ///
    /// Panics if the word list is empty.
    pub fn random_choice(&self) -> &'static str {
        fastrand::choice(&self.words).expect("Word list was empty")
    }
}
