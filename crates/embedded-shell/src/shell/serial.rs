// Some primitives are not yet used (e.g. `console_buffer` via LinuxSerialShell
// re-exports happens later; `open` is exercised only against real ports).
#![allow(dead_code)]

//! Serial transport — the byte-level foundation shared by the Linux and
//! U-Boot serial shells.
//!
//! Owns the port handle, runs a background task that drains the port into an
//! mpsc + a console buffer, and provides primitives for writing, reading
//! chunks, reading-until-pattern, draining, and dumping the captured console
//! buffer. Knows nothing about shells, prompts, or framing — those live one
//! layer up.

use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadHalf};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::trace;

use super::error::ShellError;

const READ_CHUNK_SIZE: usize = 4096;
const MPSC_CAPACITY: usize = 64;
/// Default cap for the persistent console transcript. The reader task FIFO-trims
/// older bytes once the buffer grows past this. 1 MiB covers verbose boot logs
/// (~50 KB) and thousands of commands' worth of wire chatter with two orders of
/// magnitude headroom.
pub(crate) const DEFAULT_CONSOLE_BUFFER_CAP: usize = 1024 * 1024;

pub(crate) struct SerialTransport {
    writer: Box<dyn AsyncWrite + Send + Unpin>,
    rx: mpsc::Receiver<Vec<u8>>,
    pending: Vec<u8>,
    buffer: Arc<Mutex<Vec<u8>>>,
    buffer_cap: Arc<AtomicUsize>,
    /// Wall-clock time the reader task last placed a chunk into the channel.
    /// Initialised to the transport's creation time so `idle_for()` returns a
    /// meaningful value even before any bytes have arrived.
    last_rx_at: Arc<Mutex<Instant>>,
    reader_task: Option<JoinHandle<()>>,
}

