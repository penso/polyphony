use std::sync::Arc;

use {
    async_trait::async_trait,
    polyphony_core::{
        FeedbackCapabilities, FeedbackChannelDescriptor, FeedbackInboundMode, FeedbackNotification,
        FeedbackSink,
    },
    polyphony_workflow::FeedbackConfig,
    serde::Serialize,
};

pub struct FeedbackRegistry {
    sinks: Vec<Arc<dyn FeedbackSink>>,
}

impl FeedbackRegistry {
    pub fn from_config(config: &FeedbackConfig) -> Self {
        let telegram_enabled =
            config.offered.is_empty() || config.offered.iter().any(|kind| kind == "telegram");
        let webhook_enabled =
            config.offered.is_empty() || config.offered.iter().any(|kind| kind == "webhook");
        let mut sinks: Vec<Arc<dyn FeedbackSink>> = Vec::new();
        if telegram_enabled {
            for (name, sink) in &config.telegram {
                if let Some(token) = &sink.bot_token {
                    sinks.push(Arc::new(TelegramFeedbackSink::new(
                        name.clone(),
                        token.clone(),
                        sink.chat_id.clone(),
                    )));
                }
            }
        }
        if webhook_enabled {
            for (name, sink) in &config.webhook {
                if let Some(url) = &sink.url {
                    sinks.push(Arc::new(WebhookFeedbackSink::new(
                        name.clone(),
                        url.clone(),
                        sink.bearer_token.clone(),
                    )));
                }
            }
        }
        Self { sinks }
    }

    pub fn is_empty(&self) -> bool {
        self.sinks.is_empty()
    }

    pub fn descriptors(&self) -> Vec<FeedbackChannelDescriptor> {
        self.sinks.iter().map(|sink| sink.descriptor()).collect()
    }

    pub async fn send_all(&self, notification: &FeedbackNotification) -> Vec<(String, String)> {
        let mut failures = Vec::new();
        for sink in &self.sinks {
            if let Err(error) = sink.send(notification).await {
                failures.push((sink.component_key(), error.to_string()));
            }
        }
        failures
    }
}

#[derive(Debug, Clone)]
pub struct TelegramFeedbackSink {
    name: String,
    token: String,
    chat_id: String,
    client: reqwest::Client,
}

impl TelegramFeedbackSink {
    pub fn new(name: String, token: String, chat_id: String) -> Self {
        Self {
            name,
            token,
            chat_id,
            client: reqwest::Client::new(),
        }
    }
}

#[derive(Serialize)]
struct TelegramSendMessageBody {
    chat_id: String,
    text: String,
    disable_web_page_preview: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_markup: Option<TelegramInlineKeyboard>,
}

#[derive(Serialize)]
struct TelegramInlineKeyboard {
    inline_keyboard: Vec<Vec<TelegramButton>>,
}

#[derive(Serialize)]
struct TelegramButton {
    text: String,
    url: String,
}

#[async_trait]
impl FeedbackSink for TelegramFeedbackSink {
    fn component_key(&self) -> String {
        format!("feedback:telegram:{}", self.name)
    }

    fn descriptor(&self) -> FeedbackChannelDescriptor {
        FeedbackChannelDescriptor {
            kind: "telegram".into(),
            inbound_mode: FeedbackInboundMode::Polling,
            capabilities: FeedbackCapabilities {
                supports_outbound: true,
                supports_links: true,
                supports_interactive: true,
            },
        }
    }

