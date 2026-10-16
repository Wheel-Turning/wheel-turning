//! `wt alerts`: alerts to the owner (Spec 11, PB4.2).

use std::sync::Arc;

use anyhow::Context;
use clap::Subcommand;
use wt_core::Config;
use wt_extract::{AwsSns, SnsAlerter, SnsPublisher};

/// `wt alerts` subcommands.
#[derive(Debug, Subcommand)]
pub enum AlertsCommand {
    /// Publish one test message to WT_ALERT_TOPIC_ARN, which emails every subscriber. Run it
    /// with credentials that may publish there, such as the owner's AWS profile.
    Test,
}

/// Runs an `alerts` subcommand.
pub async fn alerts(config: &Config, command: AlertsCommand) -> anyhow::Result<()> {
    match command {
        AlertsCommand::Test => {
            let publisher: Arc<dyn SnsPublisher> = Arc::new(AwsSns::from_env().await);
            let id = test(config.alert_topic_arn.as_deref(), publisher).await?;
            println!("published test alert {id}; each subscriber should receive it by email");
        }
    }
    Ok(())
}

/// Publishes the test message through `publisher` and returns its message ID.
async fn test(topic: Option<&str>, publisher: Arc<dyn SnsPublisher>) -> anyhow::Result<String> {
    let topic = topic
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .context("WT_ALERT_TOPIC_ARN is not set, so there is no topic to test")?;
    let alerter = SnsAlerter::new(publisher, topic);
    let id = alerter
        .publish(
            "test alert",
            "This is a test from `wt alerts test`. Alerts to the owner reach this address. \
             Nothing needs doing.",
        )
        .await?;
    Ok(id)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use wt_extract::FakeSns;

    #[tokio::test]
    async fn test_publishes_one_message_to_the_topic() {
        let sns = Arc::new(FakeSns::default());
        let topic = "arn:aws:sns:us-east-2:000000000000:wt-v1-alarms";
        test(Some(topic), sns.clone()).await.unwrap();
        let messages = sns.messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].0, topic);
        assert_eq!(messages[0].1, "Wheel Turning: test alert");
    }

    #[tokio::test]
    async fn test_without_a_topic_says_so() {
        let sns = Arc::new(FakeSns::default());
        let e = test(None, sns.clone()).await.unwrap_err();
        assert!(e.to_string().contains("WT_ALERT_TOPIC_ARN"));
        assert!(sns.messages().is_empty());
    }

    #[tokio::test]
    async fn a_failed_publish_is_an_error() {
        let sns = Arc::new(FakeSns::failing());
        let topic = "arn:aws:sns:us-east-2:000000000000:wt-v1-alarms";
        assert!(test(Some(topic), sns).await.is_err());
    }
}
