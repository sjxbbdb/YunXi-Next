//! Size-bounded newline-delimited JSON transport.

use std::error::Error;
use std::fmt;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;

pub const DEFAULT_MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

pub struct JsonLineTransport {
    reader: BufReader<TcpStream>,
    writer: BufWriter<TcpStream>,
    max_frame_bytes: usize,
    pending_frame: Vec<u8>,
}

impl JsonLineTransport {
    pub fn new(stream: TcpStream) -> Result<Self, ProtocolError> {
        stream.set_nodelay(true)?;
        let writer = BufWriter::new(stream.try_clone()?);
        Ok(Self {
            reader: BufReader::new(stream),
            writer,
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
            pending_frame: Vec::new(),
        })
    }

    pub fn with_max_frame_bytes(mut self, max_frame_bytes: usize) -> Self {
        self.max_frame_bytes = max_frame_bytes.max(1);
        self
    }

    /// Changes the frame bound after construction.
    ///
    /// The host uses this immediately after a plugin handshake so every
    /// subsequent request and response is subject to the launch policy. A
    /// partially received frame is never enlarged by this operation.
    pub fn set_max_frame_bytes(&mut self, max_frame_bytes: usize) {
        self.max_frame_bytes = max_frame_bytes.max(1);
        if self.pending_frame.len() > self.max_frame_bytes {
            self.pending_frame.clear();
        }
    }

    pub const fn max_frame_bytes(&self) -> usize {
        self.max_frame_bytes
    }

    pub fn peer_addr(&self) -> Result<SocketAddr, ProtocolError> {
        Ok(self.reader.get_ref().peer_addr()?)
    }

    pub fn set_timeouts(
        &self,
        read_timeout: Option<Duration>,
        write_timeout: Option<Duration>,
    ) -> Result<(), ProtocolError> {
        self.reader.get_ref().set_read_timeout(read_timeout)?;
        self.writer.get_ref().set_write_timeout(write_timeout)?;
        Ok(())
    }

    pub fn send<T>(&mut self, message: &T) -> Result<(), ProtocolError>
    where
        T: Serialize,
    {
        // Serialize before touching the socket. Apart from making the write
        // atomic at the JSON-line boundary, this bounds outbound memory and
        // prevents a plugin or host caller from bypassing the frame limit.
        let frame = serde_json::to_vec(message).map_err(ProtocolError::Encode)?;
        if frame.len().saturating_add(1) > self.max_frame_bytes {
            return Err(ProtocolError::FrameTooLarge {
                limit: self.max_frame_bytes,
            });
        }
        self.writer.write_all(&frame)?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        Ok(())
    }

    pub fn receive<T>(&mut self) -> Result<T, ProtocolError>
    where
        T: DeserializeOwned,
    {
        let frame = self.read_frame()?;
        serde_json::from_slice(&frame).map_err(ProtocolError::Decode)
    }

    pub fn close(&mut self) -> Result<(), ProtocolError> {
        self.writer.flush()?;
        self.writer.get_ref().shutdown(Shutdown::Both)?;
        Ok(())
    }

    fn read_frame(&mut self) -> Result<Vec<u8>, ProtocolError> {
        let mut frame = std::mem::take(&mut self.pending_frame);
        loop {
            let available = match self.reader.fill_buf() {
                Ok(available) => available,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) =>
                {
                    self.pending_frame = frame;
                    return Err(error.into());
                }
                Err(error) => return Err(error.into()),
            };
            if available.is_empty() {
                return Err(ProtocolError::ConnectionClosed);
            }
            let delimiter = available.iter().position(|byte| *byte == b'\n');
            let take = delimiter.map_or(available.len(), |index| index + 1);
            if frame.len().saturating_add(take) > self.max_frame_bytes {
                return Err(ProtocolError::FrameTooLarge {
                    limit: self.max_frame_bytes,
                });
            }
            frame.extend_from_slice(&available[..take]);
            self.reader.consume(take);
            if delimiter.is_some() {
                break;
            }
        }

        if frame.last() == Some(&b'\n') {
            frame.pop();
        }
        if frame.last() == Some(&b'\r') {
            frame.pop();
        }
        Ok(frame)
    }
}

#[derive(Debug)]
pub enum ProtocolError {
    Io(io::Error),
    Encode(serde_json::Error),
    Decode(serde_json::Error),
    ConnectionClosed,
    FrameTooLarge { limit: usize },
    Handshake(String),
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "protocol I/O failed: {error}"),
            Self::Encode(error) => write!(formatter, "failed to encode protocol message: {error}"),
            Self::Decode(error) => write!(formatter, "failed to decode protocol message: {error}"),
            Self::ConnectionClosed => formatter.write_str("plugin connection closed"),
            Self::FrameTooLarge { limit } => {
                write!(formatter, "protocol frame exceeded {limit} bytes")
            }
            Self::Handshake(message) => write!(formatter, "plugin handshake failed: {message}"),
        }
    }
}

impl Error for ProtocolError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Encode(error) | Self::Decode(error) => Some(error),
            Self::ConnectionClosed | Self::FrameTooLarge { .. } | Self::Handshake(_) => None,
        }
    }
}

impl From<io::Error> for ProtocolError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn fragmented_frame_survives_read_timeout_and_continues() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback listener");
        let address = listener.local_addr().expect("listener address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept client");
            stream.write_all(br#""frag"#).expect("write first fragment");
            stream.flush().expect("flush first fragment");
            thread::sleep(Duration::from_millis(120));
            stream
                .write_all(
                    br#"mented"
"#,
                )
                .expect("write second fragment");
            stream.flush().expect("flush second fragment");
        });

        let stream = TcpStream::connect(address).expect("connect loopback server");
        let mut transport = JsonLineTransport::new(stream).expect("create transport");
        transport
            .set_timeouts(Some(Duration::from_millis(30)), None)
            .expect("set read timeout");

        let first = transport.receive::<String>();
        assert!(matches!(
            first,
            Err(ProtocolError::Io(error))
                if matches!(error.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock)
        ));

        transport
            .set_timeouts(Some(Duration::from_secs(1)), None)
            .expect("extend timeout for the remaining fragment");
        let value: String = transport.receive().expect("resume fragmented frame");
        assert_eq!(value, "fragmented");
        server.join().expect("server thread");
    }

    #[test]
    fn outbound_frame_limit_is_checked_before_writing() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback listener");
        let address = listener.local_addr().expect("listener address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept client");
            stream
                .set_read_timeout(Some(Duration::from_millis(250)))
                .expect("set server read timeout");
            let mut bytes = [0_u8; 32];
            match stream.read(&mut bytes) {
                Ok(0) => {}
                Ok(length) => panic!("oversized frame wrote {length} bytes"),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) => {}
                Err(error) => panic!("unexpected server read error: {error}"),
            }
        });

        let stream = TcpStream::connect(address).expect("connect loopback server");
        let mut transport = JsonLineTransport::new(stream)
            .expect("create transport")
            .with_max_frame_bytes(8);
        let error = transport
            .send(&"oversized")
            .expect_err("oversized outbound frame must be rejected");
        assert!(matches!(error, ProtocolError::FrameTooLarge { limit: 8 }));
        server.join().expect("server thread");
    }
}
