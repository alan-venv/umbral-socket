use std::io::{self, Result};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use tokio::net::UnixListener;
use tokio::net::UnixStream;

use super::protocol::{
    MethodId, UmbralConfig, UmbralStatus, read_request_into_async, write_response_async,
};

pub enum UmbralResponse {
    Empty,
    RequestPayload,
    ResponseBuffer,
    Static(&'static [u8]),
}

type Handler<S> = Box<dyn Fn(&S, &[u8], &mut Vec<u8>) -> io::Result<UmbralResponse> + Send + Sync>;

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

    pub fn route<F>(mut self, method: MethodId, handler: F) -> Self
    where
        F: Fn(&S, &[u8], &mut Vec<u8>) -> io::Result<UmbralResponse> + Send + Sync + 'static,
    {
        self.handlers[method as usize] = Some(Box::new(handler));
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
        let mut request_buffer = Vec::new();
        let mut response_buffer = Vec::new();

        loop {
            response_buffer.clear();
            let method = match read_request_into_async(
                &mut stream,
                self.config.max_payload_len,
                &mut request_buffer,
            )
            .await
            {
                Ok(method) => method,
                Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                    write_response_async(&mut stream, UmbralStatus::PayloadTooLarge, b"").await?;
                    return Ok(());
                }
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
                Err(e) => return Err(e),
            };

            let Some(handler) = &self.handlers[method as usize] else {
                write_response_async(&mut stream, UmbralStatus::MethodNotFound, b"").await?;
                continue;
            };

            match handler(self.state.as_ref(), &request_buffer, &mut response_buffer) {
                Ok(response) => {
                    let body = match response {
                        UmbralResponse::Empty => b"".as_slice(),
                        UmbralResponse::RequestPayload => request_buffer.as_slice(),
                        UmbralResponse::ResponseBuffer => response_buffer.as_slice(),
                        UmbralResponse::Static(bytes) => bytes,
                    };

                    if body.len() > self.config.max_payload_len {
                        write_response_async(&mut stream, UmbralStatus::HandlerError, b"").await?;
                        continue;
                    }
                    write_response_async(&mut stream, UmbralStatus::Ok, body).await?;
                }
                Err(_) => {
                    write_response_async(&mut stream, UmbralStatus::HandlerError, b"").await?;
                }
            }
        }
    }
}
