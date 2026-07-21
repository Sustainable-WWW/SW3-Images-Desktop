//! Lead notifications via the Brevo transactional email API.
//!
//! Configured entirely through environment variables (typically a `.env` file
//! in the project root, loaded at startup):
//!
//!   BREVO_API_KEY      your Brevo v3 API key (xkeysib-…)
//!   LEAD_NOTIFY_EMAIL  where to send the "new lead" email, e.g. info@sustainablewww.org
//!   LEAD_FROM_EMAIL    optional verified Brevo sender; defaults to LEAD_NOTIFY_EMAIL
//!
//! When the variables are missing, notifications are silently disabled — the
//! lead is still stored in data/leads.jsonl either way.

const BREVO_ENDPOINT: &str = "https://api.brevo.com/v3/smtp/email";

pub struct Notifier {
    config: Option<Config>,
    client: reqwest::Client,
}

struct Config {
    api_key: String,
    to: String,
    from: String,
}

impl Notifier {
    pub fn from_env() -> Notifier {
        let config = match (std::env::var("BREVO_API_KEY"), std::env::var("LEAD_NOTIFY_EMAIL")) {
            (Ok(api_key), Ok(to)) if !api_key.trim().is_empty() && !to.trim().is_empty() => {
                let from = std::env::var("LEAD_FROM_EMAIL")
                    .ok()
                    .filter(|v| !v.trim().is_empty())
                    .unwrap_or_else(|| to.clone());
                println!("lead notifications: enabled → {to}");
                Some(Config { api_key, to, from })
            }
            _ => {
                println!("lead notifications: disabled (set BREVO_API_KEY and LEAD_NOTIFY_EMAIL to enable)");
                None
            }
        };
        Notifier {
            config,
            client: reqwest::Client::new(),
        }
    }

    /// Fire-and-forget: never blocks or fails the upload that triggered it.
    pub fn notify_lead(&self, name: &str, email: &str, files: usize) {
        let Some(cfg) = &self.config else { return };
        let payload = brevo_payload(name, email, files, &cfg.to, &cfg.from);
        let request = self
            .client
            .post(BREVO_ENDPOINT)
            .header("api-key", cfg.api_key.clone())
            .header("accept", "application/json")
            .json(&payload);
        let lead_email = email.to_string();
        tokio::spawn(async move {
            match request.timeout(std::time::Duration::from_secs(10)).send().await {
                Ok(resp) if resp.status().is_success() => {
                    println!("lead notification sent for {lead_email}");
                }
                Ok(resp) => {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    eprintln!("lead notification failed ({status}): {body}");
                }
                Err(e) => eprintln!("lead notification failed: {e}"),
            }
        });
    }
}

pub fn brevo_payload(
    name: &str,
    email: &str,
    files: usize,
    to: &str,
    from: &str,
) -> serde_json::Value {
    serde_json::json!({
        "sender": { "name": "SW3 Images", "email": from },
        "to": [{ "email": to }],
        "subject": format!("🌱 New SW3 Images lead: {}", name),
        "htmlContent": format!(
            "<h2>New lead from the SW3 Images compression engine</h2>\
             <p>Someone unlocked the 50-file limit:</p>\
             <ul>\
               <li><b>Name:</b> {}</li>\
               <li><b>Email:</b> {}</li>\
               <li><b>Files compressed:</b> {}</li>\
             </ul>\
             <p>The full lead log lives in <code>data/leads.jsonl</code> on the server.</p>",
            escape_html(name),
            escape_html(email),
            files
        ),
    })
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_contains_lead_and_recipient() {
        let p = brevo_payload("Anna Ek", "anna@example.se", 23, "info@sustainablewww.org", "noreply@sustainablewww.org");
        assert_eq!(p["to"][0]["email"], "info@sustainablewww.org");
        assert_eq!(p["sender"]["email"], "noreply@sustainablewww.org");
        assert!(p["subject"].as_str().unwrap().contains("Anna Ek"));
        let html = p["htmlContent"].as_str().unwrap();
        assert!(html.contains("anna@example.se"));
        assert!(html.contains("23"));
    }

    #[test]
    fn html_is_escaped() {
        let p = brevo_payload("<script>x</script>", "a@b.se", 1, "to@x.se", "from@x.se");
        let html = p["htmlContent"].as_str().unwrap();
        assert!(!html.contains("<script>"));
        assert!(html.contains("&lt;script&gt;"));
    }
}
