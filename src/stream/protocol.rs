use std::convert::TryFrom;
use std::io;
#[cfg(test)]
use std::io::{Read, Write};
use std::time::Duration;

use bytes::Bytes;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub type MethodId = u8;

pub const REQUEST_HEADER_LEN: usize = 5;
pub const RESPONSE_HEADER_LEN: usize = 5;
pub const DEFAULT_MAX_PAYLOAD_LEN: usize = 64 * 1024;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UmbralStatus {
    Ok = 0,
    MethodNotFound = 1,
    PayloadTooLarge = 2,
    InvalidFrame = 3,
    HandlerError = 4,
}

impl TryFrom<u8> for UmbralStatus {
    type Error = io::Error;

    fn try_from(value: u8) -> io::Result<Self> {
        match value {
            0 => Ok(Self::Ok),
            1 => Ok(Self::MethodNotFound),
            2 => Ok(Self::PayloadTooLarge),
            3 => Ok(Self::InvalidFrame),
            4 => Ok(Self::HandlerError),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unknown umbral status",
            )),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct UmbralConfig {
    pub max_payload_len: usize,
    pub socket_permissions: u32,
    pub connect_timeout: Duration,
    pub write_timeout: Duration,
    pub read_timeout: Duration,
}

impl Default for UmbralConfig {
    fn default() -> Self {
        Self {
            max_payload_len: DEFAULT_MAX_PAYLOAD_LEN,
            socket_permissions: 0o766,
            connect_timeout: Duration::from_millis(500),
            write_timeout: Duration::from_millis(100),
            read_timeout: Duration::from_millis(500),
        }
    }
}

fn payload_too_large() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "payload too large")
}

fn payload_len_from_header(header: &[u8], max_payload_len: usize) -> io::Result<usize> {
    let len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
    if len > max_payload_len {
        return Err(payload_too_large());
    }
    Ok(len)
}

fn ensure_encodable_payload(payload: &[u8]) -> io::Result<()> {
    if payload.len() > u32::MAX as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "payload length exceeds u32",
        ));
    }
    Ok(())
}

pub async fn read_request_async<R>(
    reader: &mut R,
    max_payload_len: usize,
) -> io::Result<(MethodId, Bytes)>
where
    R: AsyncRead + Unpin,
{
    let mut payload = Vec::new();
    let method = read_request_into_async(reader, max_payload_len, &mut payload).await?;
    Ok((method, Bytes::from(payload)))
}

pub async fn read_request_into_async<R>(
    reader: &mut R,
    max_payload_len: usize,
    payload: &mut Vec<u8>,
) -> io::Result<MethodId>
where
    R: AsyncRead + Unpin,
{
    let mut header = [0u8; REQUEST_HEADER_LEN];
    reader.read_exact(&mut header).await?;
    let len = payload_len_from_header(&header, max_payload_len)?;
    payload.clear();
    payload.resize(len, 0);
    reader.read_exact(payload).await?;
    Ok(header[0])
}

pub async fn write_request_async<W>(
    writer: &mut W,
    method: MethodId,
    payload: &[u8],
) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    ensure_encodable_payload(payload)?;
    let len = payload.len() as u32;
    let mut header = [0u8; REQUEST_HEADER_LEN];
    header[0] = method;
    header[1..].copy_from_slice(&len.to_be_bytes());
    writer.write_all(&header).await?;
    writer.write_all(payload).await
}

pub async fn read_response_async<R>(
    reader: &mut R,
    max_payload_len: usize,
) -> io::Result<(UmbralStatus, Bytes)>
where
    R: AsyncRead + Unpin,
{
    let mut payload = Vec::new();
    let status = read_response_into_async(reader, max_payload_len, &mut payload).await?;
    Ok((status, Bytes::from(payload)))
}

pub async fn read_response_into_async<R>(
    reader: &mut R,
    max_payload_len: usize,
    payload: &mut Vec<u8>,
) -> io::Result<UmbralStatus>
where
    R: AsyncRead + Unpin,
{
    let mut header = [0u8; RESPONSE_HEADER_LEN];
    reader.read_exact(&mut header).await?;
    let status = UmbralStatus::try_from(header[0])?;
    let len = payload_len_from_header(&header, max_payload_len)?;
    payload.clear();
    payload.resize(len, 0);
    reader.read_exact(payload).await?;
    Ok(status)
}

