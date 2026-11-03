use serde_json::Value;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    panic::resume_unwind,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

/// Bounds every wait on the code under test: a missing request or unreleased gate fails, not hangs.
const WAIT: Duration = Duration::from_secs(10);

pub struct Reply {
    pub path: String,
    pub status: u16,
    pub body: String,
}

pub struct Server {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
    /// Disconnects when the server thread returns or unwinds; nothing is ever sent.
    done: mpsc::Receiver<()>,
    /// Returns the paths of scripted replies whose requests never arrived.
    thread: Option<thread::JoinHandle<Vec<String>>>,
}

impl Server {
    pub fn start(replies: Vec<Reply>) -> Self {
        Self::start_gated(replies, None)
    }

    pub fn start_paused(reply: Reply) -> (Self, mpsc::Receiver<()>, mpsc::Sender<()>) {
        Self::start_paused_at(vec![reply], 0)
    }

    /// Serves `replies` in order, holding the `index`-th (0-based) after its status line and
    /// headers: `started` fires once that request is captured, its body follows `release`.
    pub fn start_paused_at(
        replies: Vec<Reply>,
        index: usize,
    ) -> (Self, mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        (
            Self::start_gated(replies, Some((index, started_tx, release_rx))),
            started_rx,
            release_tx,
        )
    }

    fn start_gated(
        replies: Vec<Reply>,
        mut gate: Option<(usize, mpsc::Sender<()>, mpsc::Receiver<()>)>,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(false).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let (done_tx, done) = mpsc::channel::<()>();
        let thread = thread::spawn(move || {
            let _done = done_tx;
            let mut replies = replies.into_iter();
            while let Some(reply) = replies.next() {
                let (mut stream, _) = listener.accept().expect("expected HTTP request");
                if stopping.load(Ordering::SeqCst) {
                    // The shutdown wakeup, not a request: report what was never asked for.
                    return std::iter::once(reply)
                        .chain(replies)
                        .map(|reply| reply.path)
                        .collect();
                }
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let (path, body) = read_request(&mut stream);
                assert_eq!(path, reply.path, "unexpected test HTTP path");
                let served = {
                    let mut captured = captured.lock().unwrap();
                    captured.push(serde_json::json!({"path": path, "body": body}));
                    captured.len()
                };
                let reason = if reply.status == 200 { "OK" } else { "Error" };
                write!(
                    stream,
                    "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    reply.status,
                    reason,
                    reply.body.len(),
                )
                .unwrap();
                if let Some((_, started, release)) = gate.take_if(|(at, ..)| *at + 1 == served) {
                    let _ = started.send(());
                    release.recv_timeout(WAIT).unwrap_or_else(|_| {
                        panic!("paused HTTP reply for {path} was never released")
                    });
                    // The client may have abandoned the held body at its deadline; tests assert that.
                    let _ = stream.write_all(reply.body.as_bytes());
                } else {
                    stream.write_all(reply.body.as_bytes()).unwrap();
                }
            }
            Vec::new()
        });
        Self {
            url,
            requests,
            stop,
            done,
            thread: Some(thread),
        }
    }

    /// Requires every scripted request, waiting at most `WAIT` for in-flight ones, and
    /// re-raises a server-thread panic (wrong path, unreleased gate) in the test.
    pub fn finish(mut self) {
        let _ = self.done.recv_timeout(WAIT);
        let unmet = self
            .shut_down()
            .unwrap()
            .unwrap_or_else(|panic| resume_unwind(panic));
        assert!(
            unmet.is_empty(),
            "expected HTTP requests never arrived: {unmet:?}"
        );
    }

    /// Wakes a blocked accept and joins the thread; `None` once already shut down.
    fn shut_down(&mut self) -> Option<thread::Result<Vec<String>>> {
        let thread = self.thread.take()?;
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.url.trim_start_matches("http://"));
        Some(thread.join())
    }

    pub fn url(&self) -> &str {
        &self.url
    }
    pub fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for Server {
    /// Lenient about unconsumed replies, but never hides a server-thread panic, and never
    /// panics again while the test is already unwinding.
    fn drop(&mut self) {
        if let Some(Err(panic)) = self.shut_down()
            && !thread::panicking()
        {
            resume_unwind(panic);
        }
    }
}

fn read_request(stream: &mut TcpStream) -> (String, Value) {
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    let header_end;
    loop {
        let count = stream.read(&mut buffer).expect("read HTTP request");
        assert_ne!(count, 0, "client closed before HTTP headers");
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            header_end = position + 4;
            break;
        }
    }
    let headers = String::from_utf8_lossy(&bytes[..header_end]);
    let path = headers
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .to_owned();
    let length: usize = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().unwrap())
        })
        .unwrap_or(0);
    while bytes.len() < header_end + length {
        let count = stream.read(&mut buffer).expect("read HTTP body");
        assert_ne!(count, 0, "client closed before HTTP body");
        bytes.extend_from_slice(&buffer[..count]);
    }
    let body = if length == 0 {
        Value::Null
    } else {
        serde_json::from_slice(&bytes[header_end..header_end + length]).expect("JSON request body")
    };
    (path, body)
}
