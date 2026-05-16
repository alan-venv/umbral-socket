use std::env;
use std::io;
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use actix_web::{App, HttpResponse, HttpServer, web};
use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::{Mutex, MutexGuard};
use tokio::time::sleep;
use umbral_socket::stream::{DEFAULT_MAX_PAYLOAD_LEN, UmbralClient, UmbralResponse, UmbralServer};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const DEFAULT_CONNECTIONS: usize = 4;
const DEFAULT_CONCURRENCY: usize = 64;
const DEFAULT_REQUESTS: usize = 100_000;
const DEFAULT_PAYLOAD_BYTES: usize = 32;
const DEFAULT_WARMUP: usize = 1_000;
const DEFAULT_UMBRAL_SOCKET: &str = "/tmp/umbral-compare.sock";
const DEFAULT_ACTIX_SOCKET: &str = "/tmp/actix-compare.sock";
const MAX_HTTP_HEADER_LEN: usize = 16 * 1024;

#[derive(Default)]
struct State;

#[derive(Clone)]
struct Config {
    connections: usize,
    concurrency: usize,
    requests: usize,
    payload_bytes: usize,
    warmup: usize,
    actix_workers: usize,
    umbral_socket: String,
    actix_socket: String,
}

struct BenchResult {
    latencies: Vec<Duration>,
    total_time: Duration,
}

struct HttpUnixClient {
    socket: Arc<str>,
    slots: Vec<HttpConnectionSlot>,
    next: AtomicUsize,
    request_head: Arc<Vec<u8>>,
    response_body_len: usize,
}

struct HttpConnectionSlot {
    stream: Mutex<HttpConnection>,
}

struct HttpConnection {
    stream: UnixStream,
    header_buffer: Vec<u8>,
    body_buffer: Vec<u8>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            connections: DEFAULT_CONNECTIONS,
            concurrency: DEFAULT_CONCURRENCY,
            requests: DEFAULT_REQUESTS,
            payload_bytes: DEFAULT_PAYLOAD_BYTES,
            warmup: DEFAULT_WARMUP,
            actix_workers: default_actix_workers(),
            umbral_socket: DEFAULT_UMBRAL_SOCKET.to_string(),
            actix_socket: DEFAULT_ACTIX_SOCKET.to_string(),
        }
    }
}

impl Config {
    fn validate(&self) -> io::Result<()> {
        if self.connections == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "connections must be greater than zero",
            ));
        }
        if self.concurrency == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "concurrency must be greater than zero",
            ));
        }
        if self.requests == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "requests must be greater than zero",
            ));
        }
        if self.payload_bytes > DEFAULT_MAX_PAYLOAD_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("payload-bytes must be <= {DEFAULT_MAX_PAYLOAD_LEN}"),
            ));
        }
        if self.actix_workers == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "actix-workers must be greater than zero",
            ));
        }
        Ok(())
    }
}

