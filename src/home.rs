use std::sync::Arc;

use atrium_api::{
    com::atproto::repo,
    types::{
        TryFromUnknown,
        string::{Datetime, Did, Nsid, RecordKey},
    },
};
use atrium_common::resolver::Resolver;
use axum::{
    extract::{Query, State},
    response::{Html, IntoResponse, Response},
};
use chrono::Local;
use minijinja::context;
use serde::{Deserialize, Serialize};
use tower_sessions::Session;

use crate::{
    AppState,
    error::Error,
    oauth::{DidResolver, agent_did, session_agent},
    open_template,
};

const STATUS_OPTIONS: [&'static str; 28] = [
    "👍",
    "👎",
    "💙",
    "🥹",
    "😧",
    "😤",
    "🙃",
    "😉",
    "😎",
    "🤓",
    "🤨",
    "🥳",
    "😭",
    "😤",
    "🤯",
    "🫡",
    "💀",
    "✊",
    "🤘",
    "👀",
    "🧠",
    "👩‍💻",
    "🧑‍💻",
    "🥷",
    "🧌",
    "🦋",
    "🚀",
    "🦀",
];

//TODO: memoize calls to this so we don't have to use resolver each time. either in-memory hashmap
// or another sqlite store would be helpful
async fn resolve_into_handle(resolver: &DidResolver, author_did: &Did) -> Result<String, Error> {
    let akas = resolver.resolve(author_did).await?.also_known_as;
    Ok(match akas {
        None => author_did.as_str().to_owned(),
        Some(akas) if akas.is_empty() => author_did.as_str().to_owned(),
        Some(akas) => format!("@{}", akas[0].replace("at://", "")),
    })
}

fn choose_date<'a>(created_at: &'a Datetime, indexed_at: &'a Datetime) -> &'a Datetime {
    if created_at < indexed_at {
        created_at
    } else {
        indexed_at
    }
}

fn display_date(dt: &Datetime) -> String {
    chrono::DateTime::<Local>::from(dt.as_ref().clone())
        .date_naive()
        .to_string()
}

#[derive(Debug, Deserialize)]
pub struct HomeQuery {
    error: Option<HomeError>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HomeError {
    LoggedOut,
}

pub async fn home(
    State(state): State<Arc<AppState>>,
    Query(home_query): Query<HomeQuery>,
    session: Session,
) -> Result<Response, Error> {
    let maybe_agent = session_agent(state.as_ref(), &session).await?;

    // fetch statuses from any user from DB
    let mut statuses = state.status_store.fetch_n(None, 10).await?;
    let user_status = match &maybe_agent {
        Some(agent) => state
            .status_store
            .fetch_one(Some(agent_did(agent).await))
            .await?
            .map(|s| s.status),
        None => None,
    };

    // fetch profile
    #[derive(Debug, Deserialize, Serialize)]
    #[serde(rename_all(deserialize = "camelCase"))]
    struct Profile {
        display_name: String,
    }
    let profile = match &maybe_agent {
        Some(agent) => {
            let object_data = agent
                .api
                .com
                .atproto
                .repo
                .get_record(
                    repo::get_record::ParametersData {
                        cid: None,
                        collection: Nsid::new("app.bsky.actor.profile".to_owned())
                            .expect("unexpected Nsid failure"),
                        repo: atrium_api::types::string::AtIdentifier::Did(agent_did(agent).await),
                        rkey: RecordKey::new("self".to_owned())
                            .expect("unexpected record key failure"),
                    }
                    .into(),
                )
                .await?
                .data
                .value;
            Some(Profile::try_from_unknown(object_data).map_err(Error::ProfileParse)?)
        }
        None => None,
    };

    // map DIDs into handles
    let mut handles = vec![];
    for status in &statuses {
        handles.push(resolve_into_handle(&state.did_resolver, &status.author_did).await?);
    }

    #[derive(Serialize)]
    struct StatusView {
        status: String,
        handle: String,
        date: String,
    }

    let status_views = statuses
        .drain(..)
        .zip(handles.drain(..))
        .map(|(status, handle)| StatusView {
            status: status.status,
            handle,
            date: display_date(choose_date(&status.created_at, &status.indexed_at)),
        })
        .collect::<Vec<_>>();

    let template = open_template!(state, "home");

    let rendered = template.render(context! {
        statuses => status_views,
        profile => profile,
        error => home_query.error,
        user_status => user_status,
        status_options => STATUS_OPTIONS,
        today => display_date(&Datetime::now())
    })?;

    Ok(Html(rendered).into_response())
}
