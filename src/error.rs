use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{Html, IntoResponse, Response},
};
use hickory_resolver::ResolveError;
use minijinja::context;
use thiserror::Error;
use tracing::error;

use crate::{AppState, open_template};

#[derive(Debug, Error)]
pub enum Error {
    // internal server errors
    #[error("oauth client creation: {0}")]
    OAuthClientCreation(atrium_oauth::Error),
    #[error("oauth authorize: {0}")]
    Authorize(atrium_oauth::Error),
    #[error("oauth restore: {0}")]
    Restore(atrium_oauth::Error),
    #[error("DNS resolver: {0}")]
    Resolver(#[from] ResolveError),
    #[error("template: {0}")]
    Template(#[from] minijinja::Error),
    #[error("session: {0}")]
    Session(#[from] tower_sessions::session::Error),
    #[error("session already exists")]
    SessionAlreadyExists,
    #[error("missing did")]
    MissingDid,
    #[error("atproto record create: {0}")]
    RecordCreate(
        #[from] atrium_api::xrpc::Error<atrium_api::com::atproto::repo::create_record::Error>,
    ),
    #[error("atproto record get: {0}")]
    RecordGet(#[from] atrium_api::xrpc::Error<atrium_api::com::atproto::repo::get_record::Error>),
    #[error("storage: {0}")]
    Storage(#[from] crate::store::Error),
    #[error("did resolution: {0}")]
    DidResolver(#[from] atrium_identity::Error),
    #[error("profile parsing: {0}")]
    ProfileParse(atrium_api::error::Error),
    #[error("jetstream connection: {0}")]
    JetstreamConnection(#[from] atproto_jetstream::connection::Error),
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        error!(%self);
        // kinda a lazy catch-all, but mostly correct
        let (status_code, message) = (StatusCode::SERVICE_UNAVAILABLE, self.to_string());

        (status_code, message).into_response()
    }
}

pub async fn error_middleware(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    let response = next.run(request).await;
    let status = response.status();
    if status.is_client_error() || status.is_server_error() {
        let template = open_template!(state, "error");

        let error_details = if state.config.show_error_messages {
            let (_, body) = response.into_parts();
            let message = axum::body::to_bytes(body, usize::MAX)
                .await
                .ok()
                .and_then(|body| str::from_utf8(&body).ok().map(ToOwned::to_owned))
                .unwrap_or_else(|| "Unable to display error message, see server logs.".to_owned());
            Some(message)
        } else {
            None
        };

        match template.render(context! {
            error_details => error_details
        }) {
            Ok(rendered) => (status, Html(rendered)).into_response(),
            Err(_) => (status, "Something went wrong!").into_response(),
        }
    } else {
        response
    }
}
