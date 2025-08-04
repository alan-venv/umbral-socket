use std::collections::HashMap;
use std::io::Result;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use bytes::Bytes;
use futures::future::BoxFuture;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixListener;
use tokio::net::UnixStream;

type Handler<S> = Arc<dyn Fn(Arc<S>, Bytes) -> BoxFuture<'static, Result<Bytes>> + Send + Sync>;

pub struct UmbralServer<S> {
    state: Arc<S>,
    handlers: HashMap<String, Handler<S>>,
}

impl<S: Send + Sync + 'static> UmbralServer<S> {
    pub fn new(state: S) -> Self {
        Self {
            state: Arc::new(state),
            handlers: HashMap::new(),
        }
    }

    pub fn route<F, Fut>(mut self, method: &str, handler: F) -> Self
    where
        F: Fn(Arc<S>, Bytes) -> Fut + Send + Sync + 'static,
        Fut: futures::Future<Output = Result<Bytes>> + Send + 'static,
    {
        let handler_arc: Handler<S> =
            Arc::new(move |state, payload| Box::pin(handler(state, payload)));
        self.handlers.insert(method.to_string(), handler_arc);
        self
    }

    pub async fn run(self, socket: &str) -> Result<()> {
        let path = Path::new(socket);
        if path.exists() {
            tokio::fs::remove_file(path).await?;
        }
        let listener = UnixListener::bind(path)?;
        let permissions = std::fs::Permissions::from_mode(0o766);
        std::fs::set_permissions(path, permissions)?;
        let server_arc = Arc::new(self);
        println!("Umbral Server listening on \"{}\"", socket);
        loop {
            let (stream, _) = listener.accept().await?;
            let server_clone = server_arc.clone();
            tokio::spawn(async move {
                if let Err(e) = server_clone.handle_connection(stream).await {
                    eprintln!("Error processing connection: {}", e);
                }
            });
        }
    }

    async fn handle_connection(&self, mut stream: UnixStream) -> Result<()> {
        let mut buffer = [0u8; 1024];
        loop {
            let n = match stream.read(&mut buffer).await {
                Ok(0) => return Ok(()),
                Ok(n) => n,
                Err(e) => return Err(e),
            };
            let message = String::from_utf8_lossy(&buffer[..n]);

            let response = if let Some((method, payload)) = message.trim().split_once("[%]") {
                if let Some(handler) = self.handlers.get(method) {
                    let state_clone = self.state.clone();
                    let payload_bytes = Bytes::from(payload.as_bytes().to_vec());
                    handler(state_clone, payload_bytes).await
                } else {
                    Ok(Bytes::from_static(b"METHOD NOT FOUND"))
                }
            } else {
                Ok(Bytes::from_static(b"INVALID PROTOCOL"))
            };

            match response {
                Ok(response_bytes) => {
                    let len = response_bytes.len() as u32;
                    stream.write_all(&len.to_be_bytes()).await?;
                    stream.write_all(&response_bytes).await?;
                }
                Err(e) => {
                    let err_msg = Bytes::from(format!("Handler error: {}", e));
                    let len = err_msg.len() as u32;
                    stream.write_all(&len.to_be_bytes()).await?;
                    stream.write_all(&err_msg).await?;
                }
            }
        }
    }
}
