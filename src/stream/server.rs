use std::future::Future;
use std::io::Result;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use bytes::Bytes;
use futures::future::BoxFuture;
use tokio::net::UnixListener;
use tokio::net::UnixStream;

use super::protocol::{
    MethodId, UmbralConfig, UmbralStatus, read_request_async, write_response_async,
};

type Handler<S> = Arc<dyn Fn(Arc<S>, Bytes) -> BoxFuture<'static, Result<Bytes>> + Send + Sync>;

pub struct UmbralServer<S> {
    state: Arc<S>,
    handlers: [Option<Handler<S>>; 256],
    config: UmbralConfig,
}

impl<S: Send + Sync + 'static> UmbralServer<S> {
    pub fn new(state: S) -> Self {
        Self::with_config(state, UmbralConfig::default())
    }

    pub fn with_config(state: S, config: UmbralConfig) -> Self {
        Self {
            state: Arc::new(state),
            handlers: std::array::from_fn(|_| None),
            config,
        }
    }

    pub fn route<F, Fut>(mut self, method: MethodId, handler: F) -> Self
    where
        F: Fn(Arc<S>, Bytes) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Bytes>> + Send + 'static,
    {
        let handler_arc: Handler<S> =
            Arc::new(move |state, payload| Box::pin(handler(state, payload)));
        self.handlers[method as usize] = Some(handler_arc);
        self
    }

    pub async fn run(self, socket: &str) -> Result<()> {
        let path = Path::new(socket);
        if path.exists() {
            tokio::fs::remove_file(path).await?;
        }
        let listener = UnixListener::bind(path)?;
        let permissions = std::fs::Permissions::from_mode(self.config.socket_permissions);
        std::fs::set_permissions(path, permissions)?;
        let server_arc = Arc::new(self);
        println!("Umbral Server listening on \"{}\"", socket);
        loop {
            let (stream, _) = listener.accept().await?;
            let server_clone = server_arc.clone();
            tokio::spawn(async move {
                let _ = server_clone.handle_connection(stream).await;
            });
        }
    }

    async fn handle_connection(&self, mut stream: UnixStream) -> Result<()> {
        loop {
            let (method, payload) =
                match read_request_async(&mut stream, self.config.max_payload_len).await {
                    Ok(request) => request,
                    Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                        write_response_async(&mut stream, UmbralStatus::PayloadTooLarge, b"")
                            .await?;
                        return Ok(());
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
                    Err(e) => return Err(e),
                };

            let Some(handler) = &self.handlers[method as usize] else {
                write_response_async(&mut stream, UmbralStatus::MethodNotFound, b"").await?;
                continue;
            };

            let state_clone = self.state.clone();
            match handler(state_clone, payload).await {
                Ok(response_bytes) => {
                    if response_bytes.len() > self.config.max_payload_len {
                        write_response_async(&mut stream, UmbralStatus::HandlerError, b"").await?;
                        continue;
                    }
                    write_response_async(&mut stream, UmbralStatus::Ok, &response_bytes).await?;
                }
                Err(_) => {
                    write_response_async(&mut stream, UmbralStatus::HandlerError, b"").await?;
                }
            }
        }
    }
}
