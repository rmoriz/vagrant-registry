//! Native HTTP server adapter.
//!
//! Like the Worker adapter, this one is deliberately dumb: it hands the raw
//! method and path to the runtime-independent core and copies the reply onto
//! the response without interpreting it.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::process::ExitCode;
use std::time::Duration;

use axum::Router;
use axum::body::Body as ResponseBody;
use axum::extract::OriginalUri;
use axum::http::{HeaderValue, Method, StatusCode, Uri};
use axum::response::Response;
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;
use vagrant_registry_core::http::{Body, Reply, handle, request_path};

const DEFAULT_PORT: u16 = 8080;
const HEALTHCHECK_TIMEOUT: Duration = Duration::from_secs(5);
const HEALTHCHECK_REQUEST: &str =
    "GET /healthz HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";

/// Query and fragment are dropped before core dispatch; percent-encoding stays.
fn path_of(uri: &Uri) -> &str {
    request_path(uri.path())
}

fn handle_reply(reply: Reply) -> Response {
    let status = StatusCode::from_u16(reply.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let body = match reply.body {
        Body::Empty => ResponseBody::empty(),
        Body::Json(bytes) => ResponseBody::from(bytes),
        Body::Html(document) => ResponseBody::from(document),
        Body::Text(text) => ResponseBody::from(text),
    };

    let mut response = Response::new(body);
    *response.status_mut() = status;

    for (name, value) in reply.headers {
        if let Ok(value) = HeaderValue::from_str(value) {
            response.headers_mut().append(*name, value);
        }
    }

    response
}

async fn handler(method: Method, OriginalUri(uri): OriginalUri) -> Response {
    handle_reply(handle(method.as_str(), path_of(&uri)))
}

#[tokio::main]
async fn main() -> ExitCode {
    let port = match port() {
        Ok(port) => port,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };

    if std::env::args()
        .skip(1)
        .any(|argument| argument == "--healthcheck")
    {
        return healthcheck(port);
    }

    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    let app = Router::new()
        .fallback(handler)
        .layer(TraceLayer::new_for_http());

    let address = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = match TcpListener::bind(address).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("cannot bind {address}: {error}");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!("listening on http://{address}");

    if let Err(error) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
    {
        eprintln!("server error: {error}");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}

fn port() -> Result<u16, String> {
    let Ok(raw) = std::env::var("PORT") else {
        return Ok(DEFAULT_PORT);
    };

    raw.parse()
        .map_err(|_| format!("PORT {raw:?} is not a port"))
}

/// Probes `/healthz` over loopback and succeeds only on a `200` status line.
fn healthcheck(port: u16) -> ExitCode {
    let mut stream = match TcpStream::connect(("127.0.0.1", port)) {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!("healthcheck: cannot connect to 127.0.0.1:{port}: {error}");
            return ExitCode::FAILURE;
        }
    };

    if let Err(error) = stream.set_read_timeout(Some(HEALTHCHECK_TIMEOUT)) {
        eprintln!("healthcheck: cannot set a read timeout: {error}");
        return ExitCode::FAILURE;
    }

    if let Err(error) = stream.write_all(HEALTHCHECK_REQUEST.as_bytes()) {
        eprintln!("healthcheck: cannot send the request: {error}");
        return ExitCode::FAILURE;
    }

    let mut status_line = String::new();
    if let Err(error) = BufReader::new(&stream).read_line(&mut status_line) {
        eprintln!("healthcheck: cannot read the status line: {error}");
        return ExitCode::FAILURE;
    }

    let status_line = status_line.trim();
    println!("{status_line}");

    let mut parts = status_line.split(' ');
    let version_ok = parts
        .next()
        .is_some_and(|version| version.starts_with("HTTP/"));
    let status_ok = parts.next() == Some("200");

    if version_ok && status_ok {
        return ExitCode::SUCCESS;
    }

    eprintln!("healthcheck: unexpected status line {status_line:?}");
    ExitCode::FAILURE
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            eprintln!("cannot listen for SIGINT: {error}");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(error) => eprintln!("cannot listen for SIGTERM: {error}"),
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }

    tracing::info!("shutting down");
}

#[cfg(test)]
mod tests {
    use super::{handle_reply, path_of};
    use axum::body::to_bytes;
    use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE};
    use axum::http::{HeaderValue, StatusCode, Uri};
    use vagrant_registry_core::http::{Body, Reply, request_path};

    static JSON_ONLY: [(&str, &str); 1] = [("content-type", "application/json")];

    async fn body_bytes(response: axum::response::Response) -> Vec<u8> {
        to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should collect")
            .to_vec()
    }

    #[tokio::test]
    async fn empty_body_keeps_its_headers_and_gains_no_content_length() {
        let reply = Reply {
            status: 200,
            headers: &JSON_ONLY,
            body: Body::Empty,
        };

        let response = handle_reply(reply);

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(CONTENT_TYPE),
            Some(&HeaderValue::from_static("application/json"))
        );
        assert!(response.headers().get(CONTENT_LENGTH).is_none());
        assert_eq!(body_bytes(response).await, Vec::<u8>::new());
    }

    #[tokio::test]
    async fn json_body_survives_byte_for_byte_with_its_status() {
        for (status, expected) in [
            (200, StatusCode::OK),
            (404, StatusCode::NOT_FOUND),
            (405, StatusCode::METHOD_NOT_ALLOWED),
        ] {
            let reply = Reply {
                status,
                headers: &JSON_ONLY,
                body: Body::Json(b"{\"key\":\"value\"}"),
            };

            let response = handle_reply(reply);

            assert_eq!(response.status(), expected, "status {status}");
            assert!(response.headers().get(CONTENT_LENGTH).is_none());
            assert_eq!(body_bytes(response).await, b"{\"key\":\"value\"}");
        }
    }

    #[test]
    fn path_of_drops_the_query_and_keeps_percent_encoding() {
        assert_eq!(request_path("/acme/base?x=1"), "/acme/base");
        assert_eq!(request_path("/acme/base#frag?x=1"), "/acme/base");

        let encoded: Uri = "/acme%2Fbase".parse().expect("valid uri");
        assert_eq!(path_of(&encoded), "/acme%2Fbase");
    }
}
