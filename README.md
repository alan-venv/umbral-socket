# Umbral Socket

Bytes server and client over Unix sockets.

Umbral Socket uses a binary framed protocol over Unix stream sockets. Methods
are identified by `u8`, and request/response payloads are byte slices on the
hot path.

## Installation
```bash
cargo add umbral-socket
```

## How to Use

Below are basic examples for the server and client.

### Server
Example of how to start a server that receives data and returns a fixed response.

```rust
use std::{
    io::Result,
    sync::{Arc, Mutex},
};

use umbral_socket::stream::{UmbralResponse, UmbralServer};

#[derive(Clone, Default)]
struct State {
    contents: Arc<Mutex<Vec<Vec<u8>>>>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let state = State::default();
    let socket = "/tmp/umbral.sock";
    UmbralServer::new(state)
        .route(1, |state, content, _| {
            println!("CLIENT REQUEST: {}", String::from_utf8_lossy(content));
            state.contents.lock().unwrap().push(content.to_vec());
            Ok(UmbralResponse::Static(b"OK"))
        })
        .run(socket)
        .await
}
```

### Client
Example of how a client can send data using the low-allocation callback API.

```rust
use umbral_socket::stream::UmbralClient;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let socket = "/tmp/umbral.sock";
    let connections = 8;
    let client = UmbralClient::new(socket, connections).await?;

    let content = b"{\"user\":\"alan\"}";
    client
        .send_with(1, content, |response| {
            println!("SERVER RESPONSE: {}", String::from_utf8_lossy(response));
            Ok(())
        })
        .await?;

    Ok(())
}
```

`send_with` and `send_raw_with` reuse one response buffer per connection slot.
`send` and `send_raw` remain available as convenience APIs that return owned
`Bytes`.

## Benchmark

```bash
cargo run --release --bin umbral-bench -- \
  --connections 4 \
  --concurrency 64 \
  --requests 100000 \
  --payload-bytes 32
```

The benchmark uses `send_with`, so it measures the callback hot path without an
owned response allocation.

## Actix Comparison

```bash
cargo run --release --example compare-actix -- \
  --connections 5 \
  --concurrency 96 \
  --requests 100000 \
  --payload-bytes 32 \
  --actix-workers 8
```

This compares Umbral over Unix sockets with Actix Web over Unix sockets using a manual keep-alive HTTP client. `--actix-workers` defaults to available CPU parallelism.
