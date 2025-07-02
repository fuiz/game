// Fuiz Game Configuration
// This file contains all configuration constants for the Fuiz game system

/// Main Fuiz configuration
pub mod fuiz {
    pub const MAX_SLIDES_COUNT: u32 = 100;
    pub const MAX_TITLE_LENGTH: usize = 200;
    pub const MAX_PLAYER_COUNT: u32 = 1000;
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

/// Alternative approach using structs for grouped configuration
/// Uncomment and use this if you prefer a more structured approach

/*
#[derive(Debug, Clone)]
pub struct FuizConfig {
    pub max_slides_count: u32,
    pub max_title_length: usize,
    pub max_player_count: u32,
}

impl Default for FuizConfig {
    fn default() -> Self {
        Self {
            max_slides_count: 100,
            max_title_length: 200,
            max_player_count: 1000,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MultipleChoiceConfig {
    pub min_title_length: usize,
    pub max_title_length: usize,
    pub min_introduce_question: u32,
    pub max_introduce_question: u32,
    pub min_time_limit: u32,
    pub max_time_limit: u32,
    pub max_answer_count: u32,
}

impl Default for MultipleChoiceConfig {
    fn default() -> Self {
        Self {
            min_title_length: 0,
            max_title_length: 200,
            min_introduce_question: 0,
            max_introduce_question: 30,
            min_time_limit: 5,
            max_time_limit: 240,
            max_answer_count: 8,
        }
    }
}

// Add similar structs for other configuration sections as needed...

#[derive(Debug, Clone)]
pub struct GameConfig {
    pub fuiz: FuizConfig,
    pub multiple_choice: MultipleChoiceConfig,
    // Add other config sections...
}

impl Default for GameConfig {
    fn default() -> Self {
        Self {
            fuiz: FuizConfig::default(),
            multiple_choice: MultipleChoiceConfig::default(),
            // Initialize other config sections...
        }
    }
}
*/
