use std::sync::Arc;

use atrium_api::{
    com::atproto,
    types::{
        Collection,
        string::{Datetime, RecordKey, Tid},
    },
};
use axum::{
    Form,
    extract::State,
    response::{IntoResponse, Redirect, Response},
};
use serde::Deserialize;
use tower_sessions::Session;

use crate::{
    AppState,
    error::Error,
    lexicons::{
        self,
        xyz::statusphere::{self, Status},
    },
    oauth::{agent_did, session_agent},
};

#[derive(Deserialize, Debug)]
pub struct LoginInput {
    status: String,
}

#[axum::debug_handler]
pub async fn post_status(
    State(state): State<Arc<AppState>>,
    session: Session,
    Form(input): Form<LoginInput>,
) -> Result<Response, Error> {
    let Some(agent) = session_agent(state.as_ref(), &session).await? else {
        return Ok(Redirect::to("/?error=logged_out").into_response());
    };

    let did = agent_did(&agent).await;
    let rkey = Tid::now(
        0.try_into()
            .expect("unexpected clock ID conversion failure"),
    )
    .to_string();

    let status_record_data = statusphere::status::RecordData {
        created_at: Datetime::now(),
        status: input.status,
    };

    let input_data = atproto::repo::create_record::InputData {
        collection: Status::NSID
            .parse()
            .expect("NSID is generated, should never fail to parse"),
        record: lexicons::record::KnownRecord::from(status_record_data.clone()).into(),
        repo: did.clone().into(),
        rkey: Some(RecordKey::new(rkey.to_owned()).expect("unexpected record key failure")),
        swap_commit: None,
        validate: None,
    };
    // TOOD: validate input data

    // add to the repo
    let record = agent
        .api
        .com
        .atproto
        .repo
        .create_record(input_data.into())
        .await?;

    // also go aheard and add to the DB so the user sees their update immediately
    state
        .status_store
        .insert(crate::store::Status {
            uri: record.data.uri,
            author_did: did,
            status: status_record_data.status,
            created_at: status_record_data.created_at,
            indexed_at: Datetime::now(),
        })
        .await?;

    Ok(Redirect::to("/").into_response())
}
