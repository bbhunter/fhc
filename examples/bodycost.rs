//! Measures what reading the response body costs a check.
//!
//! Point it at a host serving a large page:
//!     cargo run --release --example bodycost -- 127.0.0.4

use {
    fhc::{httplib, structs::LibOptions, utils},
    std::{collections::HashSet, time::Instant},
};

const ROUNDS: usize = 5;

async fn measure(host: &str, collect_body: bool) -> (f64, usize) {
    let options = LibOptions {
        hosts: HashSet::from([host.to_owned()]),
        client: httplib::return_http_client(30, 0),
        user_agents: utils::user_agents(),
        retries: 1,
        threads: 1,
        collect_body,
        ..LibOptions::default()
    };

    let started = Instant::now();
    let mut body_seen = 0;
    for _ in 0..ROUNDS {
        let data = httplib::return_http_data(&options).await;
        body_seen = data.values().map(|d| d.body.len()).sum();
    }
    (started.elapsed().as_secs_f64() / ROUNDS as f64, body_seen)
}

#[tokio::main]
async fn main() {
    let host = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.4".to_owned());

    let (without, no_body) = measure(&host, false).await;
    let (with, body) = measure(&host, true).await;

    println!("  collect_body=false  {without:.4}s/check  cuerpo retenido: {no_body} bytes");
    println!("  collect_body=true   {with:.4}s/check  cuerpo retenido: {body} bytes");
    println!("  factor: {:.1}x", with / without.max(f64::MIN_POSITIVE));
}
