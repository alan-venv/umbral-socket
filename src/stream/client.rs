use std::io;
use std::os::unix::net::UnixStream;

use crate::stream::protocol::{
    MAX_PAYLOAD_LEN, MethodId, UmbralStatus, read_response_sync, write_request_sync,
};

pub struct UmbralClient {
    stream: UnixStream,
}

impl UmbralClient {
    pub fn new(socket: &str) -> io::Result<Self> {
        let stream = UnixStream::connect(socket)?;
        Ok(Self { stream })
    }

    pub fn send(&mut self, method: MethodId, payload: &[u8]) -> io::Result<Vec<u8>> {
        if payload.len() > MAX_PAYLOAD_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "payload too large",
            ));
        }

        write_request_sync(&mut self.stream, method, payload)?;
        let (status, response) = read_response_sync(&mut self.stream, MAX_PAYLOAD_LEN)?;
        if status == UmbralStatus::Ok {
            return Ok(response);
        }
        Err(request_error(status))
    }
}

fn request_error(status: UmbralStatus) -> io::Error {
    io::Error::other(format!("umbral request failed with status {status:?}"))
}
