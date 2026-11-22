//! `CachingTileProvider` when the fetch of a shared tile panics: the leader's panic stays with its
//! caller, while followers and later requests for the tile start over instead of blocking forever.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use terra_elevation::{BlendMode, CachingTileProvider, ElevationError, FetchOutcome, TileFetcher, TileFormat, TilePosition, TileProvider, TiledScalarDataset};
use terra_projection::{SharedProjection, WebMercatorProjection};

/// The first fetch announces itself, waits for a release and panics; later fetches return a tile.
struct PanicOnce {
    first: AtomicBool,
    entered: Mutex<Sender<()>>,
    release: Mutex<Receiver<()>>,
    tile: Vec<u8>,
}

impl TileFetcher for PanicOnce {
    fn fetch(&self, _url: &str) -> Result<FetchOutcome, ElevationError> {
        if self.first.swap(false, Ordering::SeqCst) {
            self.entered.lock().unwrap().send(()).unwrap();
            self.release.lock().unwrap().recv().unwrap();
            panic!("decoder bug");
        }
        Ok(FetchOutcome::Found(self.tile.clone()))
    }
}

#[test]
fn a_panicking_fetch_does_not_wedge_the_tile() {
    let tile = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/elevation/terrarium/12/759/1608.png")).unwrap();
    let (entered_tx, entered_rx) = channel();
    let (release_tx, release_rx) = channel();
    let fetcher = PanicOnce { first: AtomicBool::new(true), entered: Mutex::new(entered_tx), release: Mutex::new(release_rx), tile };
    let provider = Arc::new(CachingTileProvider::new(fetcher, 4));
    let projection: SharedProjection = Arc::new(WebMercatorProjection::new(12).unwrap());
    let dataset = Arc::new(TiledScalarDataset::new("test".into(), vec!["test://${x}/${z}.png".into()], projection, 256, BlendMode::Cubic, TileFormat::TerrariumPng).unwrap());
    let position = TilePosition { x: 759, z: 1608 };

    let spawn_load = || {
        let (provider, dataset) = (Arc::clone(&provider), Arc::clone(&dataset));
        std::thread::spawn(move || provider.load(&dataset, position).map(|tile| tile.map(|t| t.len())))
    };
    let leader = spawn_load();
    entered_rx.recv().unwrap();
    let follower = spawn_load();
    std::thread::sleep(Duration::from_millis(50));
    release_tx.send(()).unwrap();
    assert!(leader.join().is_err(), "the leader's panic reaches its own caller");

    let (done_tx, done_rx) = channel();
    std::thread::spawn(move || done_tx.send(follower.join().unwrap()).unwrap());
    let joined = done_rx.recv_timeout(Duration::from_secs(5)).expect("the follower blocked forever on the abandoned fetch");
    assert_eq!(joined.unwrap(), Some(256 * 256));
    assert_eq!(provider.load(&dataset, position).unwrap().map(|t| t.len()), Some(256 * 256));
}
