//! MCP front-ends: stdio and streamable HTTP.

pub mod server;

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::{stdio, StreamableHttpServerConfig, StreamableHttpService};
use rmcp::ServiceExt;
use tokio_util::sync::CancellationToken;

use crate::app::App;
use server::RecServer;

/// Serve MCP over stdin/stdout (how editors launch the server).
pub async fn serve_stdio(app: Arc<App>) -> Result<()> {
    let service = RecServer::new(app);
    let running = service
        .serve(stdio())
        .await
        .map_err(|e| anyhow::anyhow!("MCP stdio handshake failed: {e}"))?;
    tracing::info!("MCP server ready on stdio");
    running.waiting().await.context("MCP stdio server stopped")?;
    Ok(())
}

/// Serve MCP over streamable HTTP at `POST /mcp`.
/// Bind the HTTP port up-front so a port clash is reported to the caller
/// instead of vanishing into a background thread.
pub fn bind_http(addr: SocketAddr) -> Result<std::net::TcpListener> {
    let listener = std::net::TcpListener::bind(addr)
        .with_context(|| format!("cannot bind {addr}"))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

pub async fn serve_http(app: Arc<App>, addr: SocketAddr) -> Result<()> {
    let listener = bind_http(addr)?;
    serve_http_on(app, listener, addr).await
}

pub async fn serve_http_on(
    app: Arc<App>,
    listener: std::net::TcpListener,
    addr: SocketAddr,
) -> Result<()> {
    let ct = CancellationToken::new();
    let service = StreamableHttpService::new(
        move || Ok(RecServer::new(app.clone())),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default().with_cancellation_token(ct.child_token()),
    );

    let router = axum::Router::new()
        .route(
            "/health",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!({
                    "ok": true,
                    "name": "rec-r",
                    "version": env!("CARGO_PKG_VERSION"),
                }))
            }),
        )
        .nest_service("/mcp", service);

    let listener = tokio::net::TcpListener::from_std(listener)
        .with_context(|| format!("cannot use the listener for {addr}"))?;
    tracing::info!("MCP server listening on http://{addr}/mcp");

    let shutdown = {
        let ct = ct.clone();
        async move { ct.cancelled().await }
    };
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown)
        .await
        .context("HTTP server failed")?;
    Ok(())
}

/// Run the MCP HTTP server on a background thread with its own runtime.
pub fn spawn_http(
    app: Arc<App>,
    addr: SocketAddr,
) -> Result<std::thread::JoinHandle<()>> {
    let listener = bind_http(addr)?;
    let handle = std::thread::Builder::new()
        .name("mcp-http".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!("cannot start tokio runtime for MCP HTTP: {e}");
                    return;
                }
            };
            if let Err(e) = rt.block_on(serve_http_on(app, listener, addr)) {
                tracing::error!("MCP HTTP server stopped: {e:#}");
            }
        })
        .context("cannot spawn MCP HTTP thread")?;
    Ok(handle)
}

pub fn parse_addr(text: &str) -> Result<SocketAddr> {
    use std::net::ToSocketAddrs;
    let addr = text
        .to_socket_addrs()
        .with_context(|| format!("invalid address {text:?}"))?
        .next()
        .ok_or_else(|| anyhow::anyhow!("cannot resolve {text:?}"))?;
    Ok(addr)
}
/// Paste-ready MCP client config (`mcpServers` block) for the running executable.
///
/// The tray menu puts this on the clipboard so a client config can be pasted
/// without hand-editing paths.
pub fn client_config() -> String {
    let command = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "rec-r".to_string());
    // Hand-written JSON keeps the conventional key order (command, args); the
    // path is escaped by serde_json so backslashes/quotes survive a paste.
    let command = serde_json::to_string(&command).unwrap_or_else(|_| "\"rec-r\"".to_string());
    format!(
        "{{\n  \"mcpServers\": {{\n    \"rec-r\": {{\n      \"command\": {command},\n      \"args\": [\"serve\", \"--stdio\"]\n    }}\n  }}\n}}"
    )
}
