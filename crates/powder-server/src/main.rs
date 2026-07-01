#![forbid(unsafe_code)]

use std::{env, net::SocketAddr, path::PathBuf, sync::Arc};

use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Json, Router};
use serde::Serialize;
use tokio::net::TcpListener;

#[derive(Debug, Clone)]
struct AppState {
    config: Arc<Config>,
}

#[derive(Debug, Clone)]
struct Config {
    db_path: PathBuf,
    auth_mode: AuthMode,
    public_base_url: Option<String>,
    port: u16,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum AuthMode {
    SharedSecret,
    TailscaleHeader,
    Disabled,
}

impl Config {
    fn from_env() -> Result<Self, String> {
        let db_path = env::var("POWDER_DB_PATH").unwrap_or_else(|_| "/data/powder.db".to_string());
        let port = env::var("PORT")
            .unwrap_or_else(|_| "4000".to_string())
            .parse::<u16>()
            .map_err(|err| format!("PORT must be a u16: {err}"))?;
        let auth_mode = match env::var("POWDER_AUTH_MODE")
            .unwrap_or_else(|_| "shared-secret".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "shared-secret" => AuthMode::SharedSecret,
            "tailscale-header" => AuthMode::TailscaleHeader,
            "disabled" => AuthMode::Disabled,
            other => return Err(format!("invalid POWDER_AUTH_MODE: {other}")),
        };

        Ok(Self {
            db_path: PathBuf::from(db_path),
            auth_mode,
            public_base_url: env::var("POWDER_PUBLIC_BASE_URL").ok(),
            port,
        })
    }
}

#[derive(Debug, Serialize)]
struct Health {
    ok: bool,
    service: &'static str,
}

#[derive(Debug, Serialize)]
struct Ready {
    ok: bool,
    db_path: String,
    auth_mode: AuthMode,
}

#[derive(Debug, Serialize)]
struct Onboarding {
    needs_setup: bool,
    db_path: String,
    auth_mode: AuthMode,
    public_base_url: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = Config::from_env().map_err(|err| {
        tracing::error!("{err}");
        err
    })?;
    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    let state = AppState {
        config: Arc::new(config),
    };
    let app = app(state);

    tracing::info!("starting powder-server on {addr}");
    let listener = TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

fn app(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/api/v1/onboarding", get(onboarding))
        .with_state(state)
}

async fn healthz() -> Json<Health> {
    Json(Health {
        ok: true,
        service: "powder",
    })
}

async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    let Some(parent) = state.config.db_path.parent() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(Ready {
                ok: false,
                db_path: state.config.db_path.display().to_string(),
                auth_mode: state.config.auth_mode,
            }),
        );
    };

    let ok = parent.exists();
    let status = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(Ready {
            ok,
            db_path: state.config.db_path.display().to_string(),
            auth_mode: state.config.auth_mode,
        }),
    )
}

async fn onboarding(State(state): State<AppState>) -> Json<Onboarding> {
    Json(Onboarding {
        needs_setup: !state.config.db_path.exists(),
        db_path: state.config.db_path.display().to_string(),
        auth_mode: state.config.auth_mode,
        public_base_url: state.config.public_base_url.clone(),
    })
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}