impl SerialTransport {
    /// Build a transport from any tokio-compatible duplex byte stream. The
    /// reader half is moved into a background task; the writer half stays
    /// on the struct.
    pub(crate) fn new<T>(io: T) -> Self
    where
        T: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let (reader, writer) = tokio::io::split(io);
        let (tx, rx) = mpsc::channel(MPSC_CAPACITY);
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let buffer_cap = Arc::new(AtomicUsize::new(DEFAULT_CONSOLE_BUFFER_CAP));
        let last_rx_at = Arc::new(Mutex::new(Instant::now()));
        let task = tokio::spawn(reader_loop(
            reader,
            tx,
            Arc::clone(&buffer),
            Arc::clone(&buffer_cap),
            Arc::clone(&last_rx_at),
        ));
        Self {
            writer: Box::new(writer),
            rx,
            pending: Vec::new(),
            buffer,
            buffer_cap,
            last_rx_at,
            reader_task: Some(task),
        }
    }

    /// Time since the reader task last received a non-empty chunk from the
    /// port. Right after activate this will be very small (microseconds);
    /// during periods of device silence it grows.
    pub(crate) fn idle_for(&self) -> Duration {
        let last = *self.last_rx_at.lock().unwrap();
        Instant::now().saturating_duration_since(last)
    }

    /// Update the console-transcript cap. The change is picked up by the
    /// reader task on its next chunk.
    pub(crate) fn set_console_buffer_cap(&self, cap: usize) {
        self.buffer_cap.store(cap, Ordering::Relaxed);
    }

    /// Open `port` at `baudrate` and wrap it.
    pub(crate) async fn open(port: &str, baudrate: u32) -> Result<Self, ShellError> {
        use tokio_serial::SerialPortBuilderExt;
        let stream = tokio_serial::new(port, baudrate)
            .open_native_async()
            .map_err(|e| ShellError::initialization(format!("opening {port}: {e}")))?;
        Ok(Self::new(stream))
    }

    pub(crate) async fn write_bytes(&mut self, buf: &[u8]) -> Result<(), ShellError> {
        trace!(bytes = ?buf, "tx");
        self.writer.write_all(buf).await?;
        self.writer.flush().await?;
        Ok(())
    }

    /// Returns the next chunk of bytes from the port (or any buffered bytes
    /// left over from an earlier `read_until` call). Errors with
    /// [`ShellError::ReadTimeout`] if nothing arrives within `timeout`.
    pub(crate) async fn read_chunk(&mut self, timeout: Duration) -> Result<Vec<u8>, ShellError> {
        if !self.pending.is_empty() {
            return Ok(std::mem::take(&mut self.pending));
        }
        match tokio::time::timeout(timeout, self.rx.recv()).await {
            Ok(Some(chunk)) => Ok(chunk),
            Ok(None) => Err(io::Error::from(io::ErrorKind::UnexpectedEof).into()),
            Err(_) => Err(ShellError::ReadTimeout {
                duration: timeout,
                captured: Vec::new(),
            }),
        }
    }

    /// Read bytes until `predicate` returns `Some(end)`. Returns the bytes
    /// `[..end]`; anything past `end` stays in the internal buffer for the
    /// next call. Errors with [`ShellError::ReadTimeout`] on deadline expiry.
    pub(crate) async fn read_until<F>(
        &mut self,
        predicate: F,
        timeout: Duration,
    ) -> Result<Vec<u8>, ShellError>
    where
        F: Fn(&[u8]) -> Option<usize>,
    {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(end) = predicate(&self.pending) {
                let result: Vec<u8> = self.pending.drain(..end).collect();
                return Ok(result);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(ShellError::ReadTimeout {
                    duration: timeout,
                    captured: std::mem::take(&mut self.pending),
                });
            }
            match tokio::time::timeout(remaining, self.rx.recv()).await {
                Ok(Some(chunk)) => self.pending.extend_from_slice(&chunk),
                Ok(None) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into()),
                Err(_) => {
                    return Err(ShellError::ReadTimeout {
                        duration: timeout,
                        captured: std::mem::take(&mut self.pending),
                    });
                }
            }
        }
    }

    /// Best-effort drain: collect whatever bytes are available within `grace`,
    /// returning them. Used to flush leftover output between commands.
    pub(crate) async fn drain(&mut self, grace: Duration) -> Vec<u8> {
        let deadline = Instant::now() + grace;
        let mut collected = std::mem::take(&mut self.pending);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, self.rx.recv()).await {
                Ok(Some(chunk)) => collected.extend_from_slice(&chunk),
                Ok(None) | Err(_) => break,
            }
        }
        collected
    }

    /// Drain bytes until the wire has been silent for `silence_threshold`,
    /// or `max_total` elapses — whichever comes first. Returns all bytes
    /// consumed. Used to wait out an indeterminate-length burst of output
    /// (e.g. a reboot transition between the dying shell and the bootloader).
    ///
    /// Distinct from [`drain`]: that one drains for a fixed wall-clock window
    /// regardless of incoming traffic. This one exits as soon as the wire
    /// settles down, which is what you want for "wait until the device stops
    /// talking".
    pub(crate) async fn drain_until_silent(
        &mut self,
        silence_threshold: Duration,
        max_total: Duration,
    ) -> Vec<u8> {
        let start = Instant::now();
        let mut collected = std::mem::take(&mut self.pending);
        while start.elapsed() < max_total {
            let idle = self.idle_for();
            if idle >= silence_threshold {
                // Wire has been quiet long enough — declare silence.
                break;
            }
            // Wait at most "remaining-to-reach-threshold" for the next chunk.
            // If a chunk arrives, accumulate and loop. If the timeout fires,
            // that means we hit the silence threshold with nothing happening.
            let wait_for = silence_threshold - idle;
            match tokio::time::timeout(wait_for, self.rx.recv()).await {
                Ok(Some(chunk)) => collected.extend_from_slice(&chunk),
                Ok(None) => break, // EOF
                Err(_) => break,   // silence achieved
            }
        }
        collected
    }

    /// ANSI-stripped, US-sentinel-stripped UTF-8-with-replacement view of
    /// everything that has come in over the wire so far.
    pub(crate) fn console_buffer(&self) -> String {
        let raw = self.buffer.lock().unwrap().clone();
        decode_console(&raw)
    }

    /// Fully close the port: abort the reader task, wait for it to finish,
    /// and drop the writer half so the underlying file descriptor is released.
    /// Idempotent.
    ///
    /// Async because we wait for the reader task to actually finish — without
    /// that, on a real serial port the OS won't have released the device file
    /// yet and an immediate re-open of the same port returns `EBUSY`.
    pub(crate) async fn close(&mut self) {
        if let Some(task) = self.reader_task.take() {
            task.abort();
            // `abort()` schedules cancellation; awaiting the handle blocks
            // until the task actually unwinds (including dropping the
            // ReadHalf, which releases its end of the split).
            let _ = task.await;
        }
        // Replace the writer with a sink so the original WriteHalf is dropped.
        // With both halves dropped, the underlying T (SerialStream / etc.)
        // is dropped and the OS file descriptor is released.
        self.writer = Box::new(tokio::io::sink());
    }
}

