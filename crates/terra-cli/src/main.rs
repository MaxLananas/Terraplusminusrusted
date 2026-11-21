//! `terraplusminus`: operator tooling around the generation stack.
//!
//! ```text
//! terraplusminus check-config <config>
//! terraplusminus tpll <config> <arguments...>
//! terraplusminus column <config> <latitude> <longitude>
//! terraplusminus benchmark <config> <output.csv> [radius] [burst]
//! ```

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};
use terra_projection::GeoCoordinate;
use terra_server::{ServerConfig, TerraStack, parse_server_config, parse_tpll_arguments};

fn load(path: &str) -> Result<(ServerConfig, PathBuf), String> {
    let text = std::fs::read_to_string(path).map_err(|error| format!("reading {path}: {error}"))?;
    let config = parse_server_config(&text).map_err(|error| error.to_string())?;
    let base = Path::new(path).parent().map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    Ok((config, base))
}

fn build(path: &str) -> Result<TerraStack, String> {
    let (config, base) = load(path)?;
    TerraStack::build(config, &base).map_err(|error| error.to_string())
}

fn block_of(stack: &TerraStack, latitude: f64, longitude: f64) -> Result<(i32, i32), String> {
    let block = stack.generator().projection().from_geo(GeoCoordinate::new(longitude, latitude)).map_err(|error| error.to_string())?;
    Ok((block.x.floor() as i32, block.y.floor() as i32))
}

fn column(path: &str, latitude: f64, longitude: f64) -> Result<(), String> {
    let stack = build(path)?;
    let (x, z) = block_of(&stack, latitude, longitude)?;
    let terrain = stack.loader().load_blocking(x >> 4, z >> 4, stack.config().generation_wait).map_err(|error| error.to_string())?;
    let (local_x, local_z) = ((x & 15) as usize, (z & 15) as usize);
    let y_offset = stack.config().generator.y_offset;
    println!("block x={x} z={z} chunk=({}, {})", x >> 4, z >> 4);
    println!("surface_y={} ground_y={}", terrain.surface_height(local_x, local_z) + y_offset, terrain.ground_height(local_x, local_z) + y_offset);
    println!("biome={}", stack.generator().biome_at_block(x, z).resource_name());
    Ok(())
}

fn tpll(path: &str, arguments: &str) -> Result<(), String> {
    let stack = build(path)?;
    let target = parse_tpll_arguments(arguments).ok_or("Usage: /tpll <latitude> <longitude> [height]")?;
    let (x, z) = block_of(&stack, target.position.latitude, target.position.longitude)?;
    let terrain = stack.loader().load_blocking(x >> 4, z >> 4, stack.config().generation_wait).map_err(|error| error.to_string())?;
    let y = match target.height {
        Some(height) => height.floor() as i32 + stack.config().generator.y_offset,
        None => terrain.surface_height((x & 15) as usize, (z & 15) as usize) + stack.config().generator.y_offset + 1,
    };
    println!("{} {} -> x={x} y={y} z={z}", target.position.latitude, target.position.longitude);
    Ok(())
}

/// Raw per-request timings: cold chunk loads over a square, the same square warm, and a burst
/// of concurrent teleport-style requests to distinct far-apart chunks.
fn benchmark(path: &str, output: &str, radius: i32, burst: i32) -> Result<(), String> {
    let (config, base) = load(path)?;
    let stack = Arc::new(TerraStack::build(config, &base).map_err(|error| error.to_string())?);
    let centre = block_of(&stack, 35.995, -113.247)?;
    let (cx, cz) = (centre.0 >> 4, centre.1 >> 4);
    let wait = Duration::from_secs(300);
    let mut rows = vec!["phase,chunk_x,chunk_z,micros".to_owned()];
    for phase in ["cold", "warm"] {
        for chunk_x in cx - radius..=cx + radius {
            for chunk_z in cz - radius..=cz + radius {
                let started = Instant::now();
                stack.loader().load_blocking(chunk_x, chunk_z, wait).map_err(|error| error.to_string())?;
                rows.push(format!("{phase},{chunk_x},{chunk_z},{}", started.elapsed().as_micros()));
            }
        }
    }
    let burst_started = Instant::now();
    let handles: Vec<_> = (0..burst)
        .map(|index| {
            let stack = Arc::clone(&stack);
            let chunk = (cx + 3 * radius + 2 + index % 8, cz + index / 8);
            std::thread::spawn(move || {
                let started = Instant::now();
                stack.loader().load_blocking(chunk.0, chunk.1, wait).map(|_| (chunk, started.elapsed().as_micros())).map_err(|error| error.to_string())
            })
        })
        .collect();
    for handle in handles {
        let ((chunk_x, chunk_z), micros) = handle.join().map_err(|_| "benchmark thread failed".to_owned())??;
        rows.push(format!("burst,{chunk_x},{chunk_z},{micros}"));
    }
    rows.push(format!("burst_total,,,{}", burst_started.elapsed().as_micros()));
    std::fs::write(output, rows.join("\n") + "\n").map_err(|error| format!("writing {output}: {error}"))?;
    eprint!("{}", stack.diagnostics_report());
    println!("wrote {} measurements to {output}", rows.len() - 1);
    Ok(())
}

fn run(arguments: &[String]) -> Result<(), String> {
    let argument = |index: usize| arguments.get(index).map(String::as_str).ok_or_else(|| "missing argument; see the module documentation".to_owned());
    let number = |index: usize, default: i32| arguments.get(index).map_or(Ok(default), |text| text.parse::<i32>().map_err(|error| format!("{text}: {error}")));
    match argument(1)? {
        "check-config" => {
            let stack = build(argument(2)?)?;
            for skipped in stack.skipped_layers() {
                println!("skipped: {skipped}");
            }
            println!("configuration valid; fingerprint:\n{:?}", stack.fingerprint());
            Ok(())
        }
        "tpll" => tpll(argument(2)?, &arguments[3..].join(" ")),
        "column" => {
            let parse = |index: usize| argument(index).and_then(|text| text.parse::<f64>().map_err(|error| format!("{text}: {error}")));
            column(argument(2)?, parse(3)?, parse(4)?)
        }
        "benchmark" => benchmark(argument(2)?, argument(3)?, number(4, 3)?, number(5, 32)?),
        other => Err(format!("unknown command {other}")),
    }
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().collect();
    match run(&arguments) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}
