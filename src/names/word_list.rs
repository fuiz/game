pub struct WordList {
    words: Vec<&'static str>,
}

impl WordList {
    pub fn new(data: &'static str) -> Self {
        Self {
            words: data.lines().collect(),
        }
    }

    pub fn random_choice(&self) -> &str {
        fastrand::choice(&self.words).expect("Word list was empty")
    }
}
