use std::sync::Arc;

use atrium_api::{agent::SessionManager, types::string::Handle};
use atrium_oauth::CallbackParams;
use axum::{
    Form,
    extract::{Query, State},
    response::{Html, IntoResponse, Redirect, Response},
};
use minijinja::context;
use serde::Deserialize;
use tower_sessions::Session;

use crate::{AppState, ClientSession, error::Error, oauth::OAuthAuthorize, open_template};

fn render_login_form(
    state: Arc<AppState>,
    error: Option<&'static str>,
) -> Result<Html<String>, crate::Error> {
    let template = open_template!(state, "login");

    let rendered = template.render(
        error
            .map(|e| context! { error => e })
            .unwrap_or_else(|| context! {}),
    )?;

    Ok(Html(rendered))
}

pub async fn login_form(State(state): State<Arc<AppState>>) -> Result<Html<String>, crate::Error> {
    render_login_form(state, None)
}

#[derive(Deserialize, Debug)]
pub struct LoginInput {
    handle: String,
}

pub async fn accept_login_form(
    State(state): State<Arc<AppState>>,
    Form(input): Form<LoginInput>,
) -> Result<Response, crate::Error> {
    // check handle validity
    if let Err(error) = Handle::new(input.handle.clone()) {
        return render_login_form(state, Some(error)).map(|form| form.into_response());
    }

    let redirect_url = state
        .oauth_client
        .oauth_authorize(input.handle.as_str())
        .await?;

    Ok(Redirect::to(&redirect_url).into_response())
}

pub async fn oauth_callback(
    State(state): State<Arc<AppState>>,
    Query(params): Query<CallbackParams>,
    session: Session,
) -> Result<Response, Error> {
    let (oauth_session, _oauth_state) = state.oauth_client.callback(params).await.unwrap();
    let did = oauth_session.did().await;
    let Some(did) = did else {
        return Err(Error::MissingDid);
    };

    let client_session: Option<ClientSession> = session.get("sid").await?;
    if client_session.is_some() {
        return Err(Error::SessionAlreadyExists);
    }
    session
        .insert("sid", ClientSession { did: did.clone() })
        .await?;

    Ok(Redirect::to("/").into_response())
}

pub async fn logout(session: Session) -> Result<Response, crate::Error> {
    session.delete().await?;

    Ok(Redirect::to("/").into_response())
}
