use anyhow::{Context, Result, anyhow};
use async_session::{MemoryStore, Session, SessionStore};
use axum::{
    RequestPartsExt, Router,
    extract::{FromRef, FromRequestParts, OptionalFromRequestParts, Query, State},
    http::{HeaderMap, header::SET_COOKIE},
    middleware,
    response::{IntoResponse, Redirect, Response},
    routing::get,
};
use axum_extra::{TypedHeader, headers, typed_header::TypedHeaderRejectionReason};
use http::{StatusCode, header, request::Parts};
use hyper_util::{client::legacy::connect::HttpConnector, rt::TokioExecutor};
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, RedirectUrl, Scope,
    TokenResponse, TokenUrl, basic::BasicClient, reqwest::async_http_client,
};
use serde::{Deserialize, Serialize};
use std::{convert::Infallible, env, time::Duration};
use tower::ServiceBuilder;
use tower_http::{services::ServeDir, trace::TraceLayer};

mod api;
mod app_state;
mod docker;
mod instance;
mod proxy;
mod websocket_proxy;

static COOKIE_NAME: &str = "INK_SESSION";
static CSRF_TOKEN: &str = "csrf_token";

// large parts of the oauth code is from
// https://github.com/tokio-rs/axum/blob/main/examples/oauth/src/main.rs

#[tokio::main]
async fn main() {
    dotenv::from_filename(".env").ok();

    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("ink=debug".parse().unwrap())
                .add_directive("bollard::docker=info".parse().unwrap())
                .add_directive("tower_http::trace::on_request=info".parse().unwrap())
                .add_directive("tower_http::trace::on_response=info".parse().unwrap())
                .add_directive("hyper_util::client::legacy::pool=info".parse().unwrap()),
        )
        .init();

    tracing::info!("starting ink");

    // `MemoryStore` is just used as an example. Don't use this in production.
    let store = MemoryStore::new();
    let oauth_client = oauth_client().unwrap();
    let client: proxy::Client =
        hyper_util::client::legacy::Client::<(), ()>::builder(TokioExecutor::new())
            .build(HttpConnector::new());

    let app_state = app_state::AppState {
        store,
        oauth_client,
        client,
    };

    let app = Router::new()
        .fallback_service(ServeDir::new("www").append_index_html_on_directories(true))
        .route("/auth/discord", get(discord_auth))
        .route("/auth/callback", get(login_authorized))
        .route("/api/create", get(api::create_instance))
        .route("/api/list", get(api::list_instances))
        .route("/api/whoami", get(api::whoami))
        .route("/api/mine", get(api::get_instance))
        .route("/logout", get(logout))
        .layer(
            ServiceBuilder::new()
                .layer(TraceLayer::new_for_http())
                .layer(middleware::from_fn_with_state(
                    app_state.clone(),
                    proxy::proxy_handler,
                )),
        )
        .with_state(app_state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8000").await.unwrap();

    let background = tokio::task::spawn(async move {
        instance_cleanup().await;
    });

    axum::serve(listener, app).await.unwrap();
    background.abort();
}

fn oauth_client() -> Result<BasicClient, AppError> {
    let client_id = env::var("DISCORD_CLIENT_ID").context("Missing DISCORD_CLIENT_ID!")?;
    let client_secret = env::var("DISCORD_SECRET").context("Missing DISCORD_SECRET!")?;
    let redirect_url = "http://localhost:8000/auth/callback".to_string();
    let auth_url = "https://discord.com/api/oauth2/authorize?response_type=code".to_string();
    let token_url = "https://discord.com/api/oauth2/token".to_string();

    return Ok(BasicClient::new(
        ClientId::new(client_id),
        Some(ClientSecret::new(client_secret)),
        AuthUrl::new(auth_url).context("failed to create new authorization server URL")?,
        Some(TokenUrl::new(token_url).context("failed to create new token endpoint URL")?),
    )
    .set_redirect_uri(
        RedirectUrl::new(redirect_url).context("failed to create new redirection URL")?,
    ));
}

