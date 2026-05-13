use bytes::Bytes;
use deadpool::managed;
use std::io::{self, Result};
use tokio::net::UnixStream;

use crate::stream::protocol::{
    MethodId, UmbralConfig, UmbralStatus, read_response_async, write_request_async,
};

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
    config: UmbralConfig,
}

impl UmbralClient {
    pub fn new(socket: &str, pool_size: usize) -> UmbralClient {
        Self::with_config(socket, pool_size, UmbralConfig::default())
    }

    pub fn with_config(socket: &str, pool_size: usize, config: UmbralConfig) -> UmbralClient {
        let manager = UnixStreamManager {
            socket: socket.to_string(),
        };
        let pool = managed::Pool::builder(manager)
            .max_size(pool_size)
            .build()
            .unwrap();
        UmbralClient { pool, config }
    }

    pub async fn send(&self, method: MethodId, payload: Bytes) -> Result<Bytes> {
        let (status, payload) = self.send_raw(method, payload).await?;
        if status == UmbralStatus::Ok {
            return Ok(payload);
        }
        Err(io::Error::new(
            io::ErrorKind::Other,
            format!("umbral request failed with status {status:?}"),
        ))
    }

    pub async fn send_raw(
        &self,
        method: MethodId,
        payload: Bytes,
    ) -> Result<(UmbralStatus, Bytes)> {
        if payload.len() > self.config.max_payload_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "payload too large",
            ));
        }

        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

        let response = match write_request_async(&mut *conn, method, &payload).await {
            Ok(()) => read_response_async(&mut *conn, self.config.max_payload_len).await,
            Err(e) => Err(e),
        };

        if response.is_err() {
            drop(managed::Object::take(conn));
        }

        response
    }
}