impl HttpUnixClient {
    async fn new(socket: &str, connections: usize, payload_bytes: usize) -> io::Result<Self> {
        if connections == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "connections must be greater than zero",
            ));
        }

        let socket: Arc<str> = Arc::from(socket);
        let request_head = Arc::new(
            format!(
                "POST /echo HTTP/1.1\r\nHost: localhost\r\nContent-Length: {payload_bytes}\r\nConnection: keep-alive\r\n\r\n"
            )
            .into_bytes(),
        );
        let mut slots = Vec::with_capacity(connections);

        for _ in 0..connections {
            let stream = UnixStream::connect(socket.as_ref()).await?;
            slots.push(HttpConnectionSlot {
                stream: Mutex::new(HttpConnection {
                    stream,
                    header_buffer: Vec::with_capacity(1024),
                    body_buffer: Vec::with_capacity(payload_bytes),
                }),
            });
        }

        Ok(Self {
            socket,
            slots,
            next: AtomicUsize::new(0),
            request_head,
            response_body_len: payload_bytes,
        })
    }

    async fn acquire_slot(&self) -> MutexGuard<'_, HttpConnection> {
        let start = self.next.fetch_add(1, Ordering::Relaxed) % self.slots.len();

        for offset in 0..self.slots.len() {
            let index = (start + offset) % self.slots.len();
            if let Ok(guard) = self.slots[index].stream.try_lock() {
                return guard;
            }
        }

        self.slots[start].stream.lock().await
    }

    async fn send(&self, payload: &[u8]) -> io::Result<()> {
        self.send_inner(payload, None).await
    }

    async fn send_checked(&self, payload: &[u8]) -> io::Result<()> {
        self.send_inner(payload, Some(payload)).await
    }

    async fn send_inner(&self, payload: &[u8], expected_body: Option<&[u8]>) -> io::Result<()> {
        if payload.len() != self.response_body_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "payload length must match configured length",
            ));
        }

        let mut connection = self.acquire_slot().await;
        connection.stream.write_all(&self.request_head).await?;
        connection.stream.write_all(payload).await?;

        let HttpConnection {
            stream,
            header_buffer,
            body_buffer,
        } = &mut *connection;
        read_http_response(stream, header_buffer, body_buffer).await?;

        if body_buffer.len() != self.response_body_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "unexpected HTTP response body length from {}: got {}, expected {}",
                    self.socket,
                    body_buffer.len(),
                    self.response_body_len
                ),
            ));
        }

        if let Some(expected_body) = expected_body {
            if body_buffer.as_slice() != expected_body {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unexpected HTTP response body from {}", self.socket),
                ));
            }
        }

        Ok(())
    }
}

async fn actix_echo(body: web::Bytes) -> HttpResponse {
    HttpResponse::Ok()
        .insert_header(("Connection", "keep-alive"))
        .body(body)
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let config = parse_args()?;
    config.validate()?;

    remove_socket_if_exists(&config.umbral_socket).await?;
    remove_socket_if_exists(&config.actix_socket).await?;

    let umbral_socket = config.umbral_socket.clone();
    let umbral_handle = tokio::spawn(async move {
        let _ = UmbralServer::new(State)
            .route(1, |_, _, _| Ok(UmbralResponse::RequestPayload))
            .run(&umbral_socket)
            .await;
    });

    let actix_socket = config.actix_socket.clone();
    let server = HttpServer::new(|| App::new().route("/echo", web::post().to(actix_echo)))
        .workers(config.actix_workers)
        .bind_uds(&actix_socket)?
        .run();
    let actix_handle = tokio::spawn(server);

    let result = async {
        wait_for_socket(&config.umbral_socket).await?;
        wait_for_socket(&config.actix_socket).await?;

        let payload = Bytes::from(vec![b'x'; config.payload_bytes]);
        let umbral = bench_umbral(&config, payload.clone()).await?;
        let actix = bench_actix(&config, payload).await?;

        Ok::<_, BoxError>((umbral, actix))
    }
    .await;

    umbral_handle.abort();
    actix_handle.abort();
    let _ = remove_socket_if_exists(&config.umbral_socket).await;
    let _ = remove_socket_if_exists(&config.actix_socket).await;

    let (umbral, actix) = result?;
    print_summary(&config, umbral, actix);

    Ok(())
}

async fn bench_umbral(config: &Config, payload: Bytes) -> Result<BenchResult, BoxError> {
    let client = Arc::new(UmbralClient::new(&config.umbral_socket, config.connections).await?);
    validate_umbral_echo(&client, &payload).await?;

    for _ in 0..config.warmup {
        client.send_with(1, payload.as_ref(), |_| Ok(())).await?;
    }

    let benchmark_start = Instant::now();
    let mut handles = Vec::with_capacity(config.concurrency);
    let per_task = config.requests / config.concurrency;
    let remainder = config.requests % config.concurrency;

    for task_index in 0..config.concurrency {
        let client = client.clone();
        let payload = payload.clone();
        let task_requests = per_task + usize::from(task_index < remainder);

        handles.push(tokio::spawn(async move {
            let mut latencies = Vec::with_capacity(task_requests);
            for _ in 0..task_requests {
                let start = Instant::now();
                client.send_with(1, payload.as_ref(), |_| Ok(())).await?;
                latencies.push(start.elapsed());
            }
            Ok::<_, io::Error>(latencies)
        }));
    }

    collect_benchmark(config.requests, benchmark_start, handles).await
}

