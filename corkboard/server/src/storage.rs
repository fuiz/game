use actix_web::web::Bytes;
use dashmap::DashMap;
use mime::Mime;
use serde::{Deserialize, Serialize};
use serde_hex::{SerHex, Strict};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MediaId(#[serde(with = "SerHex::<Strict>")] u64);

impl MediaId {
    pub fn new() -> Self {
        Self(getrandom::u64().expect("failed to generate random media id"))
    }
}

#[derive(Default)]
pub struct Memory(DashMap<MediaId, (Bytes, Mime)>);

impl Memory {
    pub fn store(&self, media_id: MediaId, bytes: Bytes, content_type: Mime) {
        self.0.insert(media_id, (bytes, content_type));
    }

    pub fn retrieve(&self, media_id: MediaId) -> Option<(Bytes, Mime)> {
        self.0.get(&media_id).map(|x| x.clone())
    }

    pub fn delete(&self, media_id: MediaId) {
        self.0.remove(&media_id);
    }
}
