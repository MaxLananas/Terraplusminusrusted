//! A count-bounded least-recently-used cache with in-flight deduplication: concurrent requests
//! for one key share a single load, and failures are handed to every waiter but never cached.

use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;
use std::sync::{Arc, Condvar, Mutex, PoisonError};

enum Outcome<V, E> {
    Pending,
    Done(Result<V, E>),
    /// The leader's load panicked: followers start over (one of them becomes the new leader).
    Abandoned,
}

struct Flight<V, E> {
    result: Mutex<Outcome<V, E>>,
    ready: Condvar,
}

impl<V, E> Flight<V, E> {
    fn finish(&self, outcome: Outcome<V, E>) {
        *self.result.lock().unwrap_or_else(PoisonError::into_inner) = outcome;
        self.ready.notify_all();
    }
}

/// Unregisters the leader's flight if its load unwinds, so the key is not left "loading" forever
/// with its followers blocked.
struct LeaderGuard<'a, K: Hash + Eq, V, E> {
    state: &'a Mutex<State<K, V, E>>,
    key: Option<K>,
    flight: Arc<Flight<V, E>>,
}

impl<K: Hash + Eq, V, E> Drop for LeaderGuard<'_, K, V, E> {
    fn drop(&mut self) {
        if let Some(key) = self.key.take() {
            self.state.lock().unwrap_or_else(PoisonError::into_inner).in_flight.remove(&key);
            self.flight.finish(Outcome::Abandoned);
        }
    }
}

struct State<K, V, E> {
    entries: HashMap<K, (V, u64)>,
    recency: BTreeMap<u64, K>,
    in_flight: HashMap<K, Arc<Flight<V, E>>>,
    clock: u64,
    hits: u64,
    misses: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CacheStatistics {
    pub entries: usize,
    pub hits: u64,
    pub misses: u64,
}

pub struct LoadingCache<K, V, E> {
    capacity: usize,
    state: Mutex<State<K, V, E>>,
}

enum Role<V, E> {
    Leader(Arc<Flight<V, E>>),
    Follower(Arc<Flight<V, E>>),
}

impl<K: Hash + Eq + Clone, V: Clone, E: Clone> LoadingCache<K, V, E> {
    pub fn new(capacity: usize) -> Self {
        let state = State { entries: HashMap::new(), recency: BTreeMap::new(), in_flight: HashMap::new(), clock: 0, hits: 0, misses: 0 };
        Self { capacity: capacity.max(1), state: Mutex::new(state) }
    }

    pub fn statistics(&self) -> CacheStatistics {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        CacheStatistics { entries: state.entries.len(), hits: state.hits, misses: state.misses }
    }

    pub fn get(&self, key: &K) -> Option<V> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        Self::touch(&mut state, key)
    }

    fn touch(state: &mut State<K, V, E>, key: &K) -> Option<V> {
        state.clock += 1;
        let tick = state.clock;
        let (value, old_tick) = state.entries.get_mut(key)?;
        let value = value.clone();
        let previous = std::mem::replace(old_tick, tick);
        state.recency.remove(&previous);
        state.recency.insert(tick, key.clone());
        state.hits += 1;
        Some(value)
    }

