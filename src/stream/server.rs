use std::io::{self, Read, Result, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener as StdUnixListener;
use std::path::Path;
use std::slice;

use mio::net::{UnixListener, UnixStream};
use mio::{Events, Interest, Poll, Token};

use super::protocol::{
    MAX_PAYLOAD_LEN, MethodId, REQUEST_HEADER_LEN, SOCKET_PERMISSIONS, UmbralStatus,
};

const SERVER: Token = Token(0);
const CONNECTION_START: usize = 1;

type Handler<S> = fn(&S, &[u8]) -> io::Result<&'static [u8]>;

pub struct UmbralServer<S> {
    state: S,
    handlers: [Option<Handler<S>>; 256],
}

struct Connection {
    stream: UnixStream,
    state: ConnectionState,
    header: [u8; REQUEST_HEADER_LEN],
    header_offset: usize,
    request: Vec<u8>,
    request_len: usize,
    write_header: [u8; REQUEST_HEADER_LEN],
    write_header_offset: usize,
    write_payload: &'static [u8],
    write_payload_offset: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConnectionState {
    ReadingHeader,
    ReadingBody,
    Writing,
}

impl<S> UmbralServer<S> {
    pub fn new(state: S) -> Self {
        Self {
            state,
            handlers: std::array::from_fn(|_| None),
        }
    }

    pub fn route(mut self, method: MethodId, handler: Handler<S>) -> Self {
        self.handlers[method as usize] = Some(handler);
        self
    }

    pub fn run(self, socket: &str) -> Result<()> {
        let path = Path::new(socket);
        if path.exists() {
            std::fs::remove_file(path)?;
        }

        let std_listener = StdUnixListener::bind(path)?;
        std_listener.set_nonblocking(true)?;
        let permissions = std::fs::Permissions::from_mode(SOCKET_PERMISSIONS);
        std::fs::set_permissions(path, permissions)?;

        let mut listener = UnixListener::from_std(std_listener);
        let mut poll = Poll::new()?;
        poll.registry()
            .register(&mut listener, SERVER, Interest::READABLE)?;

        let mut events = Events::with_capacity(1024);
        let mut connections = Vec::new();
        let mut next_token = CONNECTION_START;

        loop {
            poll.poll(&mut events, None)?;

            for event in events.iter() {
                let token = event.token();
                if token == SERVER {
                    accept_connections(
                        poll.registry(),
                        &mut listener,
                        &mut connections,
                        &mut next_token,
                    )?;
                    continue;
                }

                if event.is_readable() {
                    self.read_connection(poll.registry(), token, &mut connections)?;
                }
                if event.is_writable() {
                    flush_connection(poll.registry(), token, &mut connections)?;
                }
            }
        }
    }

    fn read_connection(
        &self,
        registry: &mio::Registry,
        token: Token,
        connections: &mut Vec<Option<Connection>>,
    ) -> Result<()> {
        loop {
            let Some(connection) = connection_mut(connections, token) else {
                return Ok(());
            };

            if connection.state == ConnectionState::Writing {
                return update_interest(registry, token, connections);
            }

            match read_request(connection, MAX_PAYLOAD_LEN) {
                Ok(Some(method)) => {
                    self.handle_request(connection, method);
                    flush_connection(registry, token, connections)?;
                }
                Ok(None) => return update_interest(registry, token, connections),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof
                    ) =>
                {
                    remove_connection(registry, token, connections)?;
                    return Ok(());
                }
                Err(error) => return Err(error),
            };
        }
    }

    fn handle_request(&self, connection: &mut Connection, method: MethodId) {
        let Some(handler) = self.handlers[method as usize] else {
            prepare_status(connection, UmbralStatus::MethodNotFound);
            return;
        };

        match handler(&self.state, &connection.request) {
            Ok(payload) => {
                if payload.len() > MAX_PAYLOAD_LEN {
                    prepare_status(connection, UmbralStatus::HandlerError);
                    return;
                }

                prepare_response(connection, UmbralStatus::Ok, payload);
            }
            Err(_) => {
                prepare_status(connection, UmbralStatus::HandlerError);
            }
        }
    }
}

impl Connection {
    fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            state: ConnectionState::ReadingHeader,
            header: [0; REQUEST_HEADER_LEN],
            header_offset: 0,
            request: Vec::new(),
            request_len: 0,
            write_header: [0; REQUEST_HEADER_LEN],
            write_header_offset: 0,
            write_payload: b"",
            write_payload_offset: 0,
        }
    }

    fn reset_read(&mut self) {
        self.state = ConnectionState::ReadingHeader;
        self.header = [0; REQUEST_HEADER_LEN];
        self.header_offset = 0;
        self.request.clear();
        self.request_len = 0;
        self.write_header_offset = 0;
        self.write_payload = b"";
        self.write_payload_offset = 0;
    }
}

