use crate::service::Config;
use anyhow::{Context, Error, anyhow, bail};
use async_channel::{Receiver, Sender, unbounded};
use axum::http::HeaderValue;
use futures_util::StreamExt;
use reqwest::Client;
use reqwest::header::{HeaderMap, HeaderName};
use reqwest_eventsource::{Event, EventSource};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fmt::{Debug, Display, Formatter};
use std::str::FromStr;
use std::time::Duration;
use tokio::select;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

static CREATE_URL: &str = "https://yuanbao.tencent.com/api/user/agent/conversation/create";
static CLEAR_URL: &str = "https://yuanbao.tencent.com/api/user/agent/conversation/v1/clear";
static CHAT_URL: &str = "https://yuanbao.tencent.com/api/chat/{}";

#[derive(Debug)]
pub enum ChatCompletionEvent {
    Message(ChatCompletionMessage),
    Error(Error),
    Finish(String),
}
#[derive(Debug)]
pub struct ChatCompletionMessage {
    pub r#type: ChatCompletionMessageType,
    pub text: String,
}
#[derive(Debug)]
pub enum ChatCompletionMessageType {
    Think,
    Msg,
}
pub struct ChatCompletionRequest {
    pub messages: ChatMessages,
    pub chat_model: ChatModel,
}
#[derive(Debug, Deserialize, Serialize)]
pub struct ChatMessages(pub Vec<ChatMessage>);
#[derive(Debug, Deserialize, Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: Option<String>,
    pub reasoning_content: Option<String>,
}
impl Display for ChatMessages {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let arr = &self.0;
        if arr.is_empty() {
            return Err(std::fmt::Error);
        }
        if arr.len() == 1 {
            write!(f, "{}", arr[0].content.as_ref().unwrap_or(&"".to_string()))?;
            return Ok(());
        }
        for item in arr {
            write!(
                f,
                "#[{}]\n{}\n\n",
                item.role.trim(),
                item.content.as_ref().unwrap_or(&"".to_string()).trim()
            )?;
        }
        Ok(())
    }
}
#[derive(Copy, Clone)]
pub enum ChatModel {
    DeepSeekV3,
    DeepSeekR1,
}
impl FromStr for ChatModel {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "deepseek-r1" => Ok(ChatModel::DeepSeekR1),
            "deepseek-v3" => Ok(ChatModel::DeepSeekV3),
            &_ => {
                bail!("invalid model")
            }
        }
    }
}
impl ChatModel {
    pub fn as_yuanbao_string(&self) -> String {
        match self {
            ChatModel::DeepSeekV3 => "deep_seek_v3",
            ChatModel::DeepSeekR1 => "deep_seek",
        }
        .to_string()
    }
    pub fn as_common_string(&self) -> String {
        match self {
            ChatModel::DeepSeekV3 => "deepseek-v3",
            ChatModel::DeepSeekR1 => "deepseek-r1",
        }
        .to_string()
    }
}

#[derive(Clone)]
pub struct Yuanbao {
    config: Config,
    client: Client,
}
impl Yuanbao {
    pub fn new(config: Config) -> Yuanbao {
        let headers = Self::make_headers(&config);
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .unwrap();
        Yuanbao { config, client }
    }
    pub async fn create_conversation(&self) -> anyhow::Result<String> {
        let res: serde_json::Value = self
            .client
            .post(CREATE_URL)
            .json(&json!({"agentId":self.config.agent_id}))
            .send()
            .await
            .context("cannot send request")?
            .error_for_status()
            .context("error status code")?
            .json()
            .await
            .context("cannot parse json")?;
        let id = res["id"].as_str().context("id not in json")?;
        Ok(id.to_string())
    }
    pub async fn create_completion(
    &self,
    request: ChatCompletionRequest,
) -> anyhow::Result<Receiver<ChatCompletionEvent>> {
    use tracing::{info, warn};
    // 使用配置中提供的固定会话 ID
    let conversation_id = &self.config.conversation_id;
    info!("Using fixed conversation id: {}", conversation_id);
    // 构造请求体
    let prompt = request.messages.to_string();
    let body = json!({
        "model": "gpt_175B_0404",
        "prompt": prompt,
        "plugin": "Adaptive",
        "displayPrompt": prompt,
        "displayPromptType": 1,
        "options": {
            "imageIntention": {
                "needIntentionModel": true,
                "backendUpdateFlag": 2,
                "intentionStatus": true
            }
        },
        "multimedia": [],
        "agentId": self.config.agent_id,
        "supportHint": 1,
        "version": "v2",
        "chatModelId": request.chat_model.as_yuanbao_string(),
    });
    // 替换 URL 中的占位符
    let formatted_url = CHAT_URL.replace("{}", conversation_id);
    // 发起 SSE 请求
    let mut sse = EventSource::new(self.client.post(&formatted_url).json(&body))
        .context("failed to get next event")?;
    // 管道返回
    let (sender, receiver) = async_channel::unbounded::<ChatCompletionEvent>();
    tokio::spawn(async move {
        if let Err(err) = Self::process_sse(&mut sse, sender).await {
            warn!("SSE exit: {:#}", err);
        }
    });
    Ok(receiver)
}
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
