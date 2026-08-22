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
}

impl JsonLineTransport {
    pub fn new(stream: TcpStream) -> Result<Self, ProtocolError> {
        stream.set_nodelay(true)?;
        let writer = BufWriter::new(stream.try_clone()?);
        Ok(Self {
            reader: BufReader::new(stream),
            writer,
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
        })
    }

    pub fn with_max_frame_bytes(mut self, max_frame_bytes: usize) -> Self {
        self.max_frame_bytes = max_frame_bytes.max(1);
        self
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
        serde_json::to_writer(&mut self.writer, message).map_err(ProtocolError::Encode)?;
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
        let mut frame = Vec::new();
        loop {
            let available = self.reader.fill_buf()?;
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
