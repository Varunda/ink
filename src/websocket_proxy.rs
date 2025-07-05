use axum::{body::Body, extract::Request, response::Response};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{SinkExt, stream::StreamExt};
use http::{HeaderMap, HeaderValue};
use hyper::StatusCode;
use hyper_util::rt::TokioIo;
use reqwest::Url;
use sha1::{Digest, Sha1};
use tokio::{sync::mpsc, time::Duration, time::timeout};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use url::Host;

// this is just
// https://github.com/tom-lubenow/axum-reverse-proxy/blob/main/src/websocket.rs
// cause it's public to crate and i need it here too

pub fn is_websocket_upgrade(headers: &HeaderMap<HeaderValue>) -> bool {
    // Check for required WebSocket upgrade headers
    let has_upgrade = headers
        .get("upgrade")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);

    let has_connection = headers
        .get("connection")
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.split(',')
                .any(|part| part.trim().eq_ignore_ascii_case("upgrade"))
        })
        .unwrap_or(false);

    let has_websocket_key = headers.contains_key("sec-websocket-key");
    let has_websocket_version = headers.contains_key("sec-websocket-version");

    tracing::trace!(
        "is_websocket_upgrade - upgrade: {has_upgrade}, connection: {has_connection}, websocket key: {has_websocket_key}, websocket version: {has_websocket_version}"
    );
    return has_upgrade && has_connection && has_websocket_key && has_websocket_version;
}

pub async fn handle_websocket(
    req: Request<Body>,
    target: &str,
) -> Result<Response<Body>, Box<dyn std::error::Error + Send + Sync>> {
    tracing::trace!("Handling WebSocket upgrade request");

    // Get the WebSocket key before upgrading
    let ws_key = req
        .headers()
        .get("sec-websocket-key")
        .and_then(|key| key.to_str().ok())
        .ok_or("Missing or invalid Sec-WebSocket-Key header")?;

    // Calculate the WebSocket accept key
    let mut hasher = Sha1::new();
    hasher.update(ws_key.as_bytes());
    hasher.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    let ws_accept = STANDARD.encode(hasher.finalize());

    // Get the path and query from the request
    let path_and_query = req.uri().path_and_query().map(|x| x.as_str()).unwrap_or("");

    tracing::trace!("Original path: {}", path_and_query);

    // Convert the target URL to WebSocket URL
    let upstream_url = if target.starts_with("ws://") || target.starts_with("wss://") {
        format!("{target}{path_and_query}")
    } else {
        let (scheme, rest) = if target.starts_with("https://") {
            ("wss://", target.trim_start_matches("https://"))
        } else {
            ("ws://", target.trim_start_matches("http://"))
        };
        format!("{}{}{}", scheme, rest.trim_end_matches('/'), path_and_query)
    };

    tracing::trace!("Connecting to upstream WebSocket at {}", upstream_url);

    // Parse the URL to get the host and scheme
    let url = Url::parse(&upstream_url)?;
    let scheme = url.scheme();
    let host = match url.host().ok_or("Missing host in URL")? {
        Host::Ipv6(addr) => format!("[{addr}]"),
        Host::Ipv4(addr) => addr.to_string(),
        Host::Domain(s) => s.to_string(),
    };
    let port = match url.port() {
        Some(p) => p,
        None => {
            if scheme == "wss" {
                443
            } else {
                80
            }
        }
    };
    let host_header = if (scheme == "wss" && port == 443) || (scheme == "ws" && port == 80) {
        host.clone()
    } else {
        format!("{host}:{port}")
    };

    // Forward all headers except host to upstream
    let mut request = tokio_tungstenite::tungstenite::handshake::client::Request::builder()
        .uri(upstream_url)
        .header("host", host_header);

    for (key, value) in req.headers() {
        if key != "host" {
            request = request.header(key.as_str(), value);
        }
    }

    // Build the request
    let request = request.body(())?;

    // Log the request headers
    tracing::trace!("Upstream request headers: {:?}", request.headers());

    // Return a response that indicates the connection has been upgraded
    tracing::trace!("Returning upgrade response to client");
    let response = Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header("Upgrade", "websocket")
        .header("Connection", "Upgrade")
        .header("Sec-WebSocket-Accept", ws_accept)
        .body(Body::empty())?;

    // Spawn a task to handle the WebSocket connection
    let (parts, body) = req.into_parts();
    let req = Request::from_parts(parts, body);
    tokio::spawn(async move {
        match handle_websocket_connection(req, request).await {
            Ok(_) => tracing::trace!("WebSocket connection closed gracefully"),
            Err(e) => tracing::error!("WebSocket connection error: {}", e),
        }
    });

    Ok(response)
}

