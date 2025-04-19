use std::time::{Duration, SystemTime, UNIX_EPOCH};

use atproto_jetstream::{
    connection::{Connection, Cursor, Options, bluesky_instances::US_EAST_1},
    consumer::{Consumer, FlattenedCommitEvent, ProcessEffect, process_message},
    multi_consumer,
};
use atrium_api::types::{
    Collection,
    string::{Datetime, Did},
};
use tracing::error;

use crate::{
    lexicons::xyz::statusphere::{Status, status::RecordData},
    store::{Error as StoreError, Status as StoreStatus, StatusStore},
};

impl TryFrom<FlattenedCommitEvent<RecordData>> for StoreStatus {
    type Error = StoreError;

    fn try_from(
        FlattenedCommitEvent {
            did,
            collection,
            rkey,
            record: RecordData { status, created_at },
            ..
        }: FlattenedCommitEvent<RecordData>,
    ) -> Result<Self, Self::Error> {
        Ok(Self {
            uri: format!("at://{did}/{collection}/{rkey}"),
            author_did: Did::new(did).map_err(StoreError::InvalidDid)?,
            status,
            created_at,
            indexed_at: Datetime::now(),
        })
    }
}

#[derive(Debug)]
struct StatusConsumer {
    store: StatusStore,
}

impl Consumer<RecordData, StoreError> for StatusConsumer {
    async fn consume(&self, message: FlattenedCommitEvent<RecordData>) -> Result<(), StoreError> {
        let store_status = StoreStatus::try_from(message)?;
        self.store.insert(store_status).await?;
        Ok(())
    }
}

pub async fn ingester(status_store: StatusStore) -> Result<(), crate::error::Error> {
    // needed for tungstenite
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("failed to install default crypto provider");

    let mut connection = Connection::new(
        Options::new(US_EAST_1)
            .wanted_collections([Status::NSID.to_owned()])
            .compress(true),
    );

    let status_multi_consumer = multi_consumer!(
        StatusMultiConsumer<StoreError> {
            Status::NSID => RecordData => StatusConsumer = StatusConsumer { store: status_store.clone() }
        }
    );

    // cursor into the stream
    let thirty_minutes_ago = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time error")
        - Duration::from_secs(30 * 60);
    let cursor = Cursor::from(thirty_minutes_ago.as_micros() as u64);

    let mut message_rx = connection
        .take_message_rx()
        .expect("message_rx already taken");

    // spawn the message loop
    tokio::spawn(async move {
        while let Some(message) = message_rx.recv().await {
            match process_message(&status_multi_consumer, message).await {
                Err(e) => {
                    error!("error during message processing: {e}");
                }
                Ok(ProcessEffect::Closed(err_message)) => {
                    error!(
                        "Jetstream connection closed{}",
                        err_message
                            .map(|em| format!(": {}", em.to_string()))
                            .unwrap_or("".to_owned())
                    );
                    break;
                }
                Ok(
                    ProcessEffect::Ignored
                    | ProcessEffect::ProcessedAccount
                    | ProcessEffect::ProcessedIdentity
                    | ProcessEffect::ProcessedCommit,
                ) => {}
            }
        }
    });

    // spin up the Jetstream connection
    tokio::spawn(async move {
        if let Err(e) = connection.connect(cursor).await {
            error!("Jetstream connection failed: {e}");
        }
    });

    Ok(())
}
