// src/gui/http.rs
//
// A minimal HTTP server — serves the embedded GUI on every request.
// The WebSocket port number is injected at serve-time by replacing the
// __WS_PORT__ placeholder, so multi-instance setups work without rebuilding.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

// Embed the GUI HTML into the binary at compile time.
// `cargo build` will fail with a clear message if the file is missing.
const GUI_HTML: &str = include_str!("../../gui/vitruvius_gui.html");

pub async fn serve(mut stream: TcpStream, ws_port: u16) {
    // Drain the HTTP request — we serve the same response for every path
    let mut buf = [0u8; 512];
    let _ = stream.read(&mut buf).await;

    let html = GUI_HTML.replace("__WS_PORT__", &ws_port.to_string());
    let response = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {len}\r\n\
         Cache-Control: no-cache\r\n\
         Connection: close\r\n\
         \r\n\
         {html}",
        len = html.len(),
    );
    let _ = stream.write_all(response.as_bytes()).await;
}
