use anyhow::{Context, Error, bail};
use async_channel::{Receiver, Sender, unbounded};
use reqwest::Client;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue}; // 添加 HeaderValue 和 HeaderName
use reqwest_eventsource::{Event, EventSource};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fmt::{Debug, Display, Formatter};
use std::str::FromStr;
use tokio::select;
use tracing::{debug, warn, info};
use futures::StreamExt; // 导入 StreamExt
use serde_yaml; // 导入 serde_yaml

#[derive(Clone, Debug, Deserialize)] // 添加 Clone
pub struct Config {
    pub key: String,
    pub agent_id: String,
    pub hy_user: String,
    pub hy_token: String,
    pub port: u16,
    pub conversation_id: String,  // 使用字符串来存储 UUID
}

// Yuanbao 结构体，用于与 API 交互
#[derive(Clone)]
pub struct Yuanbao {
    config: Config,
    client: Client,
}

impl Yuanbao {
    // 创建一个新的 Yuanbao 实例
    pub fn new(config: Config) -> Yuanbao {
        let headers = Self::make_headers(&config);
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .unwrap();
        Yuanbao { config, client }
    }

    // 创建一个新的对话，返回固定的 conversation_id
    pub async fn create_conversation(&self) -> anyhow::Result<String> {
        // 使用配置文件中的固定对话 ID
        Ok(self.config.conversation_id.clone())  // 返回 UUID 字符串
    }

    // 创建聊天完成请求
    pub async fn create_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> anyhow::Result<Receiver<ChatCompletionEvent>> {
        info!("Using fixed conversation");

        // 获取固定的 conversation_id
        let conversation_id = self
            .create_conversation()
            .await
            .context("cannot get conversation ID")?;

        info!("Using fixed conversation ID: {}", conversation_id);

        let prompt = request.messages.to_string();
        let body = json!({
            "model": "gpt_175B_0404",
            "prompt": prompt,
            "plugin": "Adaptive",
            "displayPrompt": prompt,
            "displayPromptType": 1,
            "options": {"imageIntention": {"needIntentionModel": true, "backendUpdateFlag": 2, "intentionStatus": true}},
            "multimedia": [],
            "agentId": self.config.agent_id,
            "supportHint": 1,
            "version": "v2",
            "chatModelId": request.chat_model.as_yuanbao_string(),
        });

        let formatted_url = format!("https://yuanbao.tencent.com/api/chat/{}", conversation_id);

        let mut sse = EventSource::new(self.client.post(&formatted_url).json(&body))
            .context("failed to get next event")?;

        let (sender, receiver) = unbounded::<ChatCompletionEvent>();
        tokio::spawn(async move {
            if let Err(err) = Self::process_sse(&mut sse, sender).await {
                warn!("SSE exit: {:#}", err);
            }
        });

        Ok(receiver)
    }

    // 处理 SSE 事件流
    async fn process_sse(
        sse: &mut EventSource,
        sender: Sender<ChatCompletionEvent>,
    ) -> anyhow::Result<()> {
        let mut finish_reason = "stop".to_string();
        loop {
            let event;
            select! {
                Some(e)=sse.next()=>{
                    event=e;
                },
                else => {
                    info!("Stream ended (pattern else)");
                    break;
                }
            }
            match event {
                Ok(Event::Open) => {}
                Ok(Event::Message(message)) => {
                    if message.event != "message" {
                        continue;
                    }
                    let res = serde_json::from_str::<serde_json::Value>(&message.data);
                    let value = match res {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    match value["type"].as_str().unwrap_or("") {
                        "think" => {
                            let content = value["content"].as_str().unwrap_or("");
                            if content.is_empty() {
                                continue;
                            }
                            sender
                                .send(ChatCompletionEvent::Message(ChatCompletionMessage {
                                    r#type: ChatCompletionMessageType::Think,
                                    text: content.to_string(),
                                }))
                                .await?;
                        }
                        "text" => {
                            let msg = value["msg"].as_str().unwrap_or("");
                            sender
                                .send(ChatCompletionEvent::Message(ChatCompletionMessage {
                                    r#type: ChatCompletionMessageType::Msg,
                                    text: msg.to_string(),
                                }))
                                .await?;
                        }
                        _ => {
                            let stop_reason = value["stopReason"].as_str().unwrap_or("");
                            if !stop_reason.is_empty() {
                                finish_reason = stop_reason.to_string();
                            }
                        }
                    }
                    debug!(?message, "Event message");
                }
                Err(err) => match err {
                    reqwest_eventsource::Error::StreamEnded => {
                        info!("Stream ended");
                        break;
                    }
                    _ => {
                        return Err(anyhow!("stream error {}", err));
                    }
                },
            }
        }
        sender
            .send(ChatCompletionEvent::Finish(finish_reason))
            .await?;
        Ok(())
    }

    // 创建 HTTP 请求的头部
    fn make_headers(config: &Config) -> HeaderMap {
        HeaderMap::from_iter(vec![
            (
                HeaderName::from_str("Cookie").unwrap(),
                HeaderValue::from_str(&format!(
                    "hy_source=web; hy_user={}; hy_token={}",
                    config.hy_user, config.hy_token
                ))
                .unwrap(),
            ),
            (
                HeaderName::from_str("Origin").unwrap(),
                HeaderValue::from_str("https://yuanbao.tencent.com").unwrap(),
            ),
            (
                HeaderName::from_str("Referer").unwrap(),
                HeaderValue::from_str(&format!(
                    "https://yuanbao.tencent.com/chat/{}",
                    config.agent_id
                ))
                .unwrap(),
            ),
            (
                HeaderName::from_str("X-Agentid").unwrap(),
                HeaderValue::from_str(&config.agent_id).unwrap(),
            ),
            (
                HeaderName::from_str("User-Agent").unwrap(),
                HeaderValue::from_str(
                    "Mozilla/5.0 (Windows NT 10.0; Win64; x64)\
                     AppleWebKit/537.36 (KHTML, like Gecko) Chrome/134.0.0.0 Safari/537.36",
                )
                .unwrap(),
            ),
        ])
    }
}
