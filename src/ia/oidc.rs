// ADNT - Dynamic CLI tool manager for ADNT projects
// Copyright (C) 2025 ADNT Sàrl <info@adnt.io>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Interactive OIDC login (device flow) against Authentik, through the PUBLIC
//! client "vllm-cli": no client secret involved.

use super::https_url;
use anyhow::{Context, Result};
use colored::Colorize;
use serde::Deserialize;
use std::env;
use std::time::{Duration, Instant};

const DEFAULT_AUTHENTIK_URL: &str = "https://authentik.k8s.adnt.io";
const DEFAULT_CLIENT_ID: &str = "vllm-cli";
// `vllm-audience` makes the token carry aud = confidential client id (checked by Traefik)
const SCOPE: &str = "openid offline_access vllm-audience";

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri_complete: String,
    expires_in: u64,
    #[serde(default = "default_interval")]
    interval: u64,
}

fn default_interval() -> u64 {
    5
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    error: Option<String>,
}

pub struct Token {
    pub access_token: String,
    pub refresh_token: Option<String>,
}

pub fn authentik_url() -> Result<String> {
    https_url("AUTHENTIK_URL", DEFAULT_AUTHENTIK_URL)
}

pub async fn device_login() -> Result<Token> {
    let flow = DeviceFlow {
        client: reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?,
        authentik: authentik_url()?,
        client_id: env::var("VLLM_PUBLIC_CLIENT_ID")
            .unwrap_or_else(|_| DEFAULT_CLIENT_ID.to_string()),
    };

    println!("{}", "Requesting a device code from Authentik...".dimmed());
    let device = flow.request_code().await?;

    println!("\n{}", "ADNT Authentication".cyan().bold());
    println!("{}", "─".repeat(60).cyan());
    println!("\n{}", "Please visit:".bold());
    println!("  {}", device.verification_uri_complete.green().underline());
    println!(
        "\n{} {} {}",
        "Code:".bold(),
        device.user_code.yellow().bold(),
        format!("(valid {}s)", device.expires_in).dimmed()
    );
    println!("\n{}", "Waiting for authentication...".dimmed());

    if open::that(&device.verification_uri_complete).is_err() {
        println!("{}", "  (Could not open browser automatically)".dimmed());
    }

    flow.poll_token(&device).await
}

struct DeviceFlow {
    client: reqwest::Client,
    authentik: String,
    client_id: String,
}

impl DeviceFlow {
    async fn request_code(&self) -> Result<DeviceCodeResponse> {
        self.client
            .post(format!("{}/application/o/device/", self.authentik))
            .form(&[("client_id", self.client_id.as_str()), ("scope", SCOPE)])
            .send()
            .await?
            .error_for_status()
            .context("Authentik rejected the device code request")?
            .json()
            .await
            .context("Invalid device code response from Authentik")
    }