impl Drop for SerialTransport {
    fn drop(&mut self) {
        // Best-effort cleanup from synchronous contexts. Callers that need a
        // fully-closed port (e.g. before re-opening) should call `close().await`
        // explicitly.
        if let Some(task) = self.reader_task.take() {
            task.abort();
        }
    }
}

async fn reader_loop<T>(
    mut reader: ReadHalf<T>,
    tx: mpsc::Sender<Vec<u8>>,
    buffer: Arc<Mutex<Vec<u8>>>,
    buffer_cap: Arc<AtomicUsize>,
    last_rx_at: Arc<Mutex<Instant>>,
) where
    T: AsyncRead + Send + 'static,
{
    let mut buf = vec![0u8; READ_CHUNK_SIZE];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => {
                trace!("reader saw EOF");
                break;
            }
            Ok(n) => {
                let chunk = buf[..n].to_vec();
                trace!(bytes = ?chunk, "rx");
                if let Ok(mut guard) = buffer.lock() {
                    guard.extend_from_slice(&chunk);
                    let cap = buffer_cap.load(Ordering::Relaxed);
                    if guard.len() > cap {
                        let drop = guard.len() - cap;
                        guard.drain(..drop);
                    }
                }
                if let Ok(mut guard) = last_rx_at.lock() {
                    *guard = Instant::now();
                }
                if tx.send(chunk).await.is_err() {
                    trace!("reader saw mpsc closed");
                    break;
                }
            }
            Err(e) => {
                trace!(error = %e, "reader error");
                break;
            }
        }
    }
}

fn ansi_re() -> &'static regex::bytes::Regex {
    static RE: OnceLock<regex::bytes::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::bytes::Regex::new(r"\x1B(?:[@-Z\\-_]|\[[0-?]*[ -/]*[@-~])").unwrap())
}