    pub fn insert(&self, key: K, value: V) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        self.insert_locked(&mut state, key, value);
    }

    fn insert_locked(&self, state: &mut State<K, V, E>, key: K, value: V) {
        state.clock += 1;
        let tick = state.clock;
        if let Some((_, old_tick)) = state.entries.insert(key.clone(), (value, tick)) {
            state.recency.remove(&old_tick);
        }
        state.recency.insert(tick, key);
        while state.entries.len() > self.capacity {
            let Some((_, oldest)) = state.recency.pop_first() else { break };
            state.entries.remove(&oldest);
        }
    }

    /// Returns the cached value, joins a load already running for `key`, or runs `load`. If the
    /// running load panics, its followers start over instead of waiting forever.
    pub fn get_or_load(&self, key: K, load: impl FnOnce() -> Result<V, E>) -> Result<V, E> {
        loop {
            let role = {
                let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(value) = Self::touch(&mut state, &key) {
                    return Ok(value);
                }
                state.misses += 1;
                match state.in_flight.get(&key) {
                    Some(flight) => Role::Follower(Arc::clone(flight)),
                    None => {
                        let flight = Arc::new(Flight { result: Mutex::new(Outcome::Pending), ready: Condvar::new() });
                        state.in_flight.insert(key.clone(), Arc::clone(&flight));
                        Role::Leader(flight)
                    }
                }
            };
            match role {
                Role::Follower(flight) => {
                    let mut result = flight.result.lock().unwrap_or_else(PoisonError::into_inner);
                    loop {
                        match &*result {
                            Outcome::Done(outcome) => return outcome.clone(),
                            Outcome::Abandoned => break,
                            Outcome::Pending => result = flight.ready.wait(result).unwrap_or_else(PoisonError::into_inner),
                        }
                    }
                }
                Role::Leader(flight) => {
                    let mut guard = LeaderGuard { state: &self.state, key: Some(key.clone()), flight: Arc::clone(&flight) };
                    let outcome = load();
                    guard.key = None;
                    {
                        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                        state.in_flight.remove(&key);
                        if let Ok(value) = &outcome {
                            self.insert_locked(&mut state, key, value.clone());
                        }
                    }
                    flight.finish(Outcome::Done(outcome.clone()));
                    return outcome;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn concurrent_requests_share_one_load_and_errors_are_not_cached() {
        let cache: Arc<LoadingCache<u32, u32, String>> = Arc::new(LoadingCache::new(4));
        let loads = Arc::new(AtomicUsize::new(0));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let (cache, loads) = (Arc::clone(&cache), Arc::clone(&loads));
                std::thread::spawn(move || {
                    cache.get_or_load(1, || {
                        loads.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(50));
                        Ok(10)
                    })
                })
            })
            .collect();
        for thread in threads {
            assert_eq!(thread.join().unwrap(), Ok(10));
        }
        assert_eq!(loads.load(Ordering::SeqCst), 1);
        assert_eq!(cache.get_or_load(2, || Err("down".to_owned())), Err("down".to_owned()));
        assert_eq!(cache.get_or_load(2, || Ok(20)), Ok(20));
    }

    /// A panicking load used to leave its key registered as in flight: followers waited
    /// forever and every later request for the key joined them.
    #[test]
    fn a_panicking_load_does_not_wedge_the_key() {
        let cache: Arc<LoadingCache<u32, u32, String>> = Arc::new(LoadingCache::new(4));
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let leader = {
            let cache = Arc::clone(&cache);
            std::thread::spawn(move || {
                cache.get_or_load(7, || {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    panic!("loader bug");
                })
            })
        };
        entered_rx.recv().unwrap();
        let follower = {
            let cache = Arc::clone(&cache);
            std::thread::spawn(move || cache.get_or_load(7, || Ok(70)))
        };
        std::thread::sleep(std::time::Duration::from_millis(50));
        release_tx.send(()).unwrap();
        assert!(leader.join().is_err(), "the leader's panic propagates to its own caller");
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || done_tx.send(follower.join().unwrap()).unwrap());
        assert_eq!(done_rx.recv_timeout(std::time::Duration::from_secs(5)), Ok(Ok(70)), "the follower must not wait forever");
        assert_eq!(cache.get_or_load(7, || Ok(0)), Ok(70));
    }

    #[test]
    fn evicts_the_least_recently_used_entry() {
        let cache: LoadingCache<u32, u32, ()> = LoadingCache::new(2);
        cache.insert(1, 1);
        cache.insert(2, 2);
        assert_eq!(cache.get(&1), Some(1));
        cache.insert(3, 3);
        assert_eq!((cache.get(&1), cache.get(&2), cache.get(&3)), (Some(1), None, Some(3)));
    }
}