async fn handle_websocket_connection(
    req: Request<Body>,
    upstream_request: tokio_tungstenite::tungstenite::handshake::client::Request,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let upgraded = match timeout(Duration::from_secs(5), hyper::upgrade::on(req)).await {
        Ok(Ok(upgraded)) => upgraded,
        Ok(Err(e)) => return Err(Box::new(e)),
        Err(e) => return Err(Box::new(e)),
    };

    let io = TokioIo::new(upgraded);
    let client_ws = tokio_tungstenite::WebSocketStream::from_raw_socket(
        io,
        tokio_tungstenite::tungstenite::protocol::Role::Server,
        None,
    )
    .await;

    let (upstream_ws, _) =
        match timeout(Duration::from_secs(5), connect_async(upstream_request)).await {
            Ok(Ok(conn)) => conn,
            Ok(Err(e)) => return Err(Box::new(e)),
            Err(e) => return Err(Box::new(e)),
        };

    let (mut client_sender, mut client_receiver) = client_ws.split();
    let (mut upstream_sender, mut upstream_receiver) = upstream_ws.split();

    let (close_tx, mut close_rx) = mpsc::channel::<()>(1);
    let close_tx_upstream = close_tx.clone();

    let client_to_upstream = tokio::spawn(async move {
        let mut client_closed = false;
        while let Some(msg) = client_receiver.next().await {
            let msg = msg?;
            match msg {
                Message::Close(_) => {
                    if !client_closed {
                        upstream_sender.send(Message::Close(None)).await?;
                        close_tx.send(()).await.ok();
                        client_closed = true;
                        break;
                    }
                }
                msg @ Message::Binary(_)
                | msg @ Message::Text(_)
                | msg @ Message::Ping(_)
                | msg @ Message::Pong(_) => {
                    if !client_closed {
                        upstream_sender.send(msg).await?;
                    }
                }
                Message::Frame(_) => {}
            }
        }
        if !client_closed {
            upstream_sender.send(Message::Close(None)).await?;
            close_tx.send(()).await.ok();
        }
        Ok::<_, tokio_tungstenite::tungstenite::Error>(())
    });

    let upstream_to_client = tokio::spawn(async move {
        let mut upstream_closed = false;
        while let Some(msg) = upstream_receiver.next().await {
            let msg = msg?;
            match msg {
                Message::Close(_) => {
                    if !upstream_closed {
                        client_sender.send(Message::Close(None)).await?;
                        close_tx_upstream.send(()).await.ok();
                        upstream_closed = true;
                        break;
                    }
                }
                msg @ Message::Binary(_)
                | msg @ Message::Text(_)
                | msg @ Message::Ping(_)
                | msg @ Message::Pong(_) => {
                    if !upstream_closed {
                        client_sender.send(msg).await?;
                    }
                }
                Message::Frame(_) => {}
            }
        }
        if !upstream_closed {
            client_sender.send(Message::Close(None)).await?;
            close_tx_upstream.send(()).await.ok();
        }
        Ok::<_, tokio_tungstenite::tungstenite::Error>(())
    });

    tokio::select! {
        _ = close_rx.recv() => {
            tracing::trace!("WebSocket connection closed gracefully");
        }
        res = client_to_upstream => {
            if let Err(e) = res {
                tracing::error!("Client to upstream task failed: {:?}", e);
            }
        }
        res = upstream_to_client => {
            if let Err(e) = res {
                tracing::error!("Upstream to client task failed: {:?}", e);
            }
        }
    }

    Ok(())
}