async fn bench_actix(config: &Config, payload: Bytes) -> Result<BenchResult, BoxError> {
    let client = Arc::new(
        HttpUnixClient::new(
            &config.actix_socket,
            config.connections,
            config.payload_bytes,
        )
        .await?,
    );
    client.send_checked(&payload).await?;

    for _ in 0..config.warmup {
        client.send(&payload).await?;
    }

    let benchmark_start = Instant::now();
    let mut handles = Vec::with_capacity(config.concurrency);
    let per_task = config.requests / config.concurrency;
    let remainder = config.requests % config.concurrency;

    for task_index in 0..config.concurrency {
        let client = client.clone();
        let payload = payload.clone();
        let task_requests = per_task + usize::from(task_index < remainder);

        handles.push(tokio::spawn(async move {
            let mut latencies = Vec::with_capacity(task_requests);
            for _ in 0..task_requests {
                let start = Instant::now();
                client.send(&payload).await?;
                latencies.push(start.elapsed());
            }
            Ok::<_, io::Error>(latencies)
        }));
    }

    collect_benchmark(config.requests, benchmark_start, handles).await
}

async fn collect_benchmark(
    requests: usize,
    benchmark_start: Instant,
    handles: Vec<tokio::task::JoinHandle<io::Result<Vec<Duration>>>>,
) -> Result<BenchResult, BoxError> {
    let mut latencies = Vec::with_capacity(requests);
    for handle in handles {
        latencies.extend(handle.await??);
    }

    if latencies.len() != requests {
        return Err(io::Error::other(format!(
            "collected {} latencies, expected {}",
            latencies.len(),
            requests
        ))
        .into());
    }

    Ok(BenchResult {
        latencies,
        total_time: benchmark_start.elapsed(),
    })
}

async fn read_http_response(
    stream: &mut UnixStream,
    header_buffer: &mut Vec<u8>,
    body_buffer: &mut Vec<u8>,
) -> io::Result<()> {
    header_buffer.clear();
    body_buffer.clear();

    let header_end = loop {
        if let Some(index) = find_header_end(header_buffer) {
            break index;
        }

        if header_buffer.len() >= MAX_HTTP_HEADER_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP response headers exceeded maximum length",
            ));
        }

        let mut chunk = [0u8; 1024];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "HTTP response ended before headers completed",
            ));
        }
        header_buffer.extend_from_slice(&chunk[..read]);
    };

    let body_start = header_end + 4;
    let header = std::str::from_utf8(&header_buffer[..header_end]).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "HTTP response headers were not valid UTF-8",
        )
    })?;
    let content_length = parse_content_length(header)?;
    validate_status(header)?;

    body_buffer.extend_from_slice(&header_buffer[body_start..]);
    if body_buffer.len() > content_length {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "HTTP response contained unexpected bytes after body",
        ));
    }

    let already_read = body_buffer.len();
    body_buffer.resize(content_length, 0);
    stream.read_exact(&mut body_buffer[already_read..]).await?;

    Ok(())
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn validate_status(header: &str) -> io::Result<()> {
    let status_line = header.lines().next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "HTTP response missing status line",
        )
    })?;

    if status_line.starts_with("HTTP/1.1 200") || status_line.starts_with("HTTP/1.0 200") {
        return Ok(());
    }

    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!("unexpected HTTP status line: {status_line}"),
    ))
}

fn parse_content_length(header: &str) -> io::Result<usize> {
    for line in header.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };

        if name.eq_ignore_ascii_case("content-length") {
            return value.trim().parse().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid HTTP Content-Length header",
                )
            });
        }
    }

    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "HTTP response missing Content-Length header",
    ))
}

