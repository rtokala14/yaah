//! Dev tool for honing the index: build it over a real repo and inspect
//! what the model would see.
//!
//!   cargo run -p harness-index --example repomap -- [path] [query]
//!
//! Prints build/refresh timings, corpus stats, the token-budgeted repo
//! map, and (with a query) symbol search results.

use harness_index::{registry, CodeIndex};
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let root = args.next().unwrap_or_else(|| ".".into());
    let query = args.next();

    let t0 = Instant::now();
    let index = registry::shared_index(std::path::Path::new(&root));
    let build = t0.elapsed();

    let t1 = Instant::now();
    index.refresh_now();
    let refresh = t1.elapsed();

    let t2 = Instant::now();
    let map = index.repo_map(2000).unwrap_or_default();
    let map_time = t2.elapsed();

    println!(
        "== index: {} files | build {:?} | clean refresh {:?} | repo_map {:?} ==\n",
        index.file_count(),
        build,
        refresh,
        map_time
    );
    println!("== repo map (2000-token budget, {} chars) ==\n{map}\n", map.len());

    if let Some(q) = query {
        let t3 = Instant::now();
        match index.find_symbols(&q, 15) {
            Ok(symbols) => {
                println!("== symbols matching \"{q}\" ({:?}) ==", t3.elapsed());
                for s in symbols {
                    println!("{}:{}  {:?}  {}", s.file.display(), s.line, s.kind, s.signature);
                }
            }
            Err(e) => println!("symbol search failed: {e}"),
        }
    }
}
