//! Configuration constants for the Fuiz game system
//!
//! This module contains all the configuration limits and constraints
//! used throughout the game system to ensure data integrity and
//! provide consistent boundaries for different game components.

/// Main Fuiz configuration constants
pub mod fuiz {
    /// Maximum number of slides allowed in a single Fuiz game
    pub const MAX_SLIDES_COUNT: usize = 100;
    /// Maximum length of a Fuiz title in characters
    pub const MAX_TITLE_LENGTH: usize = 200;
    /// Maximum number of players allowed in a single game session
    pub const MAX_PLAYER_COUNT: usize = 1000;
}

/// Multiple choice question configuration constants
pub mod multiple_choice {
    /// Minimum length of a multiple choice question title
    pub const MIN_TITLE_LENGTH: usize = 0;
    /// Maximum length of a multiple choice question title
    pub const MAX_TITLE_LENGTH: usize = 200;
    /// Minimum time in seconds to introduce/display a question before answers appear
    pub const MIN_INTRODUCE_QUESTION: u64 = 0;
    /// Maximum time in seconds to introduce/display a question before answers appear
    pub const MAX_INTRODUCE_QUESTION: u64 = 30;
    /// Minimum time limit in seconds for answering a multiple choice question
    pub const MIN_TIME_LIMIT: u64 = 5;
    /// Maximum time limit in seconds for answering a multiple choice question
    pub const MAX_TIME_LIMIT: u64 = 240;
    /// Maximum number of answer options for a multiple choice question
    pub const MAX_ANSWER_COUNT: usize = 8;
}

/// Type answer question configuration constants
pub mod type_answer {
    /// Minimum length of a type answer question title
    pub const MIN_TITLE_LENGTH: usize = 0;
    /// Maximum length of a type answer question title
    pub const MAX_TITLE_LENGTH: usize = 200;
    /// Minimum time limit in seconds for answering a type answer question
    pub const MIN_TIME_LIMIT: u64 = 5;
    /// Maximum time limit in seconds for answering a type answer question
    pub const MAX_TIME_LIMIT: u64 = 240;
    /// Minimum time in seconds to introduce/display a question before input appears
    pub const MIN_INTRODUCE_QUESTION: u64 = 0;
    /// Maximum time in seconds to introduce/display a question before input appears
    pub const MAX_INTRODUCE_QUESTION: u64 = 30;
    /// Maximum number of acceptable answers for a type answer question
    pub const MAX_ANSWER_COUNT: usize = 16;
}

/// Order question configuration constants
pub mod order {
    /// Minimum length of an order question title
    pub const MIN_TITLE_LENGTH: usize = 0;
    /// Maximum length of an order question title
    pub const MAX_TITLE_LENGTH: usize = 200;
    /// Minimum time limit in seconds for answering an order question
    pub const MIN_TIME_LIMIT: u64 = 5;
    /// Maximum time limit in seconds for answering an order question
    pub const MAX_TIME_LIMIT: u64 = 240;
    /// Minimum time in seconds to introduce/display a question before ordering begins
    pub const MIN_INTRODUCE_QUESTION: u64 = 0;
    /// Maximum time in seconds to introduce/display a question before ordering begins
    pub const MAX_INTRODUCE_QUESTION: u64 = 30;
    /// Maximum number of items that can be ordered in an order question
    pub const MAX_ANSWER_COUNT: usize = 8;
    /// Maximum length of a label for an item to be ordered
    pub const MAX_LABEL_LENGTH: usize = 100;
}

/// Corkboard configuration constants for media attachments
pub mod corkboard {
    /// Length of generated IDs for corkboard items
    pub const ID_LENGTH: usize = 16;
    /// Maximum length of alt text for accessibility
    pub const MAX_ALT_LENGTH: usize = 200;
}

/// Answer text configuration constants
pub mod answer_text {
    /// Maximum length of answer text in characters
    pub const MAX_LENGTH: usize = 200;
}
