//! `reduced_console_messages`: upstream's `config.yml` ships `true`; `false` logs every data
//! request, as TMM's `Http` does when its reduced-messages switch is off.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::{Arc, Mutex};
use terra_projection::GeoCoordinate;
use terra_server::{GenerationControl, LogSink, TerraStack, terrain_for_generation};

fn canyon_chunk(stack: &TerraStack) -> (i32, i32) {
    let block = stack.generator().projection().from_geo(GeoCoordinate::new(-113.247, 35.995)).unwrap();
    ((block.x.floor() as i32) >> 4, (block.y.floor() as i32) >> 4)
}

fn generate_with_log(name: &str, extra: &str) -> Vec<String> {
    let (config, directory) = common::config(name, extra);
    let lines: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink_lines = Arc::clone(&lines);
    let sink: LogSink = Arc::new(move |message: &str| sink_lines.lock().unwrap().push(message.to_owned()));
    let stack = TerraStack::build_logged(config, &directory, sink).unwrap();
    let (chunk_x, chunk_z) = canyon_chunk(&stack);
    terrain_for_generation(&stack, &GenerationControl::default(), chunk_x, chunk_z, &mut |message| panic!("{message}")).unwrap();
    stack.shut_down();
    lines.lock().unwrap().clone()
}

#[test]
fn full_console_messages_log_each_request() {
    let lines = generate_with_log("console-full", r#", "reduced_console_messages": false"#);
    assert!(lines.iter().any(|line| line.starts_with("Fetched mirror://terrarium/12/") && line.contains(" bytes in ")), "{lines:?}");
}

#[test]
fn reduced_console_messages_stay_quiet() {
    let lines = generate_with_log("console-reduced", "");
    assert!(lines.iter().all(|line| !line.starts_with("Fetched")), "{lines:?}");
}
