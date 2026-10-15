//! Email the site sends: the link that confirms a removal request (Spec 08, Rights lifecycle).
//! Sending sits behind [`Mailer`], so tests use [`FakeMailer`] and make no network calls.

use std::sync::Mutex;

use async_trait::async_trait;

/// One plain-text email.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// The recipient's address.
    pub to: String,
    /// The subject line.
    pub subject: String,
    /// The plain-text body.
    pub text: String,
}

/// Why an email was not sent.
#[derive(Debug, thiserror::Error)]
pub enum MailError {
    /// No sender is configured, so nothing can be sent.
    #[error("no mailer is configured")]
    Unavailable,
    /// The provider refused or failed the send.
    #[error("sending failed: {0}")]
    Send(String),
}

/// Sends email.
#[async_trait]
pub trait Mailer: Send + Sync {
    /// Whether this mailer can send at all. The rights form checks it before storing a request
    /// whose link could never arrive.
    fn ready(&self) -> bool {
        true
    }

    /// Sends one email.
    async fn send(&self, message: &Message) -> Result<(), MailError>;
}

/// The mailer when `WT_MAIL_FROM` or `WT_PUBLIC_URL` is unset: it sends nothing.
pub struct UnavailableMailer;

#[async_trait]
impl Mailer for UnavailableMailer {
    fn ready(&self) -> bool {
        false
    }

    async fn send(&self, _message: &Message) -> Result<(), MailError> {
        Err(MailError::Unavailable)
    }
}

/// Amazon SES, through the web task's role.
pub struct SesMailer {
    client: aws_sdk_sesv2::Client,
    from: String,
}

impl SesMailer {
    /// A mailer sending from `from`, with credentials and region from the environment (the ECS
    /// task role and `AWS_REGION`).
    pub async fn new(from: String) -> Self {
        use aws_smithy_http_client::tls::{Provider, rustls_provider::CryptoMode};
        let http = aws_smithy_http_client::Builder::new()
            .tls_provider(Provider::Rustls(CryptoMode::Ring))
            .build_https();
        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .http_client(http)
            .load()
            .await;
        Self {
            client: aws_sdk_sesv2::Client::new(&config),
            from,
        }
    }
}

#[async_trait]
impl Mailer for SesMailer {
    async fn send(&self, message: &Message) -> Result<(), MailError> {
        use aws_sdk_sesv2::types::{
            Body, Content, Destination, EmailContent, Message as SesMessage,
        };
        let content = |s: &str| {
            Content::builder()
                .data(s)
                .charset("UTF-8")
                .build()
                .map_err(|e| MailError::Send(e.to_string()))
        };
        let email = EmailContent::builder()
            .simple(
                SesMessage::builder()
                    .subject(content(&message.subject)?)
                    .body(Body::builder().text(content(&message.text)?).build())
                    .build(),
            )
            .build();
        self.client
            .send_email()
            .from_email_address(&self.from)
            .destination(Destination::builder().to_addresses(&message.to).build())
            .content(email)
            .send()
            .await
            .map_err(|e| {
                MailError::Send(aws_sdk_sesv2::error::DisplayErrorContext(e).to_string())
            })?;
        Ok(())
    }
}

/// A mailer for tests: keeps what it was asked to send, or fails every send.
#[derive(Default)]
pub struct FakeMailer {
    sent: Mutex<Vec<Message>>,
    fail: bool,
}

impl FakeMailer {
    /// A fake whose every send fails, as when the provider is down.
    pub fn failing() -> Self {
        Self {
            fail: true,
            ..Self::default()
        }
    }

    /// What was sent, oldest first.
    pub fn sent(&self) -> Vec<Message> {
        self.sent
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl Mailer for FakeMailer {
    async fn send(&self, message: &Message) -> Result<(), MailError> {
        if self.fail {
            return Err(MailError::Send("the fake is set to fail".to_owned()));
        }
        self.sent
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(message.clone());
        Ok(())
    }
}
