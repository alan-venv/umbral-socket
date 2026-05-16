# Umbral Socket

Lightweight IPC client and server over Unix sockets.

Umbral Socket uses a small binary framed protocol over Unix stream sockets.
Methods are identified by `u8`, and payloads are raw byte slices.

## Installation

```bash
cargo add umbral-socket
```

## Client

```rust
use std::io;

use umbral_socket::stream::UmbralClient;

fn main() -> io::Result<()> {
    let mut client = UmbralClient::new("/tmp/umbral.sock")?;
    let response = client.send(1, b"{\"user\":\"alan\"}")?;

    println!("response: {}", String::from_utf8_lossy(&response));
    Ok(())
}
```

## Server

Handlers receive shared state and the request payload. They return a static byte
slice response.

```rust
use std::io;

use umbral_socket::stream::UmbralServer;

struct State;

fn main() -> io::Result<()> {
    UmbralServer::new(State)
        .route(1, |_, payload| {
            println!("request: {}", String::from_utf8_lossy(payload));
            Ok(b"OK")
        })
        .run("/tmp/umbral.sock")
}
```

## Protocol

`UmbralClient` is synchronous and owns one Unix socket connection. For high
performance dispatchers, implement the event loop directly over the protocol.

Request frame:

```text
method: u8
payload_len: u32 big-endian
payload: [u8]
```

Response frame:

```text
status: u8
payload_len: u32 big-endian
payload: [u8]
```