pub async fn write_response_async<W>(
    writer: &mut W,
    status: UmbralStatus,
    payload: &[u8],
) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    ensure_encodable_payload(payload)?;
    let len = payload.len() as u32;
    let mut header = [0u8; RESPONSE_HEADER_LEN];
    header[0] = status as u8;
    header[1..].copy_from_slice(&len.to_be_bytes());
    writer.write_all(&header).await?;
    writer.write_all(payload).await
}

#[cfg(test)]
fn read_request_sync<R: Read>(
    reader: &mut R,
    max_payload_len: usize,
) -> io::Result<(MethodId, Bytes)> {
    let mut header = [0u8; REQUEST_HEADER_LEN];
    reader.read_exact(&mut header)?;
    let len = payload_len_from_header(&header, max_payload_len)?;
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload)?;
    Ok((header[0], Bytes::from(payload)))
}

#[cfg(test)]
fn write_request_sync<W: Write>(
    writer: &mut W,
    method: MethodId,
    payload: &[u8],
) -> io::Result<()> {
    ensure_encodable_payload(payload)?;
    let len = payload.len() as u32;
    let mut header = [0u8; REQUEST_HEADER_LEN];
    header[0] = method;
    header[1..].copy_from_slice(&len.to_be_bytes());
    writer.write_all(&header)?;
    writer.write_all(payload)
}

#[cfg(test)]
fn read_response_sync<R: Read>(
    reader: &mut R,
    max_payload_len: usize,
) -> io::Result<(UmbralStatus, Bytes)> {
    let mut header = [0u8; RESPONSE_HEADER_LEN];
    reader.read_exact(&mut header)?;
    let status = UmbralStatus::try_from(header[0])?;
    let len = payload_len_from_header(&header, max_payload_len)?;
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload)?;
    Ok((status, Bytes::from(payload)))
}

