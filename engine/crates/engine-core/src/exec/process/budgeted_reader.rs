use std::io::Read;
use std::sync::mpsc;
use std::time::Duration;

use crate::error::{EngineError, Result};

/// Frames from a shim, read on a thread so a reply can be waited for *at most* so long
/// (TID-98, TID-93, TID-104). A worker blocked inside a C call cannot be interrupted from the
/// inside, and a child's stdout pipe takes no read timeout on any platform; a thread that
/// blocks in `read` and a channel the owner waits on with a budget is the one guarantee that
/// holds for pipes and sockets alike.
///
/// The thread ends at EOF, on a read error, or once the owner is dropped; a hung child holds it
/// in `read` until the child is killed, which the owner does once a reply is overdue.
pub struct BudgetedReader {
    frames: mpsc::Receiver<std::io::Result<Option<Vec<u8>>>>,
    lost: bool,
}

impl BudgetedReader {
    /// Start reading frames from `reader` on a thread named `name`.
    pub fn spawn<R: Read + Send + 'static>(mut reader: R, name: &str) -> Self {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || loop {
                let frame = read_raw_frame(&mut reader);
                let last = !matches!(frame, Ok(Some(_)));
                if tx.send(frame).is_err() || last {
                    break;
                }
            })
            .expect("spawn the frame reader");
        Self {
            frames: rx,
            lost: false,
        }
    }

    /// Whether a reply was overdue: the peer is to be killed, not waited for.
    pub fn is_lost(&self) -> bool {
        self.lost
    }

    /// The next frame's bytes: `Ok(None)` at EOF, an error when `wait` runs out — which also
    /// marks the reader lost — or when the read failed.
    pub fn next_frame(&mut self, wait: Option<Duration>) -> Result<Option<Vec<u8>>> {
        let received = match wait {
            Some(d) => self.frames.recv_timeout(d).map_err(|e| match e {
                mpsc::RecvTimeoutError::Timeout => Some(d),
                mpsc::RecvTimeoutError::Disconnected => None,
            }),
            None => self.frames.recv().map_err(|_| None),
        };
        match received {
            Ok(Ok(frame)) => Ok(frame),
            Err(None) => Ok(None),
            Ok(Err(e)) => Err(EngineError::Io(e)),
            Err(Some(budget)) => {
                self.lost = true;
                Err(EngineError::WorkerLost { budget })
            }
        }
    }

    /// [`next_frame`](Self::next_frame), parsed.
    pub fn next<T: serde::de::DeserializeOwned>(
        &mut self,
        wait: Option<Duration>,
    ) -> Result<Option<T>> {
        match self.next_frame(wait)? {
            Some(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            None => Ok(None),
        }
    }
}

/// One frame's payload bytes, `None` at EOF — the framing without the parse, for a thread that
/// cannot know the type its owner wants.
fn read_raw_frame<R: Read>(r: &mut R) -> std::io::Result<Option<Vec<u8>>> {
    let mut header = [0u8; 4];
    if let Err(e) = r.read_exact(&mut header) {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            return Ok(None);
        }
        return Err(e);
    }
    let len = u32::from_le_bytes(header) as usize;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(Some(buf))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::write_frame;
    use std::io::pipe;

    #[test]
    fn a_frame_that_arrives_in_time_is_returned_and_eof_is_none() {
        let (r, mut w) = pipe().unwrap();
        let mut frames = BudgetedReader::spawn(r, "t");
        write_frame(&mut w, &serde_json::json!({"n": 1})).unwrap();
        let got: serde_json::Value = frames
            .next(Some(Duration::from_secs(5)))
            .unwrap()
            .expect("a frame");
        assert_eq!(got["n"], 1);
        drop(w);
        assert!(frames
            .next_frame(Some(Duration::from_secs(5)))
            .unwrap()
            .is_none());
        assert!(!frames.is_lost());
    }

    #[test]
    fn a_reply_past_the_budget_is_an_error_and_marks_the_reader_lost() {
        let (r, _w) = pipe().unwrap(); // held open: nothing ever arrives
        let mut frames = BudgetedReader::spawn(r, "t");
        let err = frames
            .next_frame(Some(Duration::from_millis(50)))
            .expect_err("overdue");
        assert!(err.to_string().contains("TID-98"), "{err}");
        assert!(frames.is_lost());
    }
}
