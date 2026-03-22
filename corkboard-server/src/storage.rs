use actix_web::web::Bytes;
use dashmap::DashMap;
use mime::Mime;

pub trait Storage<T>: Default {
    fn store(&self, media_id: T, bytes: Bytes, content_type: Mime);

    fn retrieve(&self, object_id: &T) -> Option<(Bytes, Mime)>;

    fn delete(&self, object_id: &T);
}

pub struct Memory<T: std::hash::Hash + Eq>(DashMap<T, (Bytes, Mime)>);

impl<T: std::hash::Hash + Eq> Default for Memory<T> {
    fn default() -> Self {
        Self(DashMap::new())
    }
}

impl<T> Storage<T> for Memory<T>
where
    T: std::hash::Hash + Eq,
{
    fn retrieve(&self, object_id: &T) -> Option<(Bytes, Mime)> {
        self.0.get(object_id).map(|x| x.clone())
    }

    fn store(&self, media_id: T, bytes: Bytes, content_type: Mime) {
        self.0.insert(media_id, (bytes, content_type));
    }

    fn delete(&self, object_id: &T) {
        self.0.remove(object_id);
    }
}
