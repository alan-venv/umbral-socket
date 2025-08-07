use bytes::Bytes;
use std::io::{Read, Result, Write};

pub struct UmbralClient {
    stream: Option<std::os::unix::net::UnixStream>,
    socket: String,
}

impl UmbralClient {
    pub fn new(socket: &str) -> Self {
        Self {
            stream: None,
            socket: socket.to_string(),
        }
    }

    pub fn send(&mut self, method: &str, payload: &Bytes) -> Result<Bytes> {
        match self.call(method, payload) {
            Ok(bytes) => Ok(bytes),
            Err(e) => {
                println!("Conexão falhou, tentando reconectar... ({})", e);
                self.stream = None;
                self.call(method, payload)
            }
        }
    }

    fn call(&mut self, method: &str, payload: &Bytes) -> Result<Bytes> {
        if self.stream.is_none() {
            let stream = std::os::unix::net::UnixStream::connect(&self.socket)?;
            stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
            self.stream = Some(stream);
        }

        if let Some(stream) = self.stream.as_mut() {
            let mut message = Vec::new();
            message.extend_from_slice(method.as_bytes());
            message.extend_from_slice(b"[%]");
            message.extend_from_slice(payload);

            stream.write_all(&message)?;

            let mut len_bytes = [0u8; 4];
            stream.read_exact(&mut len_bytes)?;
            let len = u32::from_be_bytes(len_bytes);

            let mut response_buffer = vec![0u8; len as usize];
            stream.read_exact(&mut response_buffer)?;

            return Ok(Bytes::from(response_buffer));
        }
        unreachable!();
    }
}
