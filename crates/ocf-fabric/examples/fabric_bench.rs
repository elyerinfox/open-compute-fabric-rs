//! Benchmark the encrypted host-to-host fabric transport.
//!
//! This measures the cost of the [`NoiseTransport`] / [`FabricServer`] path —
//! the encrypted channel that carries cross-node traffic — over loopback, so it
//! isolates the fabric's own overhead (Noise crypto + framing + the
//! request/response RPC) from physical-network latency. It reports:
//!
//!   1. Noise XX handshake (session establishment) latency.
//!   2. Encrypted RPC round-trip latency by payload size (p50/p90/p99).
//!   3. Throughput (msgs/s and MB/s) by payload size.
//!   4. Crypto overhead vs a raw TCP echo baseline.
//!   5. Concurrency scaling (K parallel sessions).
//!
//! Run with: `cargo run -p ocf-fabric --release --example fabric_bench`
//! (release matters — the crypto is much slower unoptimized).

use std::time::{Duration, Instant};

use ocf_fabric::{FabricNode, FabricServer, FabricStreamServer, FabricTransport, KeyPair, NoiseTransport};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Payload sizes to sweep. Capped below the 65535-byte Noise message limit.
const SIZES: &[(usize, &str)] = &[
    (64, "64 B"),
    (1024, "1 KB"),
    (8192, "8 KB"),
    (61440, "60 KB"),
];

#[tokio::main]
async fn main() {
    println!("=== OCF fabric transport benchmark (loopback, release) ===\n");

    // --- Stand up an echo FabricServer with a fixed identity. ---------------
    let server_kp = KeyPair::from_seed_name("bench-server");
    let server = FabricServer::bind("127.0.0.1:0", server_kp.clone())
        .await
        .expect("bind fabric server");
    let addr = server.local_addr();
    tokio::spawn(server.run(|_peer, req| async move { req }));
    let server_node = FabricNode::from_keypair(&server_kp, vec![addr.to_string()]);
    let client_kp = KeyPair::from_seed_name("bench-client");

    // ---------------------------------------------------------------------
    bench_handshake(&server_node, &client_kp).await;
    bench_rpc_latency(&server_node, &client_kp).await;
    bench_throughput(&server_node, &client_kp).await;
    bench_vs_raw_tcp(&server_node, &client_kp).await;
    bench_concurrency(&server_node).await;
    bench_streaming(&client_kp).await;

    println!("\nNote: loopback isolates fabric overhead (crypto + framing + RPC).");
    println!("Cross-node *workload* data-plane traffic rides the kernel VXLAN overlay");
    println!("(near line rate); this benchmarks the encrypted control/fabric channel.");
}

// === 1. Handshake latency ==================================================

async fn bench_handshake(server: &FabricNode, client_kp: &KeyPair) {
    const N: usize = 1000;
    // Warmup.
    for _ in 0..50 {
        let c = NoiseTransport::with_keypair(client_kp.clone());
        c.connect(server).await.expect("connect");
    }
    let mut durs = Vec::with_capacity(N);
    for _ in 0..N {
        let c = NoiseTransport::with_keypair(client_kp.clone());
        let t = Instant::now();
        c.connect(server).await.expect("connect");
        durs.push(t.elapsed());
    }
    let s = stats(&mut durs);
    println!("[1] Noise XX handshake — fresh session establishment (N={N})");
    println!(
        "    mean {:>8}  p50 {:>8}  p90 {:>8}  p99 {:>8}  max {:>8}\n",
        us(s.mean),
        us(s.p50),
        us(s.p90),
        us(s.p99),
        us(s.max)
    );
}

// === 2. RPC round-trip latency =============================================

async fn bench_rpc_latency(server: &FabricNode, client_kp: &KeyPair) {
    const N: usize = 5000;
    let client = NoiseTransport::with_keypair(client_kp.clone());
    client.connect(server).await.expect("connect");

    println!("[2] Encrypted RPC round-trip latency (established session, N={N})");
    println!(
        "    {:<8} {:>9} {:>9} {:>9} {:>9} {:>9}",
        "size", "mean", "p50", "p90", "p99", "max"
    );
    for &(size, label) in SIZES {
        let payload = vec![0xABu8; size];
        for _ in 0..200 {
            client.request(server, &payload).await.expect("rpc");
        }
        let mut durs = Vec::with_capacity(N);
        for _ in 0..N {
            let t = Instant::now();
            client.request(server, &payload).await.expect("rpc");
            durs.push(t.elapsed());
        }
        let s = stats(&mut durs);
        println!(
            "    {:<8} {:>9} {:>9} {:>9} {:>9} {:>9}",
            label,
            us(s.mean),
            us(s.p50),
            us(s.p90),
            us(s.p99),
            us(s.max)
        );
    }
    println!();
}