fn decode_console(buf: &[u8]) -> String {
    let stripped = ansi_re().replace_all(buf, &b""[..]);
    let filtered: Vec<u8> = stripped.iter().copied().filter(|&b| b != 0x1f).collect();
    String::from_utf8_lossy(&filtered).into_owned()
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncWriteExt;

    use super::*;
    use crate::shell::prompts;

    fn pair() -> (SerialTransport, tokio::io::DuplexStream) {
        let (host, device) = tokio::io::duplex(8192);
        (SerialTransport::new(host), device)
    }

    #[tokio::test]
    async fn write_bytes_round_trips() {
        let (mut t, mut device) = pair();
        t.write_bytes(b"hello").await.unwrap();
        let mut buf = [0u8; 5];
        tokio::io::AsyncReadExt::read_exact(&mut device, &mut buf)
            .await
            .unwrap();
        assert_eq!(&buf, b"hello");
    }

    #[tokio::test]
    async fn read_chunk_receives_from_port() {
        let (mut t, mut device) = pair();
        device.write_all(b"data").await.unwrap();
        let chunk = t.read_chunk(Duration::from_secs(1)).await.unwrap();
        assert_eq!(chunk, b"data");
    }

    #[tokio::test]
    async fn read_chunk_times_out() {
        let (mut t, _device) = pair();
        let err = t.read_chunk(Duration::from_millis(50)).await.unwrap_err();
        assert!(matches!(err, ShellError::ReadTimeout { .. }));
    }

    #[tokio::test]
    async fn read_until_matches_pattern_and_keeps_extra() {
        let (mut t, mut device) = pair();
        device.write_all(b"device login: extra").await.unwrap();
        let result = t
            .read_until(prompts::find_linux_login, Duration::from_secs(1))
            .await
            .unwrap();
        assert!(result.ends_with(b"login: "));
        // The 5 trailing bytes ("extra") should be available on the next read.
        let leftover = t.read_chunk(Duration::from_secs(1)).await.unwrap();
        assert_eq!(leftover, b"extra");
    }

    #[tokio::test]
    async fn read_until_accumulates_across_chunks() {
        let (mut t, mut device) = pair();
        device.write_all(b"device lo").await.unwrap();
        device.flush().await.unwrap();
        device.write_all(b"gin: ").await.unwrap();
        let result = t
            .read_until(prompts::find_linux_login, Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(result, b"device login: ");
    }

    #[tokio::test]
    async fn read_until_times_out_with_captured_bytes() {
        let (mut t, mut device) = pair();
        device
            .write_all(b"some garbage but no prompt")
            .await
            .unwrap();
        let err = t
            .read_until(prompts::find_linux_login, Duration::from_millis(100))
            .await
            .unwrap_err();
        match err {
            ShellError::ReadTimeout { captured, .. } => {
                assert_eq!(captured, b"some garbage but no prompt");
            }
            other => panic!("expected ReadTimeout, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn drain_collects_available_bytes() {
        let (mut t, mut device) = pair();
        device.write_all(b"a").await.unwrap();
        device.write_all(b"b").await.unwrap();
        device.write_all(b"c").await.unwrap();
        // Yield once so the reader task picks them up.
        tokio::task::yield_now().await;
        let drained = t.drain(Duration::from_millis(100)).await;
        assert!(drained.iter().filter(|&&b| b == b'a').count() == 1);
        assert!(drained.iter().filter(|&&b| b == b'b').count() == 1);
        assert!(drained.iter().filter(|&&b| b == b'c').count() == 1);
    }

    #[tokio::test]
    async fn console_buffer_accumulates() {
        let (mut t, mut device) = pair();
        device.write_all(b"hello ").await.unwrap();
        device.write_all(b"world").await.unwrap();
        // Pulling chunks makes the reader task definitely process them.
        let _ = t.read_chunk(Duration::from_secs(1)).await.unwrap();
        let _ = t.read_chunk(Duration::from_millis(100)).await;
        let dump = t.console_buffer();
        assert!(dump.contains("hello"));
        assert!(dump.contains("world"));
    }

    #[tokio::test]
    async fn console_buffer_strips_ansi_escapes() {
        let (mut t, mut device) = pair();
        device
            .write_all(b"\x1B[33mYELLOW\x1B[0m text")
            .await
            .unwrap();
        let _ = t.read_chunk(Duration::from_secs(1)).await.unwrap();
        let dump = t.console_buffer();
        assert!(!dump.contains('\x1B'));
        assert!(dump.contains("YELLOW text"));
    }

    #[tokio::test]
    async fn console_buffer_strips_unit_separator() {
        let (mut t, mut device) = pair();
        device.write_all(b"a\x1fb\x1fc").await.unwrap();
        let _ = t.read_chunk(Duration::from_secs(1)).await.unwrap();
        let dump = t.console_buffer();
        assert_eq!(dump, "abc");
    }

    #[tokio::test]
    async fn drop_aborts_reader_task() {
        let (host, _device) = tokio::io::duplex(8192);
        let t = SerialTransport::new(host);
        let task_handle = t.reader_task.as_ref().unwrap().abort_handle();
        drop(t);
        // After drop, the task must be aborted (or already finished).
        // Give the runtime a tick to observe.
        tokio::task::yield_now().await;
        assert!(task_handle.is_finished());
    }

    #[tokio::test]
    async fn eof_on_port_returns_io_error() {
        let (mut t, device) = pair();
        drop(device); // device closes → host sees EOF
        let err = t.read_chunk(Duration::from_secs(1)).await.unwrap_err();
        assert!(matches!(err, ShellError::Io(_)));
    }

    #[tokio::test]
    async fn console_buffer_caps_at_configured_size() {
        let (mut t, mut device) = pair();
        // Lower the cap so the test doesn't have to push a megabyte through.
        t.set_console_buffer_cap(32);

        // Write 200 bytes total; the cap is 32, so only the last 32 should remain.
        let payload: Vec<u8> = (0..200u8).collect();
        device.write_all(&payload).await.unwrap();

        // Drain through read_chunk so the reader task has definitely processed
        // the data (reader_loop updates the buffer before sending to mpsc).
        while t.read_chunk(Duration::from_millis(100)).await.is_ok() {}

        let raw = t.buffer.lock().unwrap().clone();
        assert_eq!(raw.len(), 32);
        // Last 32 bytes of `0..200` are `168..200`.
        assert_eq!(raw, (168u8..200u8).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn console_buffer_uses_default_cap_unset() {
        // Just verify the default doesn't immediately bite — write a small
        // payload, observe nothing gets trimmed.
        let (mut t, mut device) = pair();
        device.write_all(b"hello world").await.unwrap();
        let _ = t.read_chunk(Duration::from_secs(1)).await.unwrap();
        let raw = t.buffer.lock().unwrap().clone();
        assert_eq!(raw, b"hello world");
    }

    // ---- idle_for + drain_until_silent ----

    #[tokio::test]
    async fn idle_for_grows_when_no_chunks_arrive() {
        let (t, _device) = pair();
        // Right after creation, idle_for is tiny.
        let before = t.idle_for();
        assert!(before < Duration::from_millis(100));
        tokio::time::sleep(Duration::from_millis(150)).await;
        let after = t.idle_for();
        assert!(after >= Duration::from_millis(150));
    }

    #[tokio::test]
    async fn idle_for_resets_when_chunk_arrives() {
        let (mut t, mut device) = pair();
        tokio::time::sleep(Duration::from_millis(150)).await;
        device.write_all(b"hi").await.unwrap();
        // Read the chunk to make sure the reader task processed it
        // (which updates last_rx_at).
        let _ = t.read_chunk(Duration::from_secs(1)).await.unwrap();
        let idle = t.idle_for();
        assert!(
            idle < Duration::from_millis(50),
            "expected idle ~0 after fresh chunk, got {idle:?}"
        );
    }

    #[tokio::test]
    async fn drain_until_silent_returns_after_threshold_with_no_traffic() {
        let (mut t, _device) = pair();
        let start = Instant::now();
        let drained = t
            .drain_until_silent(Duration::from_millis(100), Duration::from_secs(5))
            .await;
        let elapsed = start.elapsed();
        // Should return roughly when "no chunks for 100ms" condition is met.
        // Allowing generous slack for scheduling.
        assert!(drained.is_empty());
        assert!(
            elapsed < Duration::from_millis(500),
            "drain_until_silent should exit quickly on a quiet line, took {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn drain_until_silent_collects_traffic_then_returns_on_silence() {
        let (mut t, mut device) = pair();

        // Spawn a producer that emits bytes for ~300ms, then stops.
        let producer = tokio::spawn(async move {
            for _ in 0..3 {
                device.write_all(b"chunk").await.unwrap();
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            // Then silence — keep device alive so the host side doesn't EOF.
            tokio::time::sleep(Duration::from_secs(2)).await;
        });

        let drained = t
            .drain_until_silent(Duration::from_millis(200), Duration::from_secs(5))
            .await;

        assert!(
            drained.len() >= b"chunkchunkchunk".len(),
            "expected to collect at least 3 chunks, got {} bytes",
            drained.len()
        );

        let _ = producer.await;
    }

    #[tokio::test]
    async fn drain_until_silent_respects_max_total_even_under_continuous_traffic() {
        let (mut t, mut device) = pair();

        // Producer never stops talking.
        let producer = tokio::spawn(async move {
            for _ in 0..50 {
                if device.write_all(b"x").await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });

        let start = Instant::now();
        let drained = t
            .drain_until_silent(Duration::from_secs(10), Duration::from_millis(300))
            .await;
        let elapsed = start.elapsed();

        // We should bail out at the max_total deadline since silence never
        // arrives. Some bytes will have been collected.
        assert!(
            elapsed < Duration::from_millis(700),
            "should exit near max_total, took {elapsed:?}"
        );
        assert!(!drained.is_empty(), "should have collected some bytes");

        producer.abort();
    }
}
