# Umbral Socket

Bytes server and client over Unix sockets.

## Installation
```bash
cargo add umbral-socket
```

## How to Use

Below are basic examples for the server and client.

### Server
Example of how to start a server that receives data, prints it to the console, and queues it in a SegQueue.

```rust
#[derive(Clone, Default)]
struct State {
    queue: Arc<SegQueue<Bytes>>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let state = State::default();
    let socket = "/tmp/umbral.sock";

    UmbralServer::new(state)
        .route("POST", receive_user)
        .run(socket)
        .await
}

async fn receive_user(state: Arc<State>, content: Bytes) -> Result<Bytes> {
    println!("CLIENT REQUEST: {}", String::from_utf8_lossy(&content));
    state.queue.push(content);
    Ok(Bytes::from("OK"))
}
```

### Client
Example of how a client can send data to the server.

```rust
#[tokio::main]
async fn main() {
    let socket = "/tmp/umbral.sock";
    let pool_size = 1;
    let client = UmbralClient::new(socket, pool_size);

    let content = Bytes::from("{\"user\":\"alan\"}");
    if let Ok(response) = client.send("POST", content).await {
        println!("SERVER RESPONSE: {}", String::from_utf8_lossy(&response))
    }
}
```
