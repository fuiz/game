// Fuiz Game Configuration
// This file contains all configuration constants for the Fuiz game system

/// Main Fuiz configuration
pub mod fuiz {
    pub const MAX_SLIDES_COUNT: usize = 100;
    pub const MAX_TITLE_LENGTH: usize = 200;
    pub const MAX_PLAYER_COUNT: usize = 1000;
}

/// Multiple choice question configuration
pub mod multiple_choice {
    pub const MIN_TITLE_LENGTH: usize = 0;
    pub const MAX_TITLE_LENGTH: usize = 200;
    pub const MIN_INTRODUCE_QUESTION: u32 = 0;
    pub const MAX_INTRODUCE_QUESTION: u32 = 30;
    pub const MIN_TIME_LIMIT: u32 = 5;
    pub const MAX_TIME_LIMIT: u32 = 240;
    pub const MAX_ANSWER_COUNT: u32 = 8;
}

/// Type answer question configuration
pub mod type_answer {
    pub const MIN_TITLE_LENGTH: usize = 0;
    pub const MAX_TITLE_LENGTH: usize = 200;
    pub const MIN_TIME_LIMIT: u32 = 5;
    pub const MAX_TIME_LIMIT: u32 = 240;
    pub const MIN_INTRODUCE_QUESTION: u32 = 0;
    pub const MAX_INTRODUCE_QUESTION: u32 = 30;
    pub const MAX_ANSWER_COUNT: u32 = 16;
}

/// Order question configuration
pub mod order {
    pub const MIN_TITLE_LENGTH: usize = 0;
    pub const MAX_TITLE_LENGTH: usize = 200;
    pub const MIN_TIME_LIMIT: u32 = 5;
    pub const MAX_TIME_LIMIT: u32 = 240;
    pub const MIN_INTRODUCE_QUESTION: u32 = 0;
    pub const MAX_INTRODUCE_QUESTION: u32 = 30;
    pub const MAX_ANSWER_COUNT: u32 = 8;
    pub const MAX_LABEL_LENGTH: usize = 100;
}

/// Corkboard configuration
pub mod corkboard {
    pub const ID_LENGTH: usize = 16;
    pub const MAX_ALT_LENGTH: usize = 200;
}

/// Answer text configuration
pub mod answer_text {
    pub const MAX_LENGTH: usize = 200;
}

/// Bingo configuration
pub mod bingo {
    pub const MAX_ANSWER_COUNT: u32 = 200;
}
