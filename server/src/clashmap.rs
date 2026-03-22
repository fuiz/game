use std::{borrow::Borrow, fmt::Debug};

use dashmap::{mapref::entry::Entry, DashMap};

#[derive(Clone)]
#[derive_where::derive_where(Default)]
pub struct ClashMap<K: Eq + std::hash::Hash + Clone, V: Clone>(DashMap<K, V>);

impl<K, V> Debug for ClashMap<K, V>
where
    K: Eq + std::hash::Hash + Clone + Debug,
    V: Clone + Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl<K, V> ClashMap<K, V>
where
    K: Eq + std::hash::Hash + Clone,
    V: Clone,
{
    pub fn get<Q>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.0.get(key).map(|v| v.to_owned())
    }

    // pub fn contains_key(&self, key: &K) -> bool {
    //     self.0.contains_key(key)
    // }

    pub fn insert(&self, key: K, value: V) -> Option<V> {
        self.0.insert(key, value)
    }

    // pub fn len(&self) -> usize {
    //     self.0.len()
    // }

    pub fn remove(&self, key: &K) -> Option<(K, V)> {
        self.0.remove(key)
    }

    pub fn _vec(&self) -> Vec<(K, V)> {
        self.0
            .iter()
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect()
    }

    // pub fn insert_if_vacant(&self, key: K, value: V) -> Option<(K, V)> {
    //     match self.0.entry(key) {
    //         Entry::Occupied(o) => Some((o.into_key(), value)),
    //         Entry::Vacant(v) => {
    //             v.insert(value);
    //             None
    //         }
    //     }
    // }
}

impl<K, V> ClashMap<K, V>
where
    K: Eq + std::hash::Hash + Clone,
    V: Clone + Default,
{
    pub fn _modify_entry_or_default<F>(&self, key: K, f: F)
    where
        F: Fn(&mut V),
    {
        match self.0.entry(key) {
            Entry::Occupied(mut o) => f(o.get_mut()),
            Entry::Vacant(v) => {
                let mut value = V::default();
                f(&mut value);
                v.insert(value);
            }
        }
    }
}
