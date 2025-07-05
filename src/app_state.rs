use async_session::MemoryStore;
use axum::extract::FromRef;
use oauth2::basic::BasicClient;

use crate::proxy;

#[derive(Clone)]
pub struct AppState {
    pub store: MemoryStore,
    pub oauth_client: BasicClient,
    pub client: proxy::Client,
}

impl FromRef<AppState> for MemoryStore {
    fn from_ref(state: &AppState) -> Self {
        return state.store.clone();
    }
}

impl FromRef<AppState> for BasicClient {
    fn from_ref(state: &AppState) -> Self {
        return state.oauth_client.clone();
    }
}
