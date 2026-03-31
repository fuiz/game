use std::path::{Path, PathBuf};

use actix_web::web::Bytes;
use mime::Mime;
use moka::future::Cache;
use serde::{Deserialize, Serialize};
use serde_hex::{SerHex, Strict};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MediaId(#[serde(with = "SerHex::<Strict>")] u64);

impl MediaId {
    pub fn new() -> Self {
        Self(getrandom::u64().expect("failed to generate random media id"))
    }

    fn hex(self) -> String {
        format!("{:016x}", self.0)
    }
}

#[derive(Clone)]
struct CacheEntry {
    bytes: Bytes,
    content_type: Mime,
}

pub struct Memory {
    dir: PathBuf,
    cache: Cache<MediaId, CacheEntry>,
}

impl Memory {
    pub fn new(dir: &Path, cache_max_bytes: u64) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;

        let cache = Cache::builder()
            .max_capacity(cache_max_bytes)
            .weigher(|_key: &MediaId, value: &CacheEntry| -> u32 { value.bytes.len().try_into().unwrap_or(u32::MAX) })
            .build();

        Ok(Self {
            dir: dir.to_owned(),
            cache,
        })
    }

    fn data_path(&self, id: MediaId) -> PathBuf {
        self.dir.join(id.hex())
    }

    fn mime_path(&self, id: MediaId) -> PathBuf {
        self.dir.join(format!("{}.mime", id.hex()))
    }

    pub async fn store(&self, media_id: MediaId, bytes: Bytes, content_type: Mime) -> std::io::Result<()> {
        tokio::fs::write(self.data_path(media_id), &bytes).await?;
        tokio::fs::write(self.mime_path(media_id), content_type.to_string()).await?;

        self.cache.insert(media_id, CacheEntry { bytes, content_type }).await;
        Ok(())
    }

    pub async fn retrieve(&self, media_id: MediaId) -> Option<(Bytes, Mime)> {
        // Check cache first.
        if let Some(entry) = self.cache.get(&media_id).await {
            return Some((entry.bytes, entry.content_type));
        }

        // Fall back to filesystem.
        let data = tokio::fs::read(self.data_path(media_id)).await.ok()?;
        let mime_str = tokio::fs::read_to_string(self.mime_path(media_id)).await.ok()?;
        let content_type: Mime = mime_str.parse().ok()?;
        let bytes = Bytes::from(data);

        // Populate cache for next access.
        self.cache
            .insert(
                media_id,
                CacheEntry {
                    bytes: bytes.clone(),
                    content_type: content_type.clone(),
                },
            )
            .await;

        Some((bytes, content_type))
    }

    pub async fn delete(&self, media_id: MediaId) {
        let _ = tokio::fs::remove_file(self.data_path(media_id)).await;
        let _ = tokio::fs::remove_file(self.mime_path(media_id)).await;
        self.cache.invalidate(&media_id).await;
    }
}