// The user data we'll get back from Discord.
// https://discord.com/developers/docs/resources/user#user-object-user-structure
#[derive(Debug, Serialize, Deserialize)]
struct User {
    id: String,
    username: String,
    discriminator: String,
}

async fn discord_auth(
    State(client): State<BasicClient>,
    State(store): State<MemoryStore>,
) -> Result<impl IntoResponse, AppError> {
    let (auth_url, csrf_token) = client
        .authorize_url(CsrfToken::new_random)
        .add_scope(Scope::new("identify".to_string()))
        .url();

    // Create session to store csrf_token
    let mut session = Session::new();
    session
        .insert(CSRF_TOKEN, &csrf_token)
        .context("failed in inserting CSRF token into session")?;

    // Store the session in MemoryStore and retrieve the session cookie
    let cookie = store
        .store_session(session)
        .await
        .context("failed to store CSRF token session")?
        .context("unexpected error retrieving CSRF cookie value")?;

    // Attach the session cookie to the response header
    let cookie = format!("{COOKIE_NAME}={cookie}; SameSite=Lax; HttpOnly; Secure; Path=/");
    let mut headers = HeaderMap::new();
    headers.insert(
        SET_COOKIE,
        cookie.parse().context("failed to parse cookie")?,
    );

    return Ok((headers, Redirect::to(auth_url.as_ref())));
}

async fn logout(
    State(store): State<MemoryStore>,
    TypedHeader(cookies): TypedHeader<headers::Cookie>,
) -> Result<impl IntoResponse, AppError> {
    let cookie = cookies
        .get(COOKIE_NAME)
        .context("unexpected error getting cookie name")?;

    let session = match store
        .load_session(cookie.to_string())
        .await
        .context("failed to load session")?
    {
        Some(s) => s,
        // No session active, just redirect
        None => return Ok(Redirect::to("/")),
    };

    store
        .destroy_session(session)
        .await
        .context("failed to destroy session")?;

    Ok(Redirect::to("/"))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct AuthRequest {
    code: String,
    state: String,
}

async fn csrf_token_validation_workflow(
    auth_request: &AuthRequest,
    cookies: &headers::Cookie,
    store: &MemoryStore,
) -> Result<(), AppError> {
    // Extract the cookie from the request
    let cookie = cookies
        .get(COOKIE_NAME)
        .context("unexpected error getting cookie name")?
        .to_string();

    // Load the session
    let session = match store
        .load_session(cookie)
        .await
        .context("failed to load session")?
    {
        Some(session) => session,
        None => return Err(anyhow!("Session not found").into()),
    };

    // Extract the CSRF token from the session
    let stored_csrf_token = session
        .get::<CsrfToken>(CSRF_TOKEN)
        .context("CSRF token not found in session")?
        .to_owned();

    // Cleanup the CSRF token session
    store
        .destroy_session(session)
        .await
        .context("Failed to destroy old session")?;

    // Validate CSRF token is the same as the one in the auth request
    if *stored_csrf_token.secret() != auth_request.state {
        return Err(anyhow!("CSRF token mismatch").into());
    }

    return Ok(());
}

async fn login_authorized(
    Query(query): Query<AuthRequest>,
    State(store): State<MemoryStore>,
    State(oauth_client): State<BasicClient>,
    TypedHeader(cookies): TypedHeader<headers::Cookie>,
) -> Result<impl IntoResponse, AppError> {
    csrf_token_validation_workflow(&query, &cookies, &store).await?;

    // Get an auth token
    let token = oauth_client
        .exchange_code(AuthorizationCode::new(query.code.clone()))
        .request_async(async_http_client)
        .await
        .context("failed in sending request request to authorization server")?;

    // Fetch user data from discord
    let client = reqwest::Client::new();
    let user_data: User = client
        // https://discord.com/developers/docs/resources/user#get-current-user
        .get("https://discordapp.com/api/users/@me")
        .bearer_auth(token.access_token().secret())
        .send()
        .await
        .context("failed in sending request to target Url")?
        .json::<User>()
        .await
        .context("failed to deserialize response as JSON")?;

    // Create a new session filled with user data
    let mut session = Session::new();
    session
        .insert("user", &user_data)
        .context("failed in inserting serialized value into session")?;

    // Store session and get corresponding cookie
    let cookie = store
        .store_session(session)
        .await
        .context("failed to store session")?
        .context("unexpected error retrieving cookie value")?;

    // Build the cookie
    let cookie = format!("{COOKIE_NAME}={cookie}; SameSite=Lax; HttpOnly; Secure; Path=/");

    // Set cookie
    let mut headers = HeaderMap::new();
    headers.insert(
        SET_COOKIE,
        cookie.parse().context("failed to parse cookie")?,
    );

    return Ok((headers, Redirect::to("/")));
}

struct AuthRedirect;

impl IntoResponse for AuthRedirect {
    fn into_response(self) -> Response {
        return Redirect::temporary("/auth/discord").into_response();
    }
}

impl<S> FromRequestParts<S> for User
where
    MemoryStore: FromRef<S>,
    S: Send + Sync,
{
    // If anything goes wrong or no session is found, redirect to the auth page
    type Rejection = AuthRedirect;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let store = MemoryStore::from_ref(state);

        let cookies = parts
            .extract::<TypedHeader<headers::Cookie>>()
            .await
            .map_err(|e| match *e.name() {
                header::COOKIE => match e.reason() {
                    TypedHeaderRejectionReason::Missing => AuthRedirect,
                    _ => panic!("unexpected error getting Cookie header(s): {e}"),
                },
                _ => panic!("unexpected error getting cookies: {e}"),
            })?;

        let session_cookie = cookies.get(COOKIE_NAME).ok_or(AuthRedirect)?;

        let session = store
            .load_session(session_cookie.to_string())
            .await
            .unwrap()
            .ok_or(AuthRedirect)?;

        let user = session.get::<User>("user").ok_or(AuthRedirect)?;

        return Ok(user);
    }
}

