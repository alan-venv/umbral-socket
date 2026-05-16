use std::io;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use bytes::Bytes;
use tokio::net::UnixStream;
use tokio::sync::{Mutex, MutexGuard};
use tokio::time::timeout;

use crate::stream::protocol::{
    MethodId, UmbralConfig, UmbralStatus, read_response_into_async, write_request_async,
};

pub struct UmbralClient {
    socket: Arc<str>,
    slots: Vec<ConnectionSlot>,
    next: AtomicUsize,
    config: UmbralConfig,
}

struct ConnectionSlot {
    connection: Mutex<Option<Connection>>,
}

struct Connection {
    stream: UnixStream,
    response_buffer: Vec<u8>,
}

fn timed_out(operation: &'static str) -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        format!("umbral {operation} timed out"),
    )
}

async fn connect_with_timeout(socket: &str, config: UmbralConfig) -> io::Result<UnixStream> {
    timeout(config.connect_timeout, UnixStream::connect(socket))
        .await
        .map_err(|_| timed_out("connect"))?
}

async fn connect_connection(socket: &str, config: UmbralConfig) -> io::Result<Connection> {
    let stream = connect_with_timeout(socket, config).await?;
    Ok(Connection {
        stream,
        response_buffer: Vec::new(),
    })
}

impl UmbralClient {
    pub async fn new(socket: &str, connections: usize) -> io::Result<Self> {
        Self::with_config(socket, connections, UmbralConfig::default()).await
    }

    pub async fn with_config(
        socket: &str,
        connections: usize,
        config: UmbralConfig,
    ) -> io::Result<Self> {
        if connections == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "connections must be greater than zero",
            ));
        }

        let socket: Arc<str> = Arc::from(socket);
        let mut slots = Vec::with_capacity(connections);

        for _ in 0..connections {
            slots.push(ConnectionSlot {
                connection: Mutex::new(Some(connect_connection(socket.as_ref(), config).await?)),
            });
        }

        Ok(Self {
            socket,
            slots,
            next: AtomicUsize::new(0),
            config,
        })
    }

    pub async fn send(&self, method: MethodId, payload: Bytes) -> io::Result<Bytes> {
        let (status, payload) = self.send_raw(method, payload).await?;
        if status == UmbralStatus::Ok {
            return Ok(payload);
        }
        Err(io::Error::new(
            io::ErrorKind::Other,
            format!("umbral request failed with status {status:?}"),
        ))
    }

    pub async fn send_with<T, F>(
        &self,
        method: MethodId,
        payload: &[u8],
        handle: F,
    ) -> io::Result<T>
    where
        F: FnOnce(&[u8]) -> io::Result<T>,
    {
        self.send_raw_with(method, payload, |status, body| {
            if status == UmbralStatus::Ok {
                return handle(body);
            }
            Err(io::Error::new(
                io::ErrorKind::Other,
                format!("umbral request failed with status {status:?}"),
            ))
        })
        .await
    }

    async fn acquire_slot(&self) -> MutexGuard<'_, Option<Connection>> {
        let start = self.next.fetch_add(1, Ordering::Relaxed) % self.slots.len();

        for offset in 0..self.slots.len() {
            let index = (start + offset) % self.slots.len();
            if let Ok(guard) = self.slots[index].connection.try_lock() {
                return guard;
            }
        }

        self.slots[start].connection.lock().await
    }

    pub async fn send_raw(
        &self,
        method: MethodId,
        payload: Bytes,
    ) -> io::Result<(UmbralStatus, Bytes)> {
        self.send_raw_with(method, payload.as_ref(), |status, body| {
            Ok((status, Bytes::copy_from_slice(body)))
        })
        .await
    }

    pub async fn send_raw_with<T, F>(
        &self,
        method: MethodId,
        payload: &[u8],
        handle: F,
    ) -> io::Result<T>
    where
        F: FnOnce(UmbralStatus, &[u8]) -> io::Result<T>,
    {
        if payload.len() > self.config.max_payload_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "payload too large",
            ));
        }

        let mut guard = self.acquire_slot().await;

        if guard.is_none() {
            *guard = Some(connect_connection(self.socket.as_ref(), self.config).await?);
        }

        let connection = guard.as_mut().expect("slot connection must exist");
        let result = async {
            timeout(
                self.config.write_timeout,
                write_request_async(&mut connection.stream, method, payload),
            )
            .await
            .map_err(|_| timed_out("write"))??;

            timeout(
                self.config.read_timeout,
                read_response_into_async(
                    &mut connection.stream,
                    self.config.max_payload_len,
                    &mut connection.response_buffer,
                ),
            )
            .await
            .map_err(|_| timed_out("read"))?
        }
        .await;

        match result {
            Ok(status) => {
                let connection = guard.as_ref().expect("slot connection must exist");
                handle(status, connection.response_buffer.as_slice())
            }
            Err(error) => {
                *guard = None;
                Err(error)
            }
        }
    }
}