    async fn send(&self, notification: &FeedbackNotification) -> Result<(), polyphony_core::Error> {
        let buttons = notification
            .links
            .iter()
            .map(|link| TelegramButton {
                text: link.label.clone(),
                url: link.url.clone(),
            })
            .chain(notification.actions.iter().filter_map(|action| {
                action.url.as_ref().map(|url| TelegramButton {
                    text: action.label.clone(),
                    url: url.clone(),
                })
            }))
            .map(|button| vec![button])
            .collect::<Vec<_>>();
        let response = self
            .client
            .post(format!(
                "https://api.telegram.org/bot{}/sendMessage",
                self.token
            ))
            .json(&TelegramSendMessageBody {
                chat_id: self.chat_id.clone(),
                text: render_notification_text(notification),
                disable_web_page_preview: true,
                reply_markup: (!buttons.is_empty()).then_some(TelegramInlineKeyboard {
                    inline_keyboard: buttons,
                }),
            })
            .send()
            .await
            .map_err(|error| polyphony_core::Error::Adapter(error.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(polyphony_core::Error::Adapter(format!(
                "telegram sendMessage failed with status {status}"
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct WebhookFeedbackSink {
    name: String,
    url: String,
    bearer_token: Option<String>,
    client: reqwest::Client,
}

impl WebhookFeedbackSink {
    pub fn new(name: String, url: String, bearer_token: Option<String>) -> Self {
        Self {
            name,
            url,
            bearer_token,
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl FeedbackSink for WebhookFeedbackSink {
    fn component_key(&self) -> String {
        format!("feedback:webhook:{}", self.name)
    }

    fn descriptor(&self) -> FeedbackChannelDescriptor {
        FeedbackChannelDescriptor {
            kind: "webhook".into(),
            inbound_mode: FeedbackInboundMode::Webhook,
            capabilities: FeedbackCapabilities {
                supports_outbound: true,
                supports_links: true,
                supports_interactive: false,
            },
        }
    }

    async fn send(&self, notification: &FeedbackNotification) -> Result<(), polyphony_core::Error> {
        let mut request = self.client.post(&self.url).json(notification);
        if let Some(token) = &self.bearer_token {
            request = request.bearer_auth(token);
        }
        let response = request
            .send()
            .await
            .map_err(|error| polyphony_core::Error::Adapter(error.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(polyphony_core::Error::Adapter(format!(
                "feedback webhook failed with status {status}"
            )));
        }
        Ok(())
    }
}

fn render_notification_text(notification: &FeedbackNotification) -> String {
    let mut lines = vec![
        notification.title.clone(),
        String::new(),
        notification.body.clone(),
    ];
    if !notification.links.is_empty() {
        lines.push(String::new());
        lines.push("Links:".into());
        for link in &notification.links {
            lines.push(format!("- {}: {}", link.label, link.url));
        }
    }
    if !notification.actions.is_empty() {
        lines.push(String::new());
        lines.push("Actions:".into());
        for action in &notification.actions {
            match &action.url {
                Some(url) => lines.push(format!("- {}: {}", action.label, url)),
                None => lines.push(format!("- {}", action.label)),
            }
        }
    }
    lines.join("\n")
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use polyphony_core::FeedbackNotification;

    use super::FeedbackRegistry;

    #[test]
    fn registry_builds_configured_sinks() {
        let config = serde_yaml::from_str(
            r#"
offered: [telegram, webhook]
telegram:
  ops:
    bot_token: telegram-token
    chat_id: "123"
webhook:
  audit:
    url: https://example.com/hook
"#,
        )
        .unwrap();

        let registry = FeedbackRegistry::from_config(&config);
        let kinds = registry
            .descriptors()
            .into_iter()
            .map(|descriptor| descriptor.kind)
            .collect::<Vec<_>>();

        assert_eq!(kinds, vec!["telegram".to_string(), "webhook".to_string()]);
    }

    #[test]
    fn notification_text_includes_links_and_actions() {
        let text = super::render_notification_text(&FeedbackNotification {
            key: "handoff:test".into(),
            title: "Ready".into(),
            body: "Issue is ready".into(),
            links: vec![polyphony_core::FeedbackLink {
                label: "PR".into(),
                url: "https://example.com/pr".into(),
            }],
            actions: vec![polyphony_core::FeedbackAction {
                id: "review".into(),
                label: "Review".into(),
                url: Some("https://example.com/review".into()),
            }],
        });

        assert!(text.contains("Ready"));
        assert!(text.contains("https://example.com/pr"));
        assert!(text.contains("https://example.com/review"));
    }
}
