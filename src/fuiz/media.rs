use garde::Validate;
use serde::{Deserialize, Serialize};

/// Represents any kinda of media, currently only images
#[derive(Debug, Serialize, Deserialize, Clone, Validate)]
pub enum Media {
    Image(#[garde(dive)] Image),
}

#[derive(Debug, Serialize, Deserialize, Clone, Validate)]
pub enum Image {
    Corkboard {
        #[garde(length(equal = crate::constants::corkboard::ID_LENGTH))]
        id: String,
        #[garde(length(max = crate::constants::corkboard::MAX_ALT_LENGTH))]
        alt: String,
    },
}