    async fn poll_token(&self, device: &DeviceCodeResponse) -> Result<Token> {
        let deadline = Instant::now() + Duration::from_secs(device.expires_in);
        let mut interval = Duration::from_secs(device.interval);
        while Instant::now() < deadline {
            tokio::time::sleep(interval).await;

            // Pending authorizations come back as HTTP 400: read the body regardless
            let response: TokenResponse = self
                .client
                .post(format!("{}/application/o/token/", self.authentik))
                .form(&[
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                    ("device_code", device.device_code.as_str()),
                    ("client_id", self.client_id.as_str()),
                ])
                .send()
                .await?
                .json()
                .await?;

            match (response.access_token, response.error.as_deref()) {
                (Some(access_token), None) => {
                    return Ok(Token {
                        access_token,
                        refresh_token: response.refresh_token,
                    })
                }
                (_, Some("authorization_pending")) => {}
                (_, Some("slow_down")) => interval += Duration::from_secs(5),
                (_, Some(error)) => anyhow::bail!("Authentik error: {}", error),
                (None, None) => anyhow::bail!("Authentik returned neither a token nor an error"),
            }
        }

        anyhow::bail!("Authentication timeout - device code expired")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;

    /// Minimal Authentik stand-in: answers each request with the next canned
    /// (status, body) response and records the request lines and bodies.
    fn mock_authentik(responses: Vec<(u16, &'static str)>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        thread::spawn(move || {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request_line = String::new();
                reader.read_line(&mut request_line).unwrap();
                let mut content_length = 0;
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = header.split_once(':') {
                        if name.eq_ignore_ascii_case("content-length") {
                            content_length = value.trim().parse().unwrap();
                        }
                    }
                }
                let mut request_body = vec![0; content_length];
                reader.read_exact(&mut request_body).unwrap();
                recorded.lock().unwrap().push(format!(
                    "{} {}",
                    request_line.trim(),
                    String::from_utf8_lossy(&request_body)
                ));

                write!(
                    stream,
                    "HTTP/1.1 {} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status,
                    body.len(),
                    body
                )
                .unwrap();
            }
        });
        (url, requests)
    }

    fn flow(authentik: String) -> DeviceFlow {
        DeviceFlow {
            client: reqwest::Client::new(),
            authentik,
            client_id: "vllm-cli".to_string(),
        }
    }

    fn device(expires_in: u64) -> DeviceCodeResponse {
        DeviceCodeResponse {
            device_code: "dev123".to_string(),
            user_code: "ABCD".to_string(),
            verification_uri_complete: "https://example.test/device".to_string(),
            expires_in,
            interval: 0,
        }
    }

    #[tokio::test]
    async fn test_request_code() {
        let (url, requests) = mock_authentik(vec![(
            200,
            r#"{"device_code":"dev123","user_code":"ABCD","verification_uri_complete":"https://a/d","expires_in":600}"#,
        )]);

        let device = flow(url).request_code().await.unwrap();

        assert_eq!(device.device_code, "dev123");
        assert_eq!(device.interval, 5, "interval defaults to 5s when omitted");
        let requests = requests.lock().unwrap();
        assert!(requests[0].starts_with("POST /application/o/device/ "));
        assert!(requests[0].contains("client_id=vllm-cli"));
        assert!(requests[0].contains("scope=openid+offline_access+vllm-audience"));
    }

    #[tokio::test]
    async fn test_request_code_rejected() {
        let (url, _) = mock_authentik(vec![(400, r#"{"error":"invalid_client"}"#)]);
        assert!(flow(url).request_code().await.is_err());
    }

    #[tokio::test]
    async fn test_poll_token_waits_for_authorization() {
        let (url, requests) = mock_authentik(vec![
            (400, r#"{"error":"authorization_pending"}"#),
            (200, r#"{"access_token":"jwt","refresh_token":"refresh"}"#),
        ]);

        let token = flow(url).poll_token(&device(60)).await.unwrap();

        assert_eq!(token.access_token, "jwt");
        assert_eq!(token.refresh_token.as_deref(), Some("refresh"));
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].starts_with("POST /application/o/token/ "));
        assert!(requests[1].contains("device_code=dev123"));
        assert!(requests[1].contains("client_id=vllm-cli"));
        assert!(!requests[1].contains("client_secret"), "public client");
    }

    #[tokio::test]
    async fn test_poll_token_access_denied() {
        let (url, _) = mock_authentik(vec![(400, r#"{"error":"access_denied"}"#)]);
        let error = flow(url)
            .poll_token(&device(60))
            .await
            .err()
            .expect("expected an error");
        assert!(error.to_string().contains("access_denied"));
    }

    #[tokio::test]
    async fn test_poll_token_expired() {
        let error = flow("http://127.0.0.1:9".to_string())
            .poll_token(&device(0))
            .await
            .err()
            .expect("expected an error");
        assert!(error.to_string().contains("expired"));
    }
}
