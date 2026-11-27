//! Minimal HTTP server for the REST API (`/api/v1/…`). Requests are
//! handed to the app's frame loop, which answers them.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use crate::{IoError, Result};

pub struct HttpRequest {
    pub method: String,
    /// Path without the query string.
    pub path: String,
    pub body: String,
    reply: Sender<HttpResponse>,
}

pub struct HttpResponse {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

impl HttpRequest {
    pub fn respond(self, status: u16, content_type: &'static str, body: Vec<u8>) {
        let _ = self.reply.send(HttpResponse {
            status,
            content_type,
            body,
        });
    }

    pub fn json(self, v: &impl serde::Serialize) {
        let body = serde_json::to_vec(v).unwrap_or_default();
        self.respond(200, "application/json", body);
    }
}

pub struct WebServer {
    server: Arc<tiny_http::Server>,
    rx: Receiver<HttpRequest>,
    pub port: u16,
}

impl WebServer {
    pub fn start(address: &str, port: u16) -> Result<Self> {
        let server = tiny_http::Server::http((address, port)).map_err(|e| IoError::Other(e.to_string()))?;
        let server = Arc::new(server);
        let (tx, rx) = channel();
        let s = server.clone();
        std::thread::Builder::new().name("http".into()).spawn(move || {
            for rq in s.incoming_requests() {
                let tx = tx.clone();
                // One thread per request: each waits for the frame loop.
                let _ = std::thread::Builder::new()
                    .name("http-rq".into())
                    .spawn(move || serve(rq, &tx));
            }
        })?;
        Ok(Self { server, rx, port })
    }

    pub fn poll(&self) -> Vec<HttpRequest> {
        self.rx.try_iter().collect()
    }
}

impl Drop for WebServer {
    fn drop(&mut self) {
        self.server.unblock();
    }
}

fn header(k: &str, v: &str) -> tiny_http::Header {
    tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes()).expect("valid header")
}

fn serve(mut rq: tiny_http::Request, tx: &Sender<HttpRequest>) {
    let cors = [
        header("Access-Control-Allow-Origin", "*"),
        header("Access-Control-Allow-Methods", "GET, PUT, POST, DELETE, OPTIONS"),
        header("Access-Control-Allow-Headers", "Content-Type"),
    ];
    if *rq.method() == tiny_http::Method::Options {
        let mut r = tiny_http::Response::empty(204);
        for h in cors {
            r.add_header(h);
        }
        let _ = rq.respond(r);
        return;
    }
    let mut body = String::new();
    let _ = rq.as_reader().read_to_string(&mut body);
    let path = rq.url().split('?').next().unwrap_or("/").to_owned();
    let (reply, wait) = channel();
    let req = HttpRequest {
        method: rq.method().as_str().to_ascii_uppercase(),
        path,
        body,
        reply,
    };
    let res = if tx.send(req).is_ok() {
        wait.recv_timeout(Duration::from_secs(3)).ok()
    } else {
        None
    };
    let res = res.unwrap_or(HttpResponse {
        status: 503,
        content_type: "text/plain",
        body: b"busy".to_vec(),
    });
    let mut r = tiny_http::Response::from_data(res.body).with_status_code(res.status);
    r.add_header(header("Content-Type", res.content_type));
    for h in cors {
        r.add_header(h);
    }
    let _ = rq.respond(r);
}
