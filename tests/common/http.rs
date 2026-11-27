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
    time::{Duration, Instant},
};

/// Bounds every wait on the code under test: a missing request or unreleased gate fails, not hangs.
const WAIT: Duration = Duration::from_secs(10);

pub struct Reply {
    pub path: String,
    pub status: u16,
    pub body: String,
    headers: Vec<(String, String)>,
    release_headers: Option<mpsc::Receiver<()>>,
    skipped_body: Option<mpsc::Sender<()>>,
}

impl Reply {
    pub fn new(path: &str, status: u16, body: impl ToString) -> Self {
        Self {
            path: path.into(),
            status,
            body: body.to_string(),
            headers: Vec::new(),
            release_headers: None,
            skipped_body: None,
        }
    }

    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn hold_headers(mut self) -> (Self, mpsc::Sender<()>) {
        let (send, receive) = mpsc::channel();
        self.release_headers = Some(receive);
        (self, send)
    }

    /// Keep a nonempty body unsent; EOF proves the client acted on headers alone.
    pub fn confirm_skipped_body(mut self) -> (Self, mpsc::Receiver<()>) {
        assert!(!self.body.is_empty());
        let (send, receive) = mpsc::channel();
        self.skipped_body = Some(send);
        (self, receive)
    }
}

/// Generated metadata as a provider returns it: `title` and a complete description.
pub fn content(title: &str) -> String {
    serde_json::json!({"title":title,"description":"A complete generated description."}).to_string()
}

/// An OpenRouter chat completion answering `content(title)`.
pub fn completion(title: &str) -> Reply {
    let body = serde_json::json!({"choices":[{"message":{"content":content(title)}}]});
    Reply::new("/chat/completions", 200, body)
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

    fn start_gated(replies: Vec<Reply>, mut gate: Option<BodyGate>) -> Self {
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
            let mut gated = Vec::new();
            while let Some(reply) = replies.next() {
                let (mut stream, _) = listener.accept().expect("expected HTTP request");
                if stopping.load(Ordering::SeqCst) {
                    gated.into_iter().for_each(join_reply);
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
                let body_gate = gate.take_if(|(at, ..)| *at + 1 == served);
                if reply.release_headers.is_some()
                    || reply.skipped_body.is_some()
                    || body_gate.is_some()
                {
                    gated.push(thread::spawn(move || send_reply(stream, reply, body_gate)));
                } else {
                    send_reply(stream, reply, None);
                }
            }
            gated.into_iter().for_each(join_reply);
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

type BodyGate = (usize, mpsc::Sender<()>, mpsc::Receiver<()>);

fn join_reply(worker: thread::JoinHandle<()>) {
    if let Err(panic) = worker.join() {
        resume_unwind(panic);
    }
}

fn send_reply(mut stream: TcpStream, reply: Reply, body_gate: Option<BodyGate>) {
    if let Some(release) = reply.release_headers {
        match release.recv_timeout(WAIT) {
            Ok(()) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Err(e) => panic!("headers never released for {}: {e}", reply.path),
        }
    }
    let reason = if reply.status == 200 { "OK" } else { "Error" };
    let extra: String = reply
        .headers
        .iter()
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect();
    let mut headers = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n",
        reply.status,
        reason,
        reply.body.len()
    );
    if reply.skipped_body.is_none() && body_gate.is_none() {
        headers.push_str(&reply.body);
        stream.write_all(headers.as_bytes()).unwrap();
        return;
    }
    stream.write_all(headers.as_bytes()).unwrap();
    if let Some(skipped) = reply.skipped_body {
        let mut byte = [0];
        assert_eq!(
            stream
                .read(&mut byte)
                .expect("client should close without reading the body"),
            0
        );
        let _ = skipped.send(());
    } else if let Some((_, started, release)) = body_gate {
        let _ = started.send(());
        release
            .recv_timeout(WAIT)
            .unwrap_or_else(|_| panic!("paused HTTP reply for {} was never released", reply.path));
        // The client may have abandoned the body at its deadline.
        let _ = stream.write_all(reply.body.as_bytes());
    }
}

/// Pause Tokio time without auto-advancing timers while real loopback I/O is in flight.
/// Keep the sender alive and move time explicitly with `tokio::time::advance`.
pub fn manual_clock() -> mpsc::Sender<()> {
    let (keep_alive, until_dropped) = mpsc::channel();
    tokio::time::pause();
    tokio::task::spawn_blocking(move || {
        // Also release auto-advance if a broken test stops making progress.
        let _ = until_dropped.recv_timeout(WAIT * 3);
    });
    keep_alive
}

pub async fn wait_for(mut ready: impl FnMut() -> bool) {
    let started = Instant::now();
    while !ready() {
        assert!(
            started.elapsed() < WAIT,
            "loopback fixture did not progress"
        );
        tokio::task::yield_now().await;
    }
}

pub async fn received(rx: &mpsc::Receiver<()>) {
    wait_for(|| match rx.try_recv() {
        Ok(()) => true,
        Err(mpsc::TryRecvError::Empty) => false,
        Err(e) => panic!("fixture signal dropped: {e}"),
    })
    .await;
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
