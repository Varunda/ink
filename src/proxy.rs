use axum::{
    body::Body,
    extract::{Request, State},
    http::uri::Uri,
    middleware::Next,
    response::{IntoResponse, Response},
};
use http::StatusCode;
use hyper_util::client::legacy::connect::HttpConnector;

use crate::{app_state, docker, websocket_proxy};

pub type Client = hyper_util::client::legacy::Client<HttpConnector, Body>;

pub async fn proxy_handler(
    State(state): State<app_state::AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let host = request.headers().get("host");

    if host.is_some() {
        let host = host.unwrap().to_str().unwrap();

        let parts = host.split(".");
        let parts = parts.collect::<Vec<&str>>();
        if parts.len() < 1 {
            panic!("how is there less than 1 part to the host in '{}'", host);
        }

        let instance = parts[0];

        let d = docker::get_instance_by_name(instance).await;
        if d.is_err() {
            tracing::error!("error getting docker instance: {}", d.unwrap_err());
            panic!();
        }

        let d = d.unwrap();

        if d.len() > 0 {
            let path = request.uri().path();
            let path_query = request
                .uri()
                .path_and_query()
                .map(|v| v.as_str())
                .unwrap_or(path);

            if path.starts_with("/DbAdmin")
                || path.starts_with("/rulesets")
                || path.starts_with("/TeamBuilder")
            {
                return axum::http::Response::builder()
                    .status(StatusCode::FORBIDDEN)
                    .body(Body::from("no permission to view this page"))
                    .unwrap();
            }

            let port = d[0].port;
            let uri = format!("http://127.0.0.1:{}{}", port, path_query);
            tracing::debug!("proxying {} to {}", host, uri);

            *request.uri_mut() = Uri::try_from(uri).unwrap();

            if websocket_proxy::is_websocket_upgrade(request.headers()) {
                tracing::trace!("Detected WebSocket upgrade request");
                match websocket_proxy::handle_websocket(request, &format!("127.0.0.1:{}", port))
                    .await
                {
                    Ok(response) => return response,
                    Err(e) => {
                        tracing::error!("Failed to handle WebSocket upgrade: {}", e);
                        return axum::http::Response::builder()
                            .status(StatusCode::INTERNAL_SERVER_ERROR)
                            .body(Body::from(format!("WebSocket upgrade failed: {e}")))
                            .unwrap();
                    }
                }
            } else {
                match state.client.request(request).await {
                    Ok(r) => {
                        return r.into_response();
                    }
                    Err(e) => {
                        tracing::error!("error proxying http request: {}", e);
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("error proxying http request: {}", e),
                        )
                            .into_response();
                    }
                }
            }
        }
    }

    let response = next.run(request).await;

    return response;
}
