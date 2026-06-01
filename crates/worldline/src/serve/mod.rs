//! Serve the captured worldline as a local web UI.
//!
//! A deliberately small async HTTP/1.1 server (GET only, `Connection: close`)
//! over `tokio::net`. The traced program has already exited, so there is no
//! streaming/websocket concern — we serve the embedded SPA plus two endpoints:
//! `/api/data` (the reconstructed timeline JSON) and `/api/output` (the raw
//! terminal byte log).

mod assets;
mod data;

use std::{net::Ipv4Addr, sync::Arc};

pub use data::{ApiData, ApiWrite, Blob, BlobEncoding, reconstruct};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
};

use crate::run::Captured;

const MAX_HEAD: usize = 16 * 1024;

struct Payload {
    json: Vec<u8>,
    output: Vec<u8>,
}

/// Serve the captured worldline on `127.0.0.1:port` until Ctrl-C.
///
/// `port` may be 0 to let the OS choose a free port. When `open` is set, the
/// resolved URL is opened in the default browser (best effort).
///
/// # Errors
///
/// Returns an error if the listener cannot be bound or serialization fails.
pub async fn serve(captured: &Captured, port: u16, open: bool) -> anyhow::Result<()> {
    let api = reconstruct(captured);
    let payload =
        Arc::new(Payload { json: serde_json::to_vec(&api)?, output: captured.data.output.clone() });

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
    let addr = listener.local_addr()?;
    let url = vite_str::format!("http://{addr}");
    announce(&url);
    if open {
        open_browser(&url);
    }

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                if let Ok((stream, _)) = accepted {
                    let payload = Arc::clone(&payload);
                    tokio::spawn(async move {
                        let _ = handle(stream, &payload).await;
                    });
                }
            }
            result = tokio::signal::ctrl_c() => {
                result?;
                break;
            }
        }
    }
    Ok(())
}

#[expect(clippy::print_stdout, reason = "worldline is a user-facing CLI")]
fn announce(url: &str) {
    println!("worldline: serving at {url} (press Ctrl-C to stop)");
}

async fn handle(mut stream: TcpStream, payload: &Payload) -> std::io::Result<()> {
    let Some(target) = read_target(&mut stream).await? else {
        return respond(&mut stream, "400 Bad Request", "text/plain", b"bad request").await;
    };

    // Strip any query string and the leading slash.
    let path = target.split('?').next().unwrap_or("").trim_start_matches('/');
    match path {
        "api/data" => {
            respond(&mut stream, "200 OK", "application/json; charset=utf-8", &payload.json).await
        }
        "api/output" => {
            respond(&mut stream, "200 OK", "application/octet-stream", &payload.output).await
        }
        _ => {
            let key = if path.is_empty() { "index.html" } else { path };
            match assets::get(key) {
                Some(bytes) => {
                    respond(&mut stream, "200 OK", assets::content_type(key), bytes).await
                }
                None => respond(&mut stream, "404 Not Found", "text/plain", b"not found").await,
            }
        }
    }
}

/// Read the request head and return the request target of a `GET`, or `None`
/// for a malformed / non-GET request.
async fn read_target(stream: &mut TcpStream) -> std::io::Result<Option<vite_str::Str>> {
    let mut head = Vec::new();
    let mut chunk = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < MAX_HEAD {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        head.extend_from_slice(&chunk[..read]);
    }

    let first_line = head.split(|&b| b == b'\n').next().unwrap_or(&[]);
    let mut parts = first_line.split(|&b| b == b' ').filter(|s| !s.is_empty());
    let method = parts.next();
    let target = parts.next();
    if method != Some(b"GET") {
        return Ok(None);
    }
    Ok(target.and_then(|t| std::str::from_utf8(t).ok()).map(vite_str::Str::from))
}

async fn respond(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let head = vite_str::format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await
}

/// Best-effort: open `url` in the default browser.
fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let (program, args): (&str, &[&str]) = ("open", &[]);
    #[cfg(target_os = "windows")]
    let (program, args): (&str, &[&str]) = ("cmd", &["/C", "start", ""]);
    #[cfg(all(unix, not(target_os = "macos")))]
    let (program, args): (&str, &[&str]) = ("xdg-open", &[]);

    let _ = std::process::Command::new(program).args(args).arg(url).spawn();
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use tokio::{
        io::{AsyncReadExt as _, AsyncWriteExt as _},
        net::{TcpListener, TcpStream},
    };

    use super::{Payload, handle, reconstruct};
    use crate::{
        capture::Snapshotter,
        run::{Captured, Meta},
    };

    /// Send `GET path` and return `(status_line, body)`.
    async fn get(addr: std::net::SocketAddr, path: &str) -> (vite_str::Str, Vec<u8>) {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let request = vite_str::format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        let split = response.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = std::str::from_utf8(&response[..split]).unwrap_or("");
        let status = vite_str::Str::from(head.lines().next().unwrap_or(""));
        (status, response[split + 4..].to_vec())
    }

    fn body_text(body: &[u8]) -> &str {
        std::str::from_utf8(body).unwrap_or("")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn serves_endpoints_and_assets() {
        let snap = Snapshotter::new();
        snap.append_output(b"hello-output");
        snap.record_open(1, 3, "/w/a.txt", b"", None);
        snap.record_close(1, 3, "/w/a.txt", b"content", None);
        let captured = Captured {
            meta: Meta { argv: vec!["prog".into()], cwd: "/w".into(), exit_code: Some(0) },
            data: snap.finish(),
        };
        let payload = std::sync::Arc::new(Payload {
            json: serde_json::to_vec(&reconstruct(&captured)).unwrap(),
            output: captured.data.output.clone(),
        });

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for _ in 0..4 {
                if let Ok((stream, _)) = listener.accept().await {
                    let _ = handle(stream, &payload).await;
                }
            }
        });

        let (status, body) = get(addr, "/api/data").await;
        assert!(status.contains("200"), "status was {status}");
        let text = body_text(&body);
        assert!(text.contains("a.txt"), "data missing a.txt: {text}");
        assert!(text.contains("\"content\""), "data missing blob content: {text}");

        let (_, out) = get(addr, "/api/output").await;
        assert_eq!(out, b"hello-output");

        let (idx_status, idx_body) = get(addr, "/").await;
        assert!(idx_status.contains("200"));
        assert!(body_text(&idx_body).contains("<!doctype html"));

        let (missing, _) = get(addr, "/nope.js").await;
        assert!(missing.contains("404"), "expected 404, got {missing}");

        server.await.unwrap();
    }
}
