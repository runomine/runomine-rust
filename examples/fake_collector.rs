//! A stand-in for runomine's `POST /api/v1/events`, for trying the other examples locally. Prints every batch it
//! receives as one JSON line (`{"authorization": …, "events": […]}`) and answers 202.
//!
//! ```sh
//! cargo run --example fake_collector              # http://127.0.0.1:8788
//! RUNOMINE_URL=http://127.0.0.1:8788 RUNOMINE_TOKEN=rm_live_test cargo run --example axum --features axum
//! ```

use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8788);
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port))).await?;
    eprintln!("fake collector on http://{}", listener.local_addr()?);
    loop {
        let (stream, _) = listener.accept().await?;
        tokio::spawn(async move {
            let _ = handle(stream).await;
        });
    }
}

/// A tiny HTTP/1.1 handler (keep-alive aware): enough for a client that POSTs JSON with a Content-Length.
async fn handle(stream: tokio::net::TcpStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream);
    loop {
        let mut request_line = String::new();
        if reader.read_line(&mut request_line).await? == 0 {
            return Ok(());
        }
        let mut length = 0usize;
        let mut authorization = String::new();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).await?;
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                match name.trim().to_ascii_lowercase().as_str() {
                    "content-length" => length = value.trim().parse().unwrap_or(0),
                    "authorization" => authorization = value.trim().to_owned(),
                    _ => {}
                }
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).await?;
        let (status, reply) = if request_line.starts_with("POST /api/v1/events ") {
            let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
            let events = parsed["events"].clone();
            let n = events.as_array().map_or(0, Vec::len);
            let line = serde_json::json!({"authorization": authorization, "events": events});
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{line}");
            let _ = out.flush();
            (
                "202 Accepted",
                format!("{{\"accepted\":{n},\"rejected\":[]}}"),
            )
        } else {
            ("404 Not Found", "{}".to_owned())
        };
        let response = format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{reply}",
            reply.len()
        );
        reader.get_mut().write_all(response.as_bytes()).await?;
    }
}
