//! Bounded NDJSON framing shared by the client and provider process pipes.

use std::io::{self, BufRead};

/// Upper bound on a single NDJSON frame, including its trailing newline.
pub const MAX_FRAME_BYTES: usize = 1 << 20;

/// Read one newline-terminated frame, accumulating partial reads in `pending`.
///
/// Returns `Ok(None)` on a clean end of stream. A timeout or `WouldBlock`
/// error from the reader propagates with `pending` intact so a later call
/// resumes the same frame. End of stream inside a frame fails with
/// [`io::ErrorKind::UnexpectedEof`] and a frame beyond [`MAX_FRAME_BYTES`]
/// fails with [`io::ErrorKind::InvalidData`]; both discard `pending`.
pub fn read_frame(reader: &mut impl BufRead, pending: &mut Vec<u8>) -> io::Result<Option<Vec<u8>>> {
    loop {
        let buffered = reader.fill_buf()?;
        if buffered.is_empty() {
            if pending.is_empty() {
                return Ok(None);
            }
            pending.clear();
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "incomplete NDJSON frame",
            ));
        }
        let newline = buffered.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(buffered.len(), |index| index + 1);
        if pending.len() + consumed > MAX_FRAME_BYTES {
            pending.clear();
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "NDJSON frame exceeds 1 MiB",
            ));
        }
        pending.extend_from_slice(&buffered[..consumed]);
        reader.consume(consumed);
        if newline.is_some() {
            return Ok(Some(std::mem::take(pending)));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn frames(input: &[u8]) -> (Vec<Vec<u8>>, io::Result<Option<Vec<u8>>>) {
        let mut reader = Cursor::new(input);
        let mut pending = Vec::new();
        let mut complete = Vec::new();
        loop {
            match read_frame(&mut reader, &mut pending) {
                Ok(Some(frame)) => complete.push(frame),
                last => return (complete, last),
            }
        }
    }

    #[test]
    fn splits_frames_on_newlines() {
        let (complete, last) = frames(b"{\"a\":1}\n{\"b\":2}\n");
        assert_eq!(complete, [b"{\"a\":1}\n".to_vec(), b"{\"b\":2}\n".to_vec()]);
        assert!(matches!(last, Ok(None)));
    }

    #[test]
    fn rejects_an_incomplete_frame_at_eof() {
        let (complete, last) = frames(b"{\"a\":1}\n{\"b\"");
        assert_eq!(complete, [b"{\"a\":1}\n".to_vec()]);
        assert_eq!(last.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn rejects_frames_beyond_the_size_bound() {
        let mut oversized = vec![b'x'; MAX_FRAME_BYTES];
        oversized.push(b'\n');
        let (complete, last) = frames(&oversized);
        assert!(complete.is_empty());
        assert_eq!(last.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn a_timeout_preserves_the_partial_frame() {
        struct Stutter(Vec<io::Result<&'static [u8]>>);

        impl io::Read for Stutter {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                match self.0.pop() {
                    Some(Ok(chunk)) => {
                        buffer[..chunk.len()].copy_from_slice(chunk);
                        Ok(chunk.len())
                    }
                    Some(Err(error)) => Err(error),
                    None => Ok(0),
                }
            }
        }

        let mut reader = io::BufReader::new(Stutter(vec![
            Ok(b":1}\n"),
            Err(io::ErrorKind::WouldBlock.into()),
            Ok(b"{\"a\""),
        ]));
        let mut pending = Vec::new();
        let error = read_frame(&mut reader, &mut pending).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(pending, b"{\"a\"");
        assert_eq!(
            read_frame(&mut reader, &mut pending).unwrap().as_deref(),
            Some(b"{\"a\":1}\n".as_slice())
        );
    }
}