#[cfg(test)]
fn write_response_sync<W: Write>(
    writer: &mut W,
    status: UmbralStatus,
    payload: &[u8],
) -> io::Result<()> {
    ensure_encodable_payload(payload)?;
    let len = payload.len() as u32;
    let mut header = [0u8; RESPONSE_HEADER_LEN];
    header[0] = status as u8;
    header[1..].copy_from_slice(&len.to_be_bytes());
    writer.write_all(&header)?;
    writer.write_all(payload)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn encode_decode_empty_request() {
        let mut buffer = Vec::new();
        write_request_sync(&mut buffer, 7, b"").unwrap();

        let (method, payload) =
            read_request_sync(&mut Cursor::new(buffer), DEFAULT_MAX_PAYLOAD_LEN).unwrap();

        assert_eq!(method, 7);
        assert!(payload.is_empty());
    }

    #[test]
    fn encode_decode_small_request() {
        let mut buffer = Vec::new();
        write_request_sync(&mut buffer, 9, b"abc").unwrap();

        let (method, payload) =
            read_request_sync(&mut Cursor::new(buffer), DEFAULT_MAX_PAYLOAD_LEN).unwrap();

        assert_eq!(method, 9);
        assert_eq!(payload, Bytes::from_static(b"abc"));
    }

    #[test]
    fn encode_decode_ok_response() {
        let mut buffer = Vec::new();
        write_response_sync(&mut buffer, UmbralStatus::Ok, b"done").unwrap();

        let (status, payload) =
            read_response_sync(&mut Cursor::new(buffer), DEFAULT_MAX_PAYLOAD_LEN).unwrap();

        assert_eq!(status, UmbralStatus::Ok);
        assert_eq!(payload, Bytes::from_static(b"done"));
    }

    #[test]
    fn encode_decode_method_not_found_response() {
        let mut buffer = Vec::new();
        write_response_sync(&mut buffer, UmbralStatus::MethodNotFound, b"").unwrap();

        let (status, payload) =
            read_response_sync(&mut Cursor::new(buffer), DEFAULT_MAX_PAYLOAD_LEN).unwrap();

        assert_eq!(status, UmbralStatus::MethodNotFound);
        assert!(payload.is_empty());
    }

    #[test]
    fn unknown_status_returns_invalid_data() {
        let buffer = [99, 0, 0, 0, 0];

        let err = read_response_sync(&mut Cursor::new(buffer), DEFAULT_MAX_PAYLOAD_LEN)
            .expect_err("unknown status must fail");

        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn payload_above_max_returns_invalid_data() {
        let buffer = [1, 0, 0, 0, 2];

        let err = read_request_sync(&mut Cursor::new(buffer), 1)
            .expect_err("payload above max must fail");

        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn incomplete_header_returns_unexpected_eof() {
        let buffer = [1, 0, 0];

        let err = read_request_sync(&mut Cursor::new(buffer), DEFAULT_MAX_PAYLOAD_LEN)
            .expect_err("incomplete header must fail");

        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn incomplete_payload_returns_unexpected_eof() {
        let buffer = [1, 0, 0, 0, 3, b'a'];

        let err = read_request_sync(&mut Cursor::new(buffer), DEFAULT_MAX_PAYLOAD_LEN)
            .expect_err("incomplete payload must fail");

        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn concatenated_frames_are_read_sequentially() {
        let mut buffer = Vec::new();
        write_request_sync(&mut buffer, 1, b"one").unwrap();
        write_request_sync(&mut buffer, 2, b"two").unwrap();
        let mut cursor = Cursor::new(buffer);

        let first = read_request_sync(&mut cursor, DEFAULT_MAX_PAYLOAD_LEN).unwrap();
        let second = read_request_sync(&mut cursor, DEFAULT_MAX_PAYLOAD_LEN).unwrap();

        assert_eq!(first, (1, Bytes::from_static(b"one")));
        assert_eq!(second, (2, Bytes::from_static(b"two")));
    }

    #[tokio::test]
    async fn async_helpers_round_trip() {
        let (mut client, mut server) = tokio::io::duplex(64);

        let writer = tokio::spawn(async move {
            write_request_async(&mut client, 4, b"ping").await.unwrap();
            read_response_async(&mut client, DEFAULT_MAX_PAYLOAD_LEN)
                .await
                .unwrap()
        });

        let (method, payload) = read_request_async(&mut server, DEFAULT_MAX_PAYLOAD_LEN)
            .await
            .unwrap();
        write_response_async(&mut server, UmbralStatus::Ok, &payload)
            .await
            .unwrap();

        assert_eq!(method, 4);
        assert_eq!(
            writer.await.unwrap(),
            (UmbralStatus::Ok, Bytes::from_static(b"ping"))
        );
    }

    #[tokio::test]
    async fn read_request_into_async_reads_payload() {
        let mut buffer = Vec::new();
        write_request_sync(&mut buffer, 11, b"payload").unwrap();
        let mut cursor = Cursor::new(buffer);
        let mut payload = Vec::new();

        let method = read_request_into_async(&mut cursor, DEFAULT_MAX_PAYLOAD_LEN, &mut payload)
            .await
            .unwrap();

        assert_eq!(method, 11);
        assert_eq!(payload, b"payload");
    }

    #[tokio::test]
    async fn read_request_into_async_reuses_vec_capacity() {
        let mut buffer = Vec::new();
        write_request_sync(&mut buffer, 12, b"abc").unwrap();
        let mut cursor = Cursor::new(buffer);
        let mut payload = Vec::with_capacity(128);
        let capacity = payload.capacity();

        read_request_into_async(&mut cursor, DEFAULT_MAX_PAYLOAD_LEN, &mut payload)
            .await
            .unwrap();

        assert_eq!(payload, b"abc");
        assert_eq!(payload.capacity(), capacity);
    }

    #[tokio::test]
    async fn read_request_into_async_rejects_oversize_before_reading_payload() {
        let mut buffer = Vec::new();
        write_request_sync(&mut buffer, 13, b"12345").unwrap();
        let mut cursor = Cursor::new(buffer);
        let mut payload = Vec::new();

        let err = read_request_into_async(&mut cursor, 4, &mut payload)
            .await
            .expect_err("oversize payload must fail");

        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(payload.is_empty());
        assert_eq!(cursor.position(), REQUEST_HEADER_LEN as u64);
    }

    #[tokio::test]
    async fn read_response_into_async_reads_status_and_payload() {
        let mut buffer = Vec::new();
        write_response_sync(&mut buffer, UmbralStatus::Ok, b"done").unwrap();
        let mut cursor = Cursor::new(buffer);
        let mut payload = Vec::new();

        let status = read_response_into_async(&mut cursor, DEFAULT_MAX_PAYLOAD_LEN, &mut payload)
            .await
            .unwrap();

        assert_eq!(status, UmbralStatus::Ok);
        assert_eq!(payload, b"done");
    }

    #[tokio::test]
    async fn read_response_into_async_rejects_oversize() {
        let mut buffer = Vec::new();
        write_response_sync(&mut buffer, UmbralStatus::Ok, b"12345").unwrap();
        let mut cursor = Cursor::new(buffer);
        let mut payload = Vec::new();

        let err = read_response_into_async(&mut cursor, 4, &mut payload)
            .await
            .expect_err("oversize response must fail");

        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(payload.is_empty());
        assert_eq!(cursor.position(), RESPONSE_HEADER_LEN as u64);
    }
}