fn parse_args() -> io::Result<Config> {
    let mut config = Config::default();
    let mut args = env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--connections" => config.connections = parse_usize_arg(&arg, args.next())?,
            "--concurrency" => config.concurrency = parse_usize_arg(&arg, args.next())?,
            "--requests" => config.requests = parse_usize_arg(&arg, args.next())?,
            "--payload-bytes" => config.payload_bytes = parse_usize_arg(&arg, args.next())?,
            "--warmup" => config.warmup = parse_usize_arg(&arg, args.next())?,
            "--actix-workers" => config.actix_workers = parse_usize_arg(&arg, args.next())?,
            "--umbral-socket" => {
                config.umbral_socket = args.next().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "--umbral-socket requires a value",
                    )
                })?;
            }
            "--actix-socket" => {
                config.actix_socket = args.next().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "--actix-socket requires a value",
                    )
                })?;
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unknown argument: {arg}"),
                ));
            }
        }
    }

    Ok(config)
}

async fn validate_umbral_echo(client: &UmbralClient, payload: &Bytes) -> io::Result<()> {
    client
        .send_with(1, payload.as_ref(), |response| {
            if response == payload.as_ref() {
                return Ok(());
            }
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected Umbral response body",
            ))
        })
        .await
}

fn default_actix_workers() -> usize {
    std::thread::available_parallelism()
        .map(|workers| workers.get())
        .unwrap_or(1)
}

fn parse_usize_arg(name: &str, value: Option<String>) -> io::Result<usize> {
    let value = value.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name} requires a value"),
        )
    })?;

    value.parse().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name} must be a positive integer"),
        )
    })
}

async fn remove_socket_if_exists(socket: &str) -> io::Result<()> {
    match tokio::fs::remove_file(socket).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

async fn wait_for_socket(socket: &str) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        if Path::new(socket).exists() {
            return Ok(());
        }
        sleep(Duration::from_millis(1)).await;
    }

    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("socket did not become available: {socket}"),
    ))
}

fn percentile(sorted: &[Duration], percentile: f64) -> Duration {
    let index = ((sorted.len() as f64 - 1.0) * percentile).round() as usize;
    sorted[index]
}

fn format_duration(duration: Duration) -> String {
    if duration.as_micros() < 1_000 {
        format!("{}us", duration.as_micros())
    } else if duration.as_millis() < 1_000 {
        format!("{:.3}ms", duration.as_secs_f64() * 1_000.0)
    } else {
        format!("{:.3}s", duration.as_secs_f64())
    }
}

fn print_summary(config: &Config, umbral: BenchResult, actix: BenchResult) {
    println!("compare-actix");
    println!("connections: {}", config.connections);
    println!("concurrency: {}", config.concurrency);
    println!("requests: {}", config.requests);
    println!("payload_bytes: {}", config.payload_bytes);
    println!("actix_workers: {}", config.actix_workers);
    println!();
    print_result("umbral", umbral);
    println!();
    print_result("actix-uds", actix);
}

fn print_result(name: &str, mut result: BenchResult) {
    result.latencies.sort_unstable();
    let min = result.latencies[0];
    let p50 = percentile(&result.latencies, 0.50);
    let p95 = percentile(&result.latencies, 0.95);
    let p99 = percentile(&result.latencies, 0.99);
    let p999 = percentile(&result.latencies, 0.999);
    let max = result.latencies[result.latencies.len() - 1];
    let requests_per_sec = result.latencies.len() as f64 / result.total_time.as_secs_f64();

    println!("{name}:");
    println!("  min: {}", format_duration(min));
    println!("  p50: {}", format_duration(p50));
    println!("  p95: {}", format_duration(p95));
    println!("  p99: {}", format_duration(p99));
    println!("  p999: {}", format_duration(p999));
    println!("  max: {}", format_duration(max));
    println!("  requests_per_sec: {:.0}", requests_per_sec);
}
