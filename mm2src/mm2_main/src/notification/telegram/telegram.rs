use crate::mm2::lp_message_service::{MessageResult, MessageServiceTraits};
use async_trait::async_trait;
use derive_more::Display;
use mm2_net::transport::{post_json, SlurpError};
use std::collections::HashMap;

pub const TELEGRAM_BOT_API_ENDPOINT: &str = "https://api.telegram.org/bot";

pub type ChatIdRegistry = HashMap<String, String>;

#[derive(Debug, Deserialize)]
pub struct SendMessageResponse {
    pub ok: bool,
}

#[derive(Debug, Deserialize, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum TelegramError {
    #[display(fmt = "{}", _0)]
    RequestError(SlurpError),
}

impl From<SlurpError> for TelegramError {
    fn from(err: SlurpError) -> Self { TelegramError::RequestError(err) }
}

#[derive(Clone)]
pub struct TgClient {
    url: String,
    api_key: String,
    chat_id_registry: ChatIdRegistry,
}

impl TgClient {
    pub fn new(api_key: String, url: Option<String>, chat_id_registry: ChatIdRegistry) -> Self {
        TgClient {
            url: url.unwrap_or_else(|| TELEGRAM_BOT_API_ENDPOINT.to_string()),
            api_key,
            chat_id_registry,
        }
    }
}

#[async_trait]
impl MessageServiceTraits for TgClient {
    async fn send_message(&self, message: String, room_id: &str, disable_notification: bool) -> MessageResult<bool> {
        let uri = self.url.to_owned() + self.api_key.as_str() + "/sendMessage";
        if let Some(chat_id) = self.chat_id_registry.get(room_id) {
            let json = json!({ "chat_id": chat_id, "text": message, "disable_notification": disable_notification });
            let result = post_json::<SendMessageResponse>(uri.as_str(), json.to_string())
                .await
                .map_err(|e| TelegramError::RequestError(e.into_inner()))?;
            return Ok(result.ok);
        }
        Ok(false)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod telegram_tests {
    use crate::mm2::lp_message_service::telegram::{ChatIdRegistry, TgClient};
    use crate::mm2::lp_message_service::MessageServiceTraits;
    use common::block_on;
    use std::env::var;

    #[test]
    fn test_send_message() {
        if let Ok(api_key) = var("TELEGRAM_API_KEY") {
            let chat_registry: ChatIdRegistry = vec![("RustTestChatId".to_string(), "-645728650".to_string())]
                .into_iter()
                .collect();
            let tg_client = TgClient::new(api_key, None, chat_registry);
            let resp = block_on(tg_client.send_message("Hello from rust".to_string(), "RustTestChatId", true)).unwrap();
            assert!(resp);
        }
    }

    /// The bot token is a path segment of the request URL. A failed send names the endpoint host and carries no
    /// part of the token (CRD ch.35 R35.1.17).
    #[test]
    fn a_failed_send_never_shows_the_bot_token() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nnot json");
        });
        let chat_registry: ChatIdRegistry = vec![("Room".to_string(), "-1".to_string())].into_iter().collect();
        let tg_client = TgClient::new(
            "123456:AAHbotTOKEN".to_string(),
            Some(format!("http://127.0.0.1:{port}/bot")),
            chat_registry,
        );
        let err = block_on(tg_client.send_message("hi".to_string(), "Room", true)).unwrap_err();
        let text = format!("{err} {err:?}");
        assert!(
            text.contains(&format!("http://127.0.0.1:{port}")),
            "the endpoint host is missing from the rendered error"
        );
        assert!(
            !text.contains("AAHbotTOKEN") && !text.contains("123456"),
            "a part of the bot token leaked into the rendered error"
        );
    }
}