impl<S> OptionalFromRequestParts<S> for User
where
    MemoryStore: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &S,
    ) -> Result<Option<Self>, Self::Rejection> {
        match <User as FromRequestParts<S>>::from_request_parts(parts, state).await {
            Ok(res) => Ok(Some(res)),
            Err(AuthRedirect) => Ok(None),
        }
    }
}

// Use anyhow, define error and enable '?'
// For a simplified example of using anyhow in axum check /examples/anyhow-error-response
#[derive(Debug)]
struct AppError(anyhow::Error);

// Tell axum how to convert `AppError` into a response.
impl IntoResponse for AppError {
    fn into_response(self) -> axum::http::Response<axum::body::Body> {
        tracing::error!("application error: {:#}", self.0);

        return (StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong").into_response();
    }
}

// This enables using `?` on functions that return `Result<_, anyhow::Error>` to turn them into
// `Result<_, AppError>`. That way you don't need to do that manually.
impl<E> From<E> for AppError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        return Self(err.into());
    }
}

/// background thread that cleans up old squittal instances
async fn instance_cleanup() {
    loop {
        let instances = docker::get_instances().await;
        if instances.is_err() {
            eprintln!("failed to perform cleanup: {}", instances.unwrap_err());
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }

        let instances = instances.unwrap();

        let now: std::time::SystemTime = std::time::SystemTime::now();
        for instance in instances {
            let diff: Duration = now
                .duration_since(instance.created_on)
                .expect("failed to diff time");

            let name: String = if instance.name.starts_with("/") {
                instance.name[1..].to_string()
            } else {
                instance.name
            };

            if diff >= Duration::from_secs(60 * 60 * 2) {
                println!("killing {}, diff={:?}", name, diff);
                let result = docker::remove_container(&name).await;
                if result.is_err() {
                    eprintln!(
                        "failed to remove instance {}: {}",
                        name,
                        result.unwrap_err()
                    );
                }
            }
        }

        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}
