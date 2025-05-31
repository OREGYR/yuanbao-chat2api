use crate::service::{Config, Handler, Service};
use anyhow::Context;
use axum::Router;
use axum::extract::State;
use axum::routing::{get, post};
use tokio::net::TcpListener;
use tracing::{info, instrument};
use tracing_subscriber::Layer;
use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[instrument]
#[tokio::main]
async fn main() {
    // Set up tracing for logging and debugging
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_filter(tracing::Level::INFO)
                .with_filter(filter_fn(|meta| {
                    meta.target().starts_with("yuanbao_chat2api")
                })),
        )
        .init();

    // Load configuration from config.yml
    let config: Config = tokio::fs::read_to_string("config.yml")
        .await
        .context("cannot get config.yml")
        .unwrap()
        .parse()
        .context("cannot parse config.yml")
        .unwrap();
    
    // Retrieve port number from config
    let port = config.port;

    // Create the service with the loaded configuration
    let service = Service::new(config);

    // Set up routes and the Axum application
    let app = Router::new()
        .route("/v1/models", get(Handler::models))
        .route("/v1/chat/completions", post(Handler::chat_completions))
        .with_state(service);

    // Bind to the configured port
    let listener = TcpListener::bind(format!("0.0.0.0:{}", port))
        .await
        .unwrap();
    
    // Log that the service has started
    info!("Launched the service on :{port}");

    // Start the server
    axum::serve(listener, app).await.unwrap();
}
