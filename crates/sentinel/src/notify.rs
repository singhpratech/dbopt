//! Webhook notifier for fired alerts.
//!
//! On a NEW fired alert we POST a JSON payload to the configured `webhook_url`.
//! Best-effort: a failed POST logs a warning and is swallowed — a notification
//! failure must NEVER crash the poller (the alert is already persisted, so it is
//! still visible in the feed).
//!
//! One body builder serves all targets. A "generic" webhook receives a rich
//! `{ text, severity, color, alert }` document; Slack's incoming-webhook only
//! needs `{ text }`; Teams wants a MessageCard with a severity-driven
//! `themeColor`. We build the right shape from [`WebhookFormat`].

use crate::alerts::{FiredAlert, WebhookFormat};

/// Build the JSON body for a fired alert in the requested flavour. Pure, so it
/// can be unit-tested without a network.
pub fn build_body(
    alert: &FiredAlert,
    instance: &str,
    format: WebhookFormat,
) -> serde_json::Value {
    let text = format!(
        "[{}] {} on {} — {}",
        alert.severity.as_str().to_uppercase(),
        alert.metric,
        instance,
        alert.message,
    );
    match format {
        WebhookFormat::Slack => serde_json::json!({ "text": text }),
        WebhookFormat::Teams => serde_json::json!({
            "@type": "MessageCard",
            "@context": "http://schema.org/extensions",
            "themeColor": alert.severity.color().trim_start_matches('#'),
            "summary": text,
            "sections": [{
                "activityTitle": format!("{} alert", alert.severity.as_str()),
                "activitySubtitle": instance,
                "facts": [
                    { "name": "Metric",    "value": alert.metric },
                    { "name": "Measured",  "value": format!("{}", alert.value) },
                    { "name": "Threshold", "value": format!("{}", alert.threshold) },
                    { "name": "Severity",  "value": alert.severity.as_str() },
                ],
                "text": alert.message,
            }],
        }),
        WebhookFormat::Generic => serde_json::json!({
            // A `text` summary so even a dumb receiver shows something useful,
            // plus structured fields for anything that parses the body.
            "text": text,
            "severity": alert.severity.as_str(),
            "color": alert.severity.color(),
            "instance": instance,
            "alert": {
                "rule_id": alert.rule_id,
                "metric": alert.metric,
                "value": alert.value,
                "threshold": alert.threshold,
                "message": alert.message,
            },
        }),
    }
}

/// POST the alert to the webhook. Best-effort: every failure logs and returns
/// `false`; the poller ignores the result. `url` empty/blank = no-op.
pub async fn notify_webhook(
    url: &str,
    alert: &FiredAlert,
    instance: &str,
    format: WebhookFormat,
) -> bool {
    let url = url.trim();
    if url.is_empty() {
        return false;
    }
    let body = build_body(alert, instance, format);
    // A short timeout so a hung receiver can't stall the poll loop. We build a
    // fresh client per call — alerts are rare, so the cost is irrelevant and it
    // keeps the module stateless.
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(target: "sentinel::notify", "failed to build http client: {e}");
            return false;
        }
    };
    match client.post(url).json(&body).send().await {
        Ok(resp) if resp.status().is_success() => {
            tracing::info!(
                target: "sentinel::notify",
                "alert {} delivered to webhook ({})",
                alert.rule_id, resp.status()
            );
            true
        }
        Ok(resp) => {
            tracing::warn!(
                target: "sentinel::notify",
                "webhook returned {} for alert {}",
                resp.status(), alert.rule_id
            );
            false
        }
        Err(e) => {
            tracing::warn!(
                target: "sentinel::notify",
                "webhook POST failed for alert {}: {e}",
                alert.rule_id
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alerts::Severity;

    fn sample() -> FiredAlert {
        FiredAlert {
            rule_id: "cpu.runnable_tasks_high".into(),
            metric: "Runnable tasks waiting for a CPU".into(),
            value: 25.0,
            threshold: 10.0,
            severity: Severity::Warning,
            message: "Runnable tasks waiting for a CPU: 25 >= 10".into(),
        }
    }

    #[test]
    fn slack_body_is_just_text() {
        let b = build_body(&sample(), "prod-sql", WebhookFormat::Slack);
        assert!(b.get("text").and_then(|v| v.as_str()).unwrap().contains("prod-sql"));
        assert!(b.get("severity").is_none());
    }

    #[test]
    fn generic_body_has_structured_fields_and_text() {
        let b = build_body(&sample(), "prod-sql", WebhookFormat::Generic);
        assert!(b["text"].as_str().unwrap().contains("WARNING"));
        assert_eq!(b["severity"], "warning");
        assert_eq!(b["alert"]["rule_id"], "cpu.runnable_tasks_high");
        assert_eq!(b["alert"]["value"], 25.0);
        // color is the severity hint (amber for warning)
        assert_eq!(b["color"], Severity::Warning.color());
    }

    #[test]
    fn teams_body_is_a_messagecard_with_theme_color() {
        let b = build_body(&sample(), "prod-sql", WebhookFormat::Teams);
        assert_eq!(b["@type"], "MessageCard");
        // themeColor strips the leading '#'
        assert_eq!(b["themeColor"], Severity::Warning.color().trim_start_matches('#'));
        assert!(b["summary"].as_str().unwrap().contains("prod-sql"));
    }
}
