use bytes::Bytes;
use deadpool::managed;
use std::io::{self, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

struct UnixStreamManager {
    socket: String,
}

impl managed::Manager for UnixStreamManager {
    type Type = UnixStream;
    type Error = io::Error;

    async fn create(&self) -> Result<Self::Type> {
        UnixStream::connect(&self.socket).await
    }

    async fn recycle(
        &self,
        conn: &mut Self::Type,
        _metrics: &managed::Metrics,
    ) -> managed::RecycleResult<Self::Error> {
        match conn.try_write(&[]) {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

#[derive(Clone)]
pub struct UmbralClient {
    pool: managed::Pool<UnixStreamManager>,
}

impl UmbralClient {
    pub fn new(socket: &str, pool_size: usize) -> UmbralClient {
        let manager = UnixStreamManager {
            socket: socket.to_string(),
        };
        let pool = managed::Pool::builder(manager)
            .max_size(pool_size)
            .build()
            .unwrap();
        UmbralClient { pool }
    }

    pub async fn send(&self, method: &str, payload: Bytes) -> Result<Bytes> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

        let mut message = Vec::new();
        message.extend_from_slice(method.as_bytes());
        message.extend_from_slice(b"[%]");
        message.extend_from_slice(&payload);

        conn.write_all(&message).await?;

        let mut len_bytes = [0u8; 4];
        conn.read_exact(&mut len_bytes).await?;
        let len = u32::from_be_bytes(len_bytes);

        let mut response_buffer = vec![0u8; len as usize];
        conn.read_exact(&mut response_buffer).await?;

        Ok(Bytes::from(response_buffer))
    }
}