// === 3. Throughput =========================================================

async fn bench_throughput(server: &FabricNode, client_kp: &KeyPair) {
    let client = NoiseTransport::with_keypair(client_kp.clone());
    client.connect(server).await.expect("connect");

    println!("[3] Throughput — serial request/response, application payload");
    println!("    {:<8} {:>14} {:>12}", "size", "msgs/s", "MB/s");
    for &(size, label) in SIZES {
        let payload = vec![0x5Au8; size];
        // Aim for ~roughly a second of work; small payloads need more messages.
        let iters = (64 * 1024 * 1024 / size).clamp(2000, 400_000);
        // Warmup.
        for _ in 0..200 {
            client.request(server, &payload).await.expect("rpc");
        }
        let t = Instant::now();
        for _ in 0..iters {
            client.request(server, &payload).await.expect("rpc");
        }
        let elapsed = t.elapsed().as_secs_f64();
        let msgs_s = iters as f64 / elapsed;
        let mb_s = (iters as f64 * size as f64) / elapsed / (1024.0 * 1024.0);
        println!("    {:<8} {:>14.0} {:>12.1}", label, msgs_s, mb_s);
    }
    println!("    (MB/s counts one-way application bytes; the wire also carries the echo.)\n");
}

// === 4. Crypto overhead vs raw TCP =========================================

async fn bench_vs_raw_tcp(server: &FabricNode, client_kp: &KeyPair) {
    // Raw length-prefixed TCP echo server (no crypto) for the baseline.
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind raw");
    let raw_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                loop {
                    let len = match sock.read_u32().await {
                        Ok(n) => n as usize,
                        Err(_) => break,
                    };
                    let mut buf = vec![0u8; len];
                    if sock.read_exact(&mut buf).await.is_err() {
                        break;
                    }
                    if sock.write_u32(len as u32).await.is_err()
                        || sock.write_all(&buf).await.is_err()
                    {
                        break;
                    }
                }
            });
        }
    });

    let noise = NoiseTransport::with_keypair(client_kp.clone());
    noise.connect(server).await.expect("connect");
    let mut raw = TcpStream::connect(raw_addr).await.expect("raw connect");
    raw.set_nodelay(true).ok();

    const N: usize = 5000;
    println!("[4] Crypto overhead — Noise RPC vs raw TCP echo (p50 round-trip, N={N})");
    println!(
        "    {:<8} {:>11} {:>11} {:>10}",
        "size", "noise p50", "raw p50", "overhead"
    );
    for &(size, label) in SIZES {
        let payload = vec![0x33u8; size];

        // Noise.
        for _ in 0..200 {
            noise.request(server, &payload).await.expect("rpc");
        }
        let mut nd = Vec::with_capacity(N);
        for _ in 0..N {
            let t = Instant::now();
            noise.request(server, &payload).await.expect("rpc");
            nd.push(t.elapsed());
        }
        let ns = stats(&mut nd);

        // Raw TCP.
        let mut rd = Vec::with_capacity(N);
        let mut rbuf = vec![0u8; size];
        for _ in 0..(N + 200) {
            let measure = rd.len() < N;
            let t = Instant::now();
            raw.write_u32(size as u32).await.unwrap();
            raw.write_all(&payload).await.unwrap();
            let _ = raw.read_u32().await.unwrap();
            raw.read_exact(&mut rbuf).await.unwrap();
            if measure {
                rd.push(t.elapsed());
            }
        }
        let rs = stats(&mut rd);

        let overhead = ns.p50.as_secs_f64() / rs.p50.as_secs_f64().max(1e-9);
        println!(
            "    {:<8} {:>11} {:>11} {:>9.2}x",
            label,
            us(ns.p50),
            us(rs.p50),
            overhead
        );
    }
    println!();
}

// === 5. Concurrency scaling ================================================

async fn bench_concurrency(server: &FabricNode) {
    const PER_CLIENT: usize = 4000;
    const SIZE: usize = 1024;
    println!("[5] Concurrency — K parallel sessions, {PER_CLIENT} RPCs each, 1 KB");
    println!("    {:<6} {:>14} {:>14}", "K", "agg msgs/s", "per-client/s");
    for k in [1usize, 4, 16, 64] {
        let t = Instant::now();
        let mut handles = Vec::with_capacity(k);
        for _ in 0..k {
            let node = server.clone();
            handles.push(tokio::spawn(async move {
                let c = NoiseTransport::new();
                c.connect(&node).await.expect("connect");
                let payload = vec![0x77u8; SIZE];
                for _ in 0..PER_CLIENT {
                    c.request(&node, &payload).await.expect("rpc");
                }
            }));
        }
        for h in handles {
            h.await.expect("client task");
        }
        let elapsed = t.elapsed().as_secs_f64();
        let total = (k * PER_CLIENT) as f64;
        println!(
            "    {:<6} {:>14.0} {:>14.0}",
            k,
            total / elapsed,
            (PER_CLIENT as f64) / elapsed
        );
    }
    println!();
}