fn accept_connections(
    registry: &mio::Registry,
    listener: &mut UnixListener,
    connections: &mut Vec<Option<Connection>>,
    next_token: &mut usize,
) -> Result<()> {
    loop {
        let (mut stream, _) = match listener.accept() {
            Ok(accepted) => accepted,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) if error.raw_os_error() == Some(24) => return Ok(()),
            Err(error) => return Err(error),
        };

        let token = Token(*next_token);
        *next_token += 1;
        registry.register(&mut stream, token, Interest::READABLE)?;
        let index = connection_index(token);
        if connections.len() <= index {
            connections.resize_with(index + 1, || None);
        }
        connections[index] = Some(Connection::new(stream));
    }
}

fn read_request(connection: &mut Connection, max_payload_len: usize) -> Result<Option<MethodId>> {
    loop {
        match connection.state {
            ConnectionState::ReadingHeader => {
                while connection.header_offset < REQUEST_HEADER_LEN {
                    match connection
                        .stream
                        .read(&mut connection.header[connection.header_offset..])
                    {
                        Ok(0) => {
                            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "eof"));
                        }
                        Ok(read) => connection.header_offset += read,
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            return Ok(None);
                        }
                        Err(error) => return Err(error),
                    }
                }

                connection.request_len = u32::from_be_bytes([
                    connection.header[1],
                    connection.header[2],
                    connection.header[3],
                    connection.header[4],
                ]) as usize;

                if connection.request_len > max_payload_len {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "payload too large",
                    ));
                }

                connection.request.clear();
                connection.state = ConnectionState::ReadingBody;

                if connection.request_len == 0 {
                    return Ok(Some(connection.header[0]));
                }
            }
            ConnectionState::ReadingBody => {
                while connection.request.len() < connection.request_len {
                    match read_into_spare(
                        &mut connection.stream,
                        &mut connection.request,
                        connection.request_len,
                    ) {
                        Ok(0) => {
                            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "eof"));
                        }
                        Ok(_) => {}
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            return Ok(None);
                        }
                        Err(error) => return Err(error),
                    }
                }

                return Ok(Some(connection.header[0]));
            }
            ConnectionState::Writing => return Ok(None),
        }
    }
}

fn read_into_spare(
    stream: &mut UnixStream,
    payload: &mut Vec<u8>,
    target_len: usize,
) -> Result<usize> {
    let remaining = target_len - payload.len();
    payload.reserve(remaining);

    let before = payload.len();
    let buffer = unsafe { slice::from_raw_parts_mut(payload.as_mut_ptr().add(before), remaining) };
    let read = stream.read(buffer)?;

    unsafe {
        payload.set_len(before + read);
    }

    Ok(read)
}

fn prepare_status(connection: &mut Connection, status: UmbralStatus) {
    prepare_response(connection, status, b"");
}

fn prepare_response(connection: &mut Connection, status: UmbralStatus, payload: &'static [u8]) {
    let len = payload.len() as u32;
    connection.write_header[0] = status as u8;
    connection.write_header[1..].copy_from_slice(&len.to_be_bytes());
    connection.write_header_offset = 0;
    connection.write_payload = payload;
    connection.write_payload_offset = 0;
    connection.state = ConnectionState::Writing;
}

fn flush_connection(
    registry: &mio::Registry,
    token: Token,
    connections: &mut Vec<Option<Connection>>,
) -> Result<()> {
    let Some(connection) = connection_mut(connections, token) else {
        return Ok(());
    };

    if flush_write(connection)? {
        connection.reset_read();
    }

    update_interest(registry, token, connections)
}

fn flush_write(connection: &mut Connection) -> Result<bool> {
    while connection.write_header_offset < connection.write_header.len() {
        match connection
            .stream
            .write(&connection.write_header[connection.write_header_offset..])
        {
            Ok(0) => return write_zero(),
            Ok(written) => connection.write_header_offset += written,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) => return Err(error),
        }
    }

    while connection.write_payload_offset < connection.write_payload.len() {
        match connection
            .stream
            .write(&connection.write_payload[connection.write_payload_offset..])
        {
            Ok(0) => return write_zero(),
            Ok(written) => connection.write_payload_offset += written,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) => return Err(error),
        }
    }

    Ok(true)
}

fn write_zero<T>() -> Result<T> {
    Err(io::Error::new(
        io::ErrorKind::WriteZero,
        "failed to write umbral frame",
    ))
}

fn update_interest(
    registry: &mio::Registry,
    token: Token,
    connections: &mut Vec<Option<Connection>>,
) -> Result<()> {
    let Some(connection) = connection_mut(connections, token) else {
        return Ok(());
    };
    let interest = if connection.state == ConnectionState::Writing {
        Interest::WRITABLE
    } else {
        Interest::READABLE
    };
    registry.reregister(&mut connection.stream, token, interest)
}

fn remove_connection(
    registry: &mio::Registry,
    token: Token,
    connections: &mut Vec<Option<Connection>>,
) -> Result<()> {
    let index = connection_index(token);
    if let Some(mut connection) = connections.get_mut(index).and_then(Option::take) {
        registry.deregister(&mut connection.stream)?;
    }
    Ok(())
}

fn connection_index(token: Token) -> usize {
    token.0 - CONNECTION_START
}

fn connection_mut(connections: &mut [Option<Connection>], token: Token) -> Option<&mut Connection> {
    connections.get_mut(connection_index(token))?.as_mut()
}
