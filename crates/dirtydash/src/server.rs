use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
use axum::http::{header, HeaderValue, Response, StatusCode, Uri};
use axum::routing::get;
use axum::{Json, Router};
use include_dir::{include_dir, Dir};
use tokio::net::TcpListener;

use crate::cli::ServeArgs;
use crate::config::Config;
use crate::db::Database;

static DASHBOARD_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../dashboard/dist");

#[derive(Debug, Clone)]
struct AppState {
    db_path: PathBuf,
}

pub fn serve(db_path: PathBuf, args: ServeArgs) -> Result<()> {
    let runtime = tokio::runtime::Runtime::new().context("starting Tokio runtime")?;
    runtime.block_on(async move { serve_async(db_path, args).await })
}

async fn serve_async(db_path: PathBuf, args: ServeArgs) -> Result<()> {
    let state = Arc::new(AppState { db_path });
    let app = Router::new()
        .route("/api/summary", get(summary))
        .route("/api/sources", get(sources))
        .route("/api/sessions", get(sessions))
        .route("/api/days/:day/sessions", get(day_sessions))
        .route("/api/pricing", get(pricing))
        .route("/api/doctor", get(doctor))
        .fallback(static_asset)
        .with_state(state);

    let addr: SocketAddr = format!("{}:{}", args.host, args.port)
        .parse()
        .with_context(|| format!("parsing listen address {}:{}", args.host, args.port))?;
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding dashboard server to {addr}"))?;
    let local_addr = listener
        .local_addr()
        .context("reading dashboard listen address")?;
    let url = format!("http://{local_addr}");
    println!("dirtydash dashboard: {url}");
    if args.open {
        let _ = webbrowser::open(&url);
    }

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("running dashboard server")?;
    Ok(())
}

async fn summary(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let db = Database::open(&state.db_path).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let summary = db
        .dashboard_summary()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    serde_json::to_value(summary)
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn sources(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let db = Database::open(&state.db_path).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let sources = db
        .source_summaries()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    serde_json::to_value(sources)
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn sessions(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let db = Database::open(&state.db_path).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let sessions = db
        .sessions(100)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    serde_json::to_value(sessions)
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn day_sessions(
    State(state): State<Arc<AppState>>,
    AxumPath(day): AxumPath<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let db = Database::open(&state.db_path).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let sessions = db
        .sessions_for_day(&day, 100)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    serde_json::to_value(sessions)
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn pricing(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let db = Database::open(&state.db_path).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let pricing = db
        .list_pricing(None)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    serde_json::to_value(pricing)
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn doctor(State(state): State<Arc<AppState>>) -> Result<Json<serde_json::Value>, StatusCode> {
    let db = Database::open(&state.db_path).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let config = Config::default();
    let report = db
        .doctor(&config)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    serde_json::to_value(report)
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn static_asset(uri: Uri) -> Response<Body> {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    let file = DASHBOARD_DIR
        .get_file(path)
        .or_else(|| DASHBOARD_DIR.get_file("index.html"));

    match file {
        Some(file) => {
            let mime = mime_guess::from_path(file.path()).first_or_octet_stream();
            Response::builder()
                .status(StatusCode::OK)
                .header(
                    header::CONTENT_TYPE,
                    HeaderValue::from_str(mime.as_ref())
                        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
                )
                .body(Body::from(file.contents().to_vec()))
                .unwrap_or_else(|_| Response::new(Body::empty()))
        }
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("not found"))
            .unwrap_or_else(|_| Response::new(Body::empty())),
    }
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
