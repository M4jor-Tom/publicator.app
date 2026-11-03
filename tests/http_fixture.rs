//! The loopback HTTP fixture reports unmet expectations instead of hanging the harness.
//! Each scenario runs in a child test process owned by `common::failing_child_test`.
mod common;

use common::http::{Reply, Server};
use std::{
    io::{Read, Write},
    net::TcpStream,
    time::Duration,
};

#[test]
#[ignore = "child process of the watchdog tests below"]
fn scenario() {
    match std::env::var("HTTP_FIXTURE_SCENARIO").unwrap().as_str() {
        "unrequested" => Server::start(vec![Reply::new("/never", 200, "{}")]).finish(),
        "unreleased" => {
            let (server, started, _held) = Server::start_paused(Reply::new("/held", 200, "{}"));
            let mut client =
                TcpStream::connect(server.url().trim_start_matches("http://")).unwrap();
            write!(client, "GET /held HTTP/1.1\r\nHost: test\r\n\r\n").unwrap();
            started.recv_timeout(Duration::from_secs(10)).unwrap();
            server.finish();
        }
        "unwinding" => {
            let server = Server::start(vec![Reply::new("/expected", 200, "{}")]);
            let mut client =
                TcpStream::connect(server.url().trim_start_matches("http://")).unwrap();
            write!(client, "GET /other HTTP/1.1\r\nHost: test\r\n\r\n").unwrap();
            // EOF: the server thread consumed the request and already panicked.
            let _ = client.read(&mut [0; 1]);
            panic!("test body failed first");
        }
        other => panic!("unknown scenario {other}"),
    }
}

/// Runs one scenario under a watchdog; it must fail as a test (exit 101), not hang or abort.
fn run(name: &str) -> String {
    common::failing_child_test("scenario", &[("HTTP_FIXTURE_SCENARIO", name.as_ref())])
}

#[test]
fn finishing_without_an_expected_request_fails_with_its_path() {
    let text = run("unrequested");
    assert!(
        text.contains(r#"expected HTTP requests never arrived: ["/never"]"#),
        "{text}"
    );
}

#[test]
fn finishing_with_an_unreleased_gate_fails_with_its_path() {
    let text = run("unreleased");
    assert!(
        text.contains("paused HTTP reply for /held was never released"),
        "{text}"
    );
}

#[test]
fn server_failure_during_test_unwinding_does_not_double_panic() {
    let text = run("unwinding");
    assert!(text.contains("unexpected test HTTP path"), "{text}");
    assert!(text.contains("test body failed first"), "{text}");
}
