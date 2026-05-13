# Umbral Socket

Bytes server and client over Unix sockets.

Umbral Socket uses a binary framed protocol over Unix stream sockets. Methods
are identified by `u8`, and request/response payloads are `Bytes`.

## Installation
```bash
cargo add umbral-socket
```

## How to Use

Below are basic examples for the server and client.

### Server
Example of how to start a server that receives data, prints it to the console, and pushes it in a Vec.

```rust
use std::{io::Result, sync::Arc};

use bytes::Bytes;
use tokio::sync::Mutex;
use umbral_socket::stream::UmbralServer;

#[derive(Clone, Default)]
struct State {
    contents: Arc<Mutex<Vec<Bytes>>>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let state = State::default();
    let socket = "/tmp/umbral.sock";
    UmbralServer::new(state)
        .route(1, handler)
        .run(socket)
        .await
}

async fn handler(state: Arc<State>, content: Bytes) -> Result<Bytes> {
    println!("CLIENT REQUEST: {}", String::from_utf8_lossy(&content));
    state.contents.lock().await.push(content);
    Ok(Bytes::from("OK"))
}
```

### Client
Example of how a client can send data to the server.

```rust
use bytes::Bytes;
use umbral_socket::stream::UmbralClient;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let socket = "/tmp/umbral.sock";
    let connections = 8;
    let client = UmbralClient::new(socket, connections).await?;

    let content = Bytes::from("{\"user\":\"alan\"}");
    let response = client.send(1, content).await?;

    println!("SERVER RESPONSE: {}", String::from_utf8_lossy(&response));

    Ok(())
}
```

Only the async `UmbralClient` is currently exposed. It manages persistent
connections internally for a single Unix socket. To talk to multiple servers,
create one `UmbralClient` per server. `UmbralConfig` allows configuring the
payload limit, socket permissions, and connect/write/read timeouts.