// === 6. Streaming throughput (the big-transfer path) =======================

async fn bench_streaming(client_kp: &KeyPair) {
    // One raw stream server and one zstd-decompressing stream server.
    let raw_kp = KeyPair::from_seed_name("bench-stream-raw");
    let raw_srv = FabricStreamServer::bind("127.0.0.1:0", raw_kp.clone())
        .await
        .expect("bind raw stream server");
    let raw_node = FabricNode::from_keypair(&raw_kp, vec![raw_srv.local_addr().to_string()]);
    tokio::spawn(raw_srv.run(false, |_pk| tokio::io::sink()));

    let zst_kp = KeyPair::from_seed_name("bench-stream-zstd");
    let zst_srv = FabricStreamServer::bind("127.0.0.1:0", zst_kp.clone())
        .await
        .expect("bind zstd stream server");
    let zst_node = FabricNode::from_keypair(&zst_kp, vec![zst_srv.local_addr().to_string()]);
    tokio::spawn(zst_srv.run(true, |_pk| tokio::io::sink()));

    let client = NoiseTransport::with_keypair(client_kp.clone());
    const GIB: u64 = 1024 * 1024 * 1024;

    println!("[6] Streaming throughput — pipelined sealed records (bulk transfer path)");
    println!("    {:<28} {:>8} {:>12} {:>9}", "mode / data", "time", "MB/s", "GB/s");

    // Warmup both paths.
    client.send_stream(&raw_node, &mut tokio::io::repeat(0).take(32 << 20), false).await.ok();
    client.send_stream(&zst_node, &mut tokio::io::repeat(0).take(32 << 20), true).await.ok();

    // Raw: incompressible-equivalent (cipher is the bottleneck regardless of data).
    let t = Instant::now();
    let sent = client.send_stream(&raw_node, &mut tokio::io::repeat(0xAB).take(GIB), false).await.expect("raw");
    report("raw / 1 GiB", sent, t.elapsed().as_secs_f64());

    // Compressed: sparse/repetitive data (the realistic VM-memory case — zero
    // pages, repeated structure). Effective (uncompressed) throughput climbs
    // because far fewer bytes hit the cipher and the wire.
    let t = Instant::now();
    let sent = client.send_stream(&zst_node, &mut tokio::io::repeat(0x00).take(GIB), true).await.expect("zstd");
    report("zstd / 1 GiB sparse", sent, t.elapsed().as_secs_f64());

    // The standalone zstd ratio on one 64 KB window of representative sparse data.
    let sample: Vec<u8> = (0..(64 * 1024)).map(|i| if i % 64 == 0 { (i % 251) as u8 } else { 0 }).collect();
    let comp = zstd::bulk::compress(&sample, 3).unwrap();
    println!(
        "    zstd ratio on a sparse 64 KB window: {:.1}x  ({} -> {} bytes)",
        sample.len() as f64 / comp.len() as f64,
        sample.len(),
        comp.len()
    );
    println!("    (one encrypted connection, no per-record round-trip — RTT-independent;");
    println!("     compression trades CPU for far fewer encrypted/wire bytes on real data.)\n");
}

fn report(label: &str, bytes: u64, secs: f64) {
    let mbps = bytes as f64 / secs / (1024.0 * 1024.0);
    println!("    {:<28} {:>7.2}s {:>12.1} {:>9.2}", label, secs, mbps, mbps / 1024.0);
}

// === helpers ===============================================================

struct Stats {
    mean: Duration,
    p50: Duration,
    p90: Duration,
    p99: Duration,
    max: Duration,
}

fn stats(durs: &mut [Duration]) -> Stats {
    durs.sort_unstable();
    let n = durs.len();
    let sum: Duration = durs.iter().sum();
    let pct = |p: f64| durs[((n as f64 * p) as usize).min(n - 1)];
    Stats {
        mean: sum / n as u32,
        p50: pct(0.50),
        p90: pct(0.90),
        p99: pct(0.99),
        max: durs[n - 1],
    }
}

/// Format a duration in microseconds (the natural unit on loopback).
fn us(d: Duration) -> String {
    format!("{:.1}µs", d.as_secs_f64() * 1e6)
}
