use std::sync::Arc;

use atrium_api::{agent::Agent, types::string::Did};
use atrium_identity::{
    did::{CommonDidResolver, CommonDidResolverConfig, DEFAULT_PLC_DIRECTORY_URL},
    handle::{AtprotoHandleResolver, AtprotoHandleResolverConfig, DnsTxtResolver},
};
use atrium_oauth::{
    AtprotoLocalhostClientMetadata, AuthorizeOptions, DefaultHttpClient, KnownScope, OAuthClient,
    OAuthClientConfig, OAuthResolverConfig, Scope,
};
use hickory_resolver::TokioResolver;
use tower_sessions::Session;
use tracing::info;

use crate::{
    AppState, ClientSession, Error,
    store::{OAuthSessionStore, OAuthStateStore},
};

pub struct HickoryDnsTxtResolver {
    resolver: TokioResolver,
}

impl HickoryDnsTxtResolver {
    fn new() -> Result<Self, Error> {
        Ok(Self {
            resolver: TokioResolver::builder_tokio()?.build(),
        })
    }
}

impl DnsTxtResolver for HickoryDnsTxtResolver {
    async fn resolve(
        &self,
        query: &str,
    ) -> core::result::Result<Vec<String>, Box<dyn std::error::Error + Send + Sync + 'static>> {
        Ok(self
            .resolver
            .txt_lookup(query)
            .await?
            .iter()
            .map(|txt| txt.to_string())
            .collect())
    }
}

pub type DidResolver = CommonDidResolver<DefaultHttpClient>;

pub type Config = OAuthClientConfig<
    OAuthStateStore,
    OAuthSessionStore,
    AtprotoLocalhostClientMetadata,
    CommonDidResolver<DefaultHttpClient>,
    AtprotoHandleResolver<HickoryDnsTxtResolver, DefaultHttpClient>,
>;

pub fn http_client() -> DefaultHttpClient {
    DefaultHttpClient::default()
}

pub fn did_resolver(http_client: Arc<DefaultHttpClient>) -> DidResolver {
    CommonDidResolver::new(CommonDidResolverConfig {
        plc_directory_url: DEFAULT_PLC_DIRECTORY_URL.to_string(),
        http_client: http_client,
    })
}

pub fn config(
    http_client: Arc<DefaultHttpClient>,
    oauth_session_store: OAuthSessionStore,
    oauth_state_store: OAuthStateStore,
) -> Result<Config, Error> {
    let config = OAuthClientConfig {
        client_metadata: AtprotoLocalhostClientMetadata {
            redirect_uris: Some(vec![String::from("http://127.0.0.1:8081/oauth/callback")]),
            scopes: Some(vec![
                Scope::Known(KnownScope::Atproto),
                Scope::Known(KnownScope::TransitionGeneric),
            ]),
        },
        keys: None,
        resolver: OAuthResolverConfig {
            did_resolver: did_resolver(Arc::clone(&http_client)),
            handle_resolver: AtprotoHandleResolver::new(AtprotoHandleResolverConfig {
                dns_txt_resolver: HickoryDnsTxtResolver::new()?,
                http_client: Arc::clone(&http_client),
            }),
            authorization_server_metadata: Default::default(),
            protected_resource_metadata: Default::default(),
        },
        // A store for saving state data while the user is being redirected to the authorization server.
        state_store: oauth_state_store,
        // A store for saving session data.
        session_store: oauth_session_store,
    };
    Ok(config)
}

pub type Client = OAuthClient<
    OAuthStateStore,
    OAuthSessionStore,
    CommonDidResolver<DefaultHttpClient>,
    AtprotoHandleResolver<HickoryDnsTxtResolver, DefaultHttpClient>,
>;

pub fn client(
    http_client: Arc<DefaultHttpClient>,
    oauth_session_store: OAuthSessionStore,
    oauth_state_store: OAuthStateStore,
) -> Result<Client, Error> {
    Ok(
        OAuthClient::new(config(http_client, oauth_session_store, oauth_state_store)?)
            .map_err(Error::OAuthClientCreation)?,
    )
}

pub trait OAuthAuthorize {
    async fn oauth_authorize(&self, handle: &str) -> Result<String, Error>;
}

impl OAuthAuthorize for Client {
    /// Initiates authorization of a handle. Returns the URL to visit for OAuth authorization.
    async fn oauth_authorize(&self, handle: &str) -> Result<String, Error> {
        let url = self
            .authorize(
                handle,
                AuthorizeOptions {
                    scopes: vec![
                        Scope::Known(KnownScope::Atproto),
                        Scope::Known(KnownScope::TransitionGeneric),
                    ],
                    ..Default::default()
                },
            )
            .await
            .map_err(Error::Authorize)?;
        Ok(url)
    }
}

pub type OAuthSession = atrium_oauth::OAuthSession<
    DefaultHttpClient,
    CommonDidResolver<DefaultHttpClient>,
    AtprotoHandleResolver<HickoryDnsTxtResolver, DefaultHttpClient>,
    OAuthSessionStore,
>;

pub type ATProtoAgent = Agent<OAuthSession>;

pub async fn session_agent(
    state: &AppState,
    session: &Session,
) -> Result<Option<ATProtoAgent>, Error> {
    let client_session: Option<ClientSession> = session.get("sid").await?;
    let oauth_session = match client_session {
        Some(cs) => match state.oauth_client.restore(&cs.did).await {
            Ok(session) => {
                let agent = Agent::new(session);
                info!("Restored session agent for user: {:?}", agent.did().await);
                Some(agent)
            }
            // ideally we'd want to inspect the SessionRegistry error to make sure it's a
            // 'not found' error, but that type isn't visible
            Err(e @ atrium_oauth::Error::SessionRegistry(_)) => {
                info!("No oauth session found for user {}: {e}", cs.did.as_str());
                None
            }
            Err(e) => return Err(Error::Restore(e)),
        },
        None => {
            info!("No user session found");
            None
        }
    };
    Ok(oauth_session)
}

pub async fn agent_did(agent: &ATProtoAgent) -> Did {
    agent.did().await.expect("agent should always have Did")
}
