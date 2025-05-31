mod service; // 引入 service.rs 模块
use crate::service::{Config, Handler, Service};
use anyhow::Context;
use axum::Router;
use axum::extract::State;
use axum::routing::{get, post};
use tokio::net::TcpListener;
use tracing::{info, instrument};
use tracing_subscriber::{filter::LevelFilter, fmt::layer, util::SubscriberInitExt};
use tracing_subscriber::filter::filter_fn;

#[instrument]
#[tokio::main]
async fn main() {
    // 配置 tracing 日志
    tracing_subscriber::registry()
        .with(
            layer()
                .with_filter(LevelFilter::INFO) // 使用 LevelFilter::INFO
                .with_filter(filter_fn(|meta| {
                    meta.target().starts_with("yuanbao_chat2api")
                }))
        )
        .init();

    // 读取配置文件
    let config: Config = tokio::fs::read_to_string("config.yml")
        .await
        .context("cannot get config.yaml")
        .unwrap()
        .parse()
        .context("cannot parse config.yaml")
        .unwrap();
    
    let port = config.port;
    let service = Service::new(config);
    let app = Router::new()
        .route("/v1/models", get(Handler::models))
        .route("/v1/chat/completions", post(Handler::chat_completions))
        .with_state(service);

    // 绑定端口并启动服务器
    let listener = TcpListener::bind(format!("0.0.0.0:{}", port))
        .await
        .unwrap();
    
    info!("Launched the service on :{port}");
    axum::serve(listener, app).await.unwrap();
}
