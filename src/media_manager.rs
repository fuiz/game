use actix_web::web::Bytes;
use mime::Mime;
use serde::{Deserialize, Serialize};
use serde_hex::{SerHex, Strict};

use crate::storage::Storage;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MediaId(#[serde(with = "SerHex::<Strict>")] u64);

impl MediaId {
    fn new() -> Self {
        Self(getrandom::u64().expect("failed to generate random media id"))
    }
}

#[derive(Default)]
pub struct MediaManager<T: Storage<MediaId> + Default> {
    storage: T,
}

impl<T: Storage<MediaId>> MediaManager<T> {
    pub fn store(&self, bytes: Bytes, content_type: Mime) -> MediaId {
        let media_id = MediaId::new();
        self.storage.store(media_id, bytes, content_type);
        media_id
    }

    pub fn retrieve(&self, media_id: MediaId) -> Option<(Bytes, Mime)> {
        self.storage.retrieve(&media_id)
    }

    pub fn delete(&self, media_id: MediaId) {
        self.storage.delete(&media_id);
    }
}
