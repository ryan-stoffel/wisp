//! Newline-delimited JSON: one compact JSON-RPC message per line, in both directions.
//!
//! [`FrameCodec`] is a `tokio_util` codec. Use it with `FramedRead` and `FramedWrite`, then parse
//! each frame with [`Message::from_frame`](crate::jsonrpc::Message::from_frame):
//!
//! - A frame that is not UTF-8 JSON, or not JSON-RPC, is not a codec error. `from_frame` returns
//!   the error response to send, and the connection stays open.
//! - A frame longer than the limit is [`FrameError::TooLarge`], after which the codec yields
//!   nothing more. Close the connection.
//!
//! Frames end with `\n`. A `\r` before it is dropped, empty lines are skipped, and a last line
//! without a newline still counts at the end of the stream. The limit leaves out the line ending,
//! whether it is `\n` or `\r\n`.
//!
//! It splits lines like `tokio_util`'s `LinesCodec::new_with_max_length`, which 0007 names, but
//! yields bytes instead of strings. `LinesCodec` fails the stream on a line that is not UTF-8,
//! which would close the connection instead of answering that line with a parse error.

use std::error::Error;
use std::{fmt, io};

use bytes::{BufMut, Bytes, BytesMut};
use serde::Serialize;
use tokio_util::codec::{Decoder, Encoder};

/// The largest frame either side sends or accepts: 8 MiB, not counting the line ending.
///
/// Diffs, logs, and files that could be larger come through paged methods.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Splits a byte stream into frames and writes messages as frames.
#[derive(Clone, Debug)]
pub struct FrameCodec {
    max_frame_bytes: usize,
    next_index: usize,
    too_large: bool,
}

impl FrameCodec {
    /// A codec with the protocol's limit, [`MAX_FRAME_BYTES`].
    #[must_use]
    pub fn new() -> Self {
        Self::with_max_frame_bytes(MAX_FRAME_BYTES)
    }

    /// A codec with another limit, in bytes, not counting the line ending.
    #[must_use]
    pub fn with_max_frame_bytes(max_frame_bytes: usize) -> Self {
        Self {
            max_frame_bytes,
            next_index: 0,
            too_large: false,
        }
    }

    fn too_large(&mut self) -> FrameError {
        self.too_large = true;
        FrameError::TooLarge {
            max_frame_bytes: self.max_frame_bytes,
        }
    }
}

impl Default for FrameCodec {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder for FrameCodec {
    type Item = Bytes;
    type Error = FrameError;

    fn decode(&mut self, buf: &mut BytesMut) -> Result<Option<Bytes>, FrameError> {
        let max = self.max_frame_bytes;
        loop {
            if self.too_large {
                return Err(self.too_large());
            }
            // The limit leaves out the line ending, so the `\r` of a `\r\n` may sit just past it.
            let search_end = buf.len().min(max.saturating_add(2));
            let newline = buf[self.next_index..search_end]
                .iter()
                .position(|&byte| byte == b'\n');
            let Some(offset) = newline else {
                let past_limit =
                    buf.len() > max.saturating_add(1) || (buf.len() > max && buf[max] != b'\r');
                if past_limit {
                    return Err(self.too_large());
                }
                self.next_index = buf.len();
                return Ok(None);
            };
            let mut line = buf.split_to(self.next_index + offset + 1);
            self.next_index = 0;
            line.truncate(line.len() - 1);
            if line.last() == Some(&b'\r') {
                line.truncate(line.len() - 1);
            }
            if line.len() > max {
                return Err(self.too_large());
            }
            if !line.is_empty() {
                return Ok(Some(line.freeze()));
            }
        }
    }

    fn decode_eof(&mut self, buf: &mut BytesMut) -> Result<Option<Bytes>, FrameError> {
        if let Some(frame) = self.decode(buf)? {
            return Ok(Some(frame));
        }
        self.next_index = 0;
        let mut line = buf.split();
        if line.last() == Some(&b'\r') {
            line.truncate(line.len() - 1);
        }
        Ok((!line.is_empty()).then(|| line.freeze()))
    }
}

impl<T: Serialize> Encoder<T> for FrameCodec {
    type Error = FrameError;

