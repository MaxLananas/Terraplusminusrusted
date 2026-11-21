//! Concurrency and failure behaviour of the chunk loader: deduplication, backpressure,
//! cancellation, error propagation without caching, timeouts and shutdown.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{centre_chunk, generator_with};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use terra_chunk_generation::{ChunkLoader, LoaderError, LoaderLimits, TerrainGenerator};
use terra_elevation::{ElevationError, Tile, TilePosition, TileProvider, TiledScalarDataset};

#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    changed: Condvar,
}

impl Gate {
    fn set(&self, open: bool) {
        *self.open.lock().unwrap() = open;
        self.changed.notify_all();
    }

    fn pass(&self) {
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.changed.wait(open).unwrap();
        }
    }
}

struct ControlledProvider {
    inner: Arc<dyn TileProvider>,
    gate: Arc<Gate>,
    fail_next: Arc<AtomicBool>,
    loads: Arc<AtomicUsize>,
}

impl TileProvider for ControlledProvider {
    fn load(&self, dataset: &TiledScalarDataset, position: TilePosition) -> Result<Option<Tile>, ElevationError> {
        self.gate.pass();
        self.loads.fetch_add(1, Ordering::SeqCst);
        if self.fail_next.swap(false, Ordering::SeqCst) {
            return Err(ElevationError::Fetch { url: "test://tile".into(), reason: "injected failure".into() });
        }
        self.inner.load(dataset, position)
    }
}

struct Harness {
    loader: ChunkLoader,
    gate: Arc<Gate>,
    fail_next: Arc<AtomicBool>,
    loads: Arc<AtomicUsize>,
    centre: (i32, i32),
}

fn harness(workers: usize, queue_capacity: usize, open: bool) -> Harness {
    let gate = Arc::new(Gate::default());
    gate.set(open);
    let fail_next = Arc::new(AtomicBool::new(false));
    let loads = Arc::new(AtomicUsize::new(0));
    let (g, f, l) = (Arc::clone(&gate), Arc::clone(&fail_next), Arc::clone(&loads));
    let generator: TerrainGenerator = generator_with(false, move |inner| Arc::new(ControlledProvider { inner, gate: g, fail_next: f, loads: l }));
    let centre = centre_chunk(&generator);
    let loader = ChunkLoader::new(Arc::new(generator), LoaderLimits { workers, queue_capacity, ready_capacity: 64 }).unwrap();
    Harness { loader, gate, fail_next, loads, centre }
}

fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "condition not reached");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn concurrent_requests_for_one_chunk_generate_it_once() {
    let h = harness(2, 16, false);
    let (x, z) = h.centre;
    let tickets: Vec<_> = (0..8).map(|_| h.loader.request(x, z).unwrap()).collect();
    h.gate.set(true);
    let results: Vec<_> = tickets.into_iter().map(|mut ticket| ticket.wait(Duration::from_secs(10)).unwrap()).collect();
    assert!(results.windows(2).all(|pair| Arc::ptr_eq(&pair[0], &pair[1])));
    let diagnostics = h.loader.diagnostics();
    assert_eq!((diagnostics.completed, diagnostics.deduplicated), (1, 7));
    assert!(h.loader.is_ready(x, z));
}

#[test]
fn full_queue_refuses_new_work_and_recovers() {
    let h = harness(1, 2, false);
    let (x, z) = h.centre;
    let _running = h.loader.request(x, z).unwrap();
    wait_until(|| h.loader.diagnostics().running == 1);
    let _queued_a = h.loader.request(x + 1, z).unwrap();
    let _queued_b = h.loader.request(x + 2, z).unwrap();
    assert_eq!(h.loader.request(x + 3, z).err(), Some(LoaderError::Overloaded));
    assert!(h.loader.request(x + 1, z).is_ok(), "joining queued work is not refused");
    assert_eq!(h.loader.diagnostics().rejected, 1);
    h.gate.set(true);
    assert!(h.loader.load_blocking(x + 3, z, Duration::from_secs(10)).is_ok());
}

#[test]
fn dropping_all_tickets_cancels_queued_work() {
    let h = harness(1, 8, false);
    let (x, z) = h.centre;
    let mut running = h.loader.request(x, z).unwrap();
    wait_until(|| h.loader.diagnostics().running == 1);
    let first = h.loader.request(x + 5, z).unwrap();
    let second = h.loader.request(x + 5, z).unwrap();
    drop(first);
    assert_eq!(h.loader.diagnostics().cancelled, 0);
    drop(second);
    let diagnostics = h.loader.diagnostics();
    assert_eq!((diagnostics.cancelled, diagnostics.queued), (1, 0));
    h.gate.set(true);
    running.wait(Duration::from_secs(10)).unwrap();
    assert!(!h.loader.is_ready(x + 5, z));
    assert_eq!(h.loader.diagnostics().completed, 1);
}

#[test]
fn failures_reach_every_waiter_and_are_not_cached() {
    let h = harness(1, 8, false);
    let (x, z) = h.centre;
    h.fail_next.store(true, Ordering::SeqCst);
    let mut first = h.loader.request(x, z).unwrap();
    let mut second = h.loader.request(x, z).unwrap();
    h.gate.set(true);
    for ticket in [&mut first, &mut second] {
        match ticket.wait(Duration::from_secs(10)) {
            Err(LoaderError::Generation(error)) => assert!(error.to_string().contains("injected failure"), "{error}"),
            other => panic!("expected failure, got {:?}", other.map(|_| ())),
        }
    }
    let loads_after_failure = h.loads.load(Ordering::SeqCst);
    assert!(h.loader.load_blocking(x, z, Duration::from_secs(10)).is_ok());
    assert!(h.loads.load(Ordering::SeqCst) > loads_after_failure);
    assert_eq!(h.loader.diagnostics().failed, 1);
}

#[test]
fn waiting_times_out_and_shutdown_is_prompt() {
    let h = harness(1, 8, false);
    let (x, z) = h.centre;
    assert_eq!(h.loader.load_blocking(x, z, Duration::from_millis(50)).err(), Some(LoaderError::TimedOut));
    let mut pending = h.loader.request(x + 1, z).unwrap();
    let gate = Arc::clone(&h.gate);
    let opener = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        gate.set(true);
    });
    let started = Instant::now();
    drop(h.loader);
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(pending.wait(Duration::from_secs(1)).err(), Some(LoaderError::ShutDown));
    opener.join().unwrap();
}
