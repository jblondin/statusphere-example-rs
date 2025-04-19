mod error;
mod home;
mod ingester;
mod lexicons;
mod login;
mod oauth;
mod status;
mod store;

use std::{env, sync::Arc};

use atrium_api::types::string::Did;
use axum::{
    Router, middleware,
    routing::{get, post},
};
use minijinja::Environment;
use oauth::DidResolver;
use serde::{Deserialize, Serialize};
use store::{OAuthSessionStore, OAuthStateStore, StatusStore};
use tower_http::services::ServeDir;
use tower_sessions::{
    Expiry, SessionManagerLayer,
    cookie::{SameSite, time::Duration},
};
use tower_sessions_sqlx_store::{
    SqliteStore,
    sqlx::{self, Sqlite, SqlitePool, migrate::MigrateDatabase},
};
use tracing::info;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

use error::Error;
use home::home;
use login::{accept_login_form, login_form, logout, oauth_callback};
use status::post_status;

macro_rules! open_template {
    ($state:ident, $name:expr) => {
        $state
            .template_env
            .get_template($name)
            // panic, this is an unrecoverable error
            .expect(format!("missing {} template", $name).as_str())
    };
}
pub(crate) use open_template;

struct AppConfig {
    show_error_messages: bool,
}

struct AppState {
    template_env: Environment<'static>,
    oauth_client: oauth::Client,
    status_store: StatusStore,
    did_resolver: DidResolver,
    config: AppConfig,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ClientSession {
    did: Did,
}

// improve std::env::var error reporting
fn env_var_or_default(key: &'static str, default: impl AsRef<str>) -> anyhow::Result<String> {
    Ok(match env::var(key) {
        Ok(v) => v,
        Err(env::VarError::NotPresent) => default.as_ref().to_string(),
        Err(e) => Err(e)?,
    })
}

fn env_var_required(key: &'static str) -> anyhow::Result<String> {
    env::var(key).map_err(|e| anyhow::anyhow!("{e}: {key}"))
}

// connect to DB at URL (creating if not existing)
async fn db_connect(url: &str) -> Result<SqlitePool, sqlx::error::Error> {
    if !Sqlite::database_exists(url).await? {
        Sqlite::create_database(url).await?;
        info!("Database created at {url}");
    }
    let pool = SqlitePool::connect(url).await?;
    info!("Sqlite DB connected: {url}");
    Ok(pool)
}

fn initialize_templates<'a>() -> Environment<'a> {
    let mut template_env = Environment::new();
    template_env
        .add_template("layout", include_str!("../templates/layout.jinja"))
        .expect("missing jinja file");
    template_env
        .add_template("login", include_str!("../templates/login.jinja"))
        .expect("missing jinja file");
    template_env
        .add_template("home", include_str!("../templates/home.jinja"))
        .expect("missing jinja file");
    template_env
        .add_template("error", include_str!("../templates/error.jinja"))
        .expect("missing jinja file");
    template_env
}

async fn initialize_stores()
-> anyhow::Result<(StatusStore, SqliteStore, OAuthSessionStore, OAuthStateStore)> {
    // set up Sqlite DB connection pool
    let db_pool = db_connect(env_var_required("DATABASE_URL")?.as_str()).await?;

    let status_store = StatusStore::new(db_pool.clone(), "status")?;
    status_store.migrate().await?;
    let session_store = SqliteStore::new(db_pool.clone());
    session_store.migrate().await?;
    let oauth_session_store = OAuthSessionStore::new(db_pool.clone());
    oauth_session_store.migrate().await?;
    let oauth_state_store = OAuthStateStore::new(db_pool);
    oauth_state_store.migrate().await?;

    Ok((
        status_store,
        session_store,
        oauth_session_store,
        oauth_state_store,
    ))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(EnvFilter::from_default_env())
        .init();

    let template_env = initialize_templates();

    let (status_store, session_store, oauth_session_store, oauth_state_store) =
        initialize_stores().await?;

    //TODO: spawn clientsession cleanup task?
    // (https://github.com/maxcountryman/tower-sessions-stores/tree/main/sqlx-store#sqlite-example)

    let app_config = AppConfig {
        show_error_messages: env_var_or_default("SHOW_ERRORS", "false")?.parse()?,
    };

    // HTTP client used by oauth client and DID resolver
    let http_client = Arc::new(oauth::http_client());

    let oauth_client = oauth::client(
        Arc::clone(&http_client),
        oauth_session_store,
        oauth_state_store,
    )?;
    let did_resolver = oauth::did_resolver(Arc::clone(&http_client));

    // common app state
    let app_state = Arc::new(AppState {
        template_env,
        oauth_client,
        status_store: status_store.clone(),
        did_resolver,
        config: app_config,
    });

    // fire up ingester
    ingester::ingester(status_store).await?;
    info!("Ingester started");

    // user session management layer
    let sesssion_layer = SessionManagerLayer::new(session_store)
        .with_secure(false)
        .with_expiry(Expiry::OnInactivity(Duration::weeks(1)))
        // the `/oauth/callback` redirect doesn't set a session cookie unless this is set to Lax
        .with_same_site(SameSite::Lax);

    let app = Router::new()
        .route("/login", get(login_form).post(accept_login_form))
        .route("/oauth/callback", get(oauth_callback))
        .route("/logout", post(logout))
        .route("/status", post(post_status))
        .route("/", get(home))
        .layer(sesssion_layer)
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&app_state),
            error::error_middleware,
        ))
        .nest_service("/assets", ServeDir::new("assets"))
        .with_state(app_state);

    let addr = "0.0.0.0:8081";
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("Server bound on {addr}");
    axum::serve(listener, app).await?;

    Ok(())
}