    fn encode(&mut self, message: T, buf: &mut BytesMut) -> Result<(), FrameError> {
        let start = buf.len();
        // The limited writer fails the first write past the limit, which stops serde_json there,
        // so an oversized message is never written out in full. Its only I/O errors are that one.
        let frame = (&mut *buf).limit(self.max_frame_bytes).writer();
        if let Err(error) = serde_json::to_writer(frame, &message) {
            buf.truncate(start);
            return Err(if error.is_io() {
                FrameError::TooLarge {
                    max_frame_bytes: self.max_frame_bytes,
                }
            } else {
                FrameError::Serialize(error)
            });
        }
        buf.put_u8(b'\n');
        Ok(())
    }
}

/// Why a frame could not be read or written.
#[derive(Debug)]
pub enum FrameError {
    /// A frame is longer than the limit. When reading, the connection must close.
    TooLarge {
        /// The limit, in bytes, not counting the line ending.
        max_frame_bytes: usize,
    },
    /// A message could not be serialized.
    Serialize(serde_json::Error),
    /// Reading or writing the stream failed.
    Io(io::Error),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { max_frame_bytes } => {
                write!(f, "frame exceeds the limit of {max_frame_bytes} bytes")
            }
            Self::Serialize(error) => write!(f, "could not serialize a message: {error}"),
            Self::Io(error) => error.fmt(f),
        }
    }
}

impl Error for FrameError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::TooLarge { .. } => None,
            Self::Serialize(error) => Some(error),
            Self::Io(error) => Some(error),
        }
    }
}

impl From<io::Error> for FrameError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use bytes::{Bytes, BytesMut};
    use futures_util::{StreamExt, stream};
    use serde::ser::SerializeSeq;
    use serde::{Serialize, Serializer};
    use serde_json::json;
    use tokio_util::codec::{Decoder, Encoder, FramedRead};
    use tokio_util::io::StreamReader;

    use super::{FrameCodec, FrameError, MAX_FRAME_BYTES};
    use crate::jsonrpc::{INVALID_REQUEST, Message, PARSE_ERROR};

    fn decode_all(codec: &mut FrameCodec, buf: &mut BytesMut) -> Vec<Bytes> {
        let mut frames = Vec::new();
        while let Some(frame) = codec.decode(buf).unwrap() {
            frames.push(frame);
        }
        frames
    }

    #[test]
    fn a_frame_split_across_reads_waits_for_its_newline() {
        let mut codec = FrameCodec::new();
        let mut buf = BytesMut::new();
        for chunk in [&b"{\"jsonrpc\":"[..], b"\"2.0\",", b"\"method\":\"a\"}"] {
            buf.extend_from_slice(chunk);
            assert_eq!(codec.decode(&mut buf).unwrap(), None);
        }
        buf.extend_from_slice(b"\n{\"jso");
        assert_eq!(
            decode_all(&mut codec, &mut buf),
            [Bytes::from_static(
                b"{\"jsonrpc\":\"2.0\",\"method\":\"a\"}"
            )]
        );
        assert_eq!(&buf[..], b"{\"jso");
    }

    #[test]
    fn several_frames_in_one_read_come_out_in_order() {
        let mut codec = FrameCodec::new();
        let mut buf = BytesMut::from(&b"{\"a\":1}\n{\"b\":2}\r\n\n\r\n{\"c\":3}\n{\"d\""[..]);
        assert_eq!(
            decode_all(&mut codec, &mut buf),
            [
                Bytes::from_static(b"{\"a\":1}"),
                Bytes::from_static(b"{\"b\":2}"),
                Bytes::from_static(b"{\"c\":3}"),
            ]
        );
        assert_eq!(&buf[..], b"{\"d\"");
    }

    #[test]
    fn a_frame_over_the_limit_fails_before_its_newline_arrives() {
        let mut codec = FrameCodec::with_max_frame_bytes(8);
        let mut buf = BytesMut::from(&b"12345678"[..]);
        assert_eq!(codec.decode(&mut buf).unwrap(), None);
        buf.extend_from_slice(b"9");
        assert!(matches!(
            codec.decode(&mut buf),
            Err(FrameError::TooLarge { max_frame_bytes: 8 })
        ));
        buf.extend_from_slice(b"\n{}\n");
        assert!(
            matches!(codec.decode(&mut buf), Err(FrameError::TooLarge { .. })),
            "nothing is decoded after an oversized frame"
        );
    }

    #[test]
    fn the_limit_leaves_out_a_crlf_line_ending() {
        for (input, frame) in [
            (&b"12345678\r\n"[..], &b"12345678"[..]),
            (b"12345678\n", b"12345678"),
            (b"1234567\r\r\n", b"1234567\r"),
        ] {
            let mut codec = FrameCodec::with_max_frame_bytes(8);
            let mut buf = BytesMut::from(input);
            assert_eq!(codec.decode(&mut buf).unwrap().as_deref(), Some(frame));
        }
        for input in [&b"123456789\r\n"[..], b"123456789\n", b"12345678\r\r\n"] {
            let mut codec = FrameCodec::with_max_frame_bytes(8);
            let mut buf = BytesMut::from(input);
            assert!(
                matches!(codec.decode(&mut buf), Err(FrameError::TooLarge { .. })),
                "{input:?}"
            );
        }
    }

    #[test]
    fn a_crlf_split_across_reads_at_the_limit_passes() {
        let mut codec = FrameCodec::with_max_frame_bytes(8);
        let mut buf = BytesMut::from(&b"12345678\r"[..]);
        assert_eq!(codec.decode(&mut buf).unwrap(), None);
        buf.extend_from_slice(b"\n");
        assert_eq!(
            codec.decode(&mut buf).unwrap(),
            Some(Bytes::from_static(b"12345678"))
        );

        let mut buf = BytesMut::from(&b"12345678\r"[..]);
        assert_eq!(codec.decode(&mut buf).unwrap(), None);
        assert_eq!(
            codec.decode_eof(&mut buf).unwrap(),
            Some(Bytes::from_static(b"12345678"))
        );

        let mut buf = BytesMut::from(&b"12345678\r"[..]);
        assert_eq!(codec.decode(&mut buf).unwrap(), None);
        buf.extend_from_slice(b"x");
        assert!(matches!(
            codec.decode(&mut buf),
            Err(FrameError::TooLarge { .. })
        ));
    }

    #[test]
    fn an_oversized_frame_is_found_in_a_single_large_read() {
        let mut codec = FrameCodec::with_max_frame_bytes(8);
        let mut buf = BytesMut::from(&b"{}\n123456789\n{}\n"[..]);
        assert_eq!(
            codec.decode(&mut buf).unwrap(),
            Some(Bytes::from_static(b"{}"))
        );
        assert!(matches!(
            codec.decode(&mut buf),
            Err(FrameError::TooLarge { .. })
        ));
    }

    #[test]
    fn the_last_line_counts_at_end_of_stream() {
        let mut codec = FrameCodec::new();
        let mut buf = BytesMut::from(&b"{\"a\":1}\n{\"b\":2}\r"[..]);
        assert_eq!(
            codec.decode_eof(&mut buf).unwrap(),
            Some(Bytes::from_static(b"{\"a\":1}"))
        );
        assert_eq!(
            codec.decode_eof(&mut buf).unwrap(),
            Some(Bytes::from_static(b"{\"b\":2}"))
        );
        assert_eq!(codec.decode_eof(&mut buf).unwrap(), None);
    }

    #[test]
    fn encoding_writes_compact_json_and_one_newline() {
        let mut codec = FrameCodec::new();
        let mut buf = BytesMut::new();
        codec
            .encode(json!({"text": "two\nlines", "n": [1, 2]}), &mut buf)
            .unwrap();
        assert_eq!(&buf[..], b"{\"n\":[1,2],\"text\":\"two\\nlines\"}\n");
        assert!(!buf[..buf.len() - 1].contains(&b'\n'));
    }

    #[test]
    fn encoding_an_oversized_message_writes_nothing() {
        let mut codec = FrameCodec::with_max_frame_bytes(8);
        let mut buf = BytesMut::from(&b"{}\n"[..]);
        assert!(matches!(
            codec.encode("1234567", &mut buf),
            Err(FrameError::TooLarge { max_frame_bytes: 8 })
        ));
        assert_eq!(&buf[..], b"{}\n");
        codec.encode("123456", &mut buf).unwrap();
        assert_eq!(&buf[..], b"{}\n\"123456\"\n");
    }

    #[test]
    fn encoding_stops_as_soon_as_a_frame_passes_the_limit() {
        struct Endless<'a>(&'a Cell<usize>);

        impl Serialize for Endless<'_> {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let mut sequence = serializer.serialize_seq(None)?;
                for _ in 0..1_000_000 {
                    self.0.set(self.0.get() + 1);
                    sequence.serialize_element("x")?;
                }
                sequence.end()
            }
        }

        let serialized = Cell::new(0);
        let mut codec = FrameCodec::with_max_frame_bytes(64);
        let mut buf = BytesMut::new();
        assert!(matches!(
            codec.encode(Endless(&serialized), &mut buf),
            Err(FrameError::TooLarge {
                max_frame_bytes: 64
            })
        ));
        assert!(buf.is_empty());
        assert!(serialized.get() < 32, "{} elements", serialized.get());
    }

    #[test]
    fn the_default_limit_is_max_frame_bytes() {
        let mut codec = FrameCodec::default();
        let mut buf = BytesMut::from(&vec![b' '; MAX_FRAME_BYTES][..]);
        buf.extend_from_slice(b"\n");
        assert_eq!(
            codec.decode(&mut buf).unwrap().unwrap().len(),
            MAX_FRAME_BYTES
        );

        let mut buf = BytesMut::from(&vec![b' '; MAX_FRAME_BYTES + 1][..]);
        assert!(matches!(
            codec.decode(&mut buf),
            Err(FrameError::TooLarge {
                max_frame_bytes: MAX_FRAME_BYTES
            })
        ));
    }

    // Each chunk arrives as a separate read, and the limit is 64 bytes.
    fn reader(
        chunks: Vec<&'static [u8]>,
    ) -> FramedRead<impl tokio::io::AsyncRead + Unpin, FrameCodec> {
        let chunks = stream::iter(
            chunks
                .into_iter()
                .map(|chunk| Ok::<_, std::io::Error>(Bytes::from_static(chunk))),
        );
        FramedRead::new(
            StreamReader::new(chunks),
            FrameCodec::with_max_frame_bytes(64),
        )
    }

    #[tokio::test]
    async fn malformed_lines_get_errors_and_the_stream_goes_on() {
        let mut frames = reader(vec![
            &b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"host/health\"}\nnot json\n"[..],
            &b"{\"jsonrpc\":\"2.0\",\"id\":2,\"me"[..],
            &b"thod\":\"\xff\"}\n[1]\n"[..],
            &b"{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"host/version\"}"[..],
        ]);
        let mut outcomes = Vec::new();
        while let Some(frame) = frames.next().await {
            outcomes.push(match Message::from_frame(&frame.unwrap()) {
                Ok(Message::Request(request)) => Ok(request.method),
                Ok(other) => panic!("unexpected {other:?}"),
                Err(malformed) => Err(malformed.error.code),
            });
        }
        assert_eq!(
            outcomes,
            [
                Ok("host/health".to_owned()),
                Err(PARSE_ERROR),
                Err(PARSE_ERROR),
                Err(INVALID_REQUEST),
                Ok("host/version".to_owned()),
            ]
        );
    }

    #[tokio::test]
    async fn an_oversized_line_ends_the_stream() {
        let mut frames = reader(vec![
            &b"{\"jsonrpc\":\"2.0\",\"method\":\"a\"}\n"[..],
            &[b'x'; 40][..],
            &[b'x'; 40][..],
            &b"\n{\"jsonrpc\":\"2.0\",\"method\":\"b\"}\n"[..],
        ]);
        assert!(frames.next().await.unwrap().is_ok());
        assert!(matches!(
            frames.next().await,
            Some(Err(FrameError::TooLarge {
                max_frame_bytes: 64
            }))
        ));
        assert!(frames.next().await.is_none());
    }
}
