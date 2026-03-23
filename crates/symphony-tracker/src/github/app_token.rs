//! GitHub App installation token generation.
//!
//! Generates short-lived installation access tokens for authenticating as
//! a GitHub App. Tokens are valid for 1 hour and auto-refreshed when they
//! are within 5 minutes of expiring.
//!
//! Flow:
//! 1. Generate a JWT signed with the app's RSA private key (RS256)
//! 2. Exchange the JWT for an installation access token via GitHub API
//! 3. Cache the token and refresh before expiry

use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use reqwest::header::{self, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info};

use symphony_core::error::SymphonyError;

/// Refresh tokens 5 minutes before they expire.
const REFRESH_BUFFER_SECS: u64 = 300;

/// Configuration for GitHub App authentication.
#[derive(Debug, Clone)]
pub struct GitHubAppConfig {
    pub app_id: u64,
    pub installation_id: u64,
    pub private_key_pem: String,
    pub api_endpoint: String,
}

/// A cached installation token with its expiry time.
#[derive(Debug, Clone)]
struct CachedToken {
    token: String,
    expires_at: u64, // Unix timestamp
}

/// JWT claims for GitHub App authentication.
#[derive(Debug, Serialize)]
struct JwtClaims {
    iat: u64,
    exp: u64,
    iss: String,
}

/// Response from the installation token endpoint.
#[derive(Debug, Deserialize)]
struct InstallationTokenResponse {
    token: String,
    expires_at: Option<String>,
}

/// Manages GitHub App installation tokens with auto-refresh.
pub struct GitHubAppTokenProvider {
    config: GitHubAppConfig,
    cached: Arc<RwLock<Option<CachedToken>>>,
    http: reqwest::Client,
}

impl GitHubAppTokenProvider {
    /// Create a new token provider from the app configuration.
    pub fn new(config: GitHubAppConfig) -> Result<Self, SymphonyError> {
        // Validate that the private key can be parsed.
        EncodingKey::from_rsa_pem(config.private_key_pem.as_bytes())
            .map_err(|e| SymphonyError::ConfigValidation {
                detail: format!("invalid GitHub App private key: {e}"),
            })?;

        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| SymphonyError::TrackerApiRequest {
                detail: format!("failed to build HTTP client: {e}"),
            })?;

        Ok(Self {
            config,
            cached: Arc::new(RwLock::new(None)),
            http,
        })
    }

    /// Get a valid installation token, refreshing if necessary.
    pub async fn get_token(&self) -> Result<String, SymphonyError> {
        // Check if we have a cached token that's still valid.
        if let Some(token) = self.get_cached_token() {
            return Ok(token);
        }

        // Generate a new token.
        let token = self.generate_installation_token().await?;
        Ok(token)
    }

    /// Force-generate a new token, ignoring the cache.
    /// Used by the background refresh task to ensure the token file
    /// always has a fresh token, not the cached one that might expire soon.
    pub async fn force_refresh(&self) -> Result<String, SymphonyError> {
        self.generate_installation_token().await
    }

    /// Check if the cached token is still valid (with buffer).
    fn get_cached_token(&self) -> Option<String> {
        let guard = self.cached.read().ok()?;
        let cached = guard.as_ref()?;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        if now + REFRESH_BUFFER_SECS < cached.expires_at {
            Some(cached.token.clone())
        } else {
            debug!("cached token expired or expiring soon, refreshing");
            None
        }
    }

    /// Generate a JWT and exchange it for an installation access token.
    async fn generate_installation_token(&self) -> Result<String, SymphonyError> {
        let jwt = self.generate_jwt()?;
        let token_response = self.exchange_jwt_for_token(&jwt).await?;

        // Parse expiry from the response or default to 1 hour from now.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let expires_at = token_response
            .expires_at
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.timestamp() as u64)
            .unwrap_or(now + 3600);

        let token = token_response.token.clone();

        // Cache the token.
        if let Ok(mut guard) = self.cached.write() {
            *guard = Some(CachedToken {
                token: token.clone(),
                expires_at,
            });
        }

        info!(
            expires_in_secs = expires_at.saturating_sub(now),
            "generated GitHub App installation token"
        );

        Ok(token)
    }

    /// Generate a JWT signed with the app's private key.
    fn generate_jwt(&self) -> Result<String, SymphonyError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| SymphonyError::ConfigValidation {
                detail: format!("system time error: {e}"),
            })?;

        // Issue the JWT 60 seconds in the past to account for clock drift.
        let iat = now.as_secs().saturating_sub(60);
        let exp = iat + Duration::from_secs(600).as_secs(); // 10 minute max

        let claims = JwtClaims {
            iat,
            exp,
            iss: self.config.app_id.to_string(),
        };

        let key = EncodingKey::from_rsa_pem(self.config.private_key_pem.as_bytes())
            .map_err(|e| SymphonyError::ConfigValidation {
                detail: format!("failed to parse private key: {e}"),
            })?;

        let token = encode(&Header::new(Algorithm::RS256), &claims, &key)
            .map_err(|e| SymphonyError::ConfigValidation {
                detail: format!("failed to encode JWT: {e}"),
            })?;

        debug!("generated GitHub App JWT");
        Ok(token)
    }

    /// Exchange a JWT for an installation access token via the GitHub API.
    async fn exchange_jwt_for_token(
        &self,
        jwt: &str,
    ) -> Result<InstallationTokenResponse, SymphonyError> {
        let url = format!(
            "{}/app/installations/{}/access_tokens",
            self.config.api_endpoint, self.config.installation_id
        );

        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {jwt}")).map_err(|e| {
                SymphonyError::TrackerApiRequest {
                    detail: format!("invalid JWT header: {e}"),
                }
            })?,
        );
        headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(
            header::USER_AGENT,
            HeaderValue::from_static("chronoai-symphony/0.1"),
        );

        let response = self
            .http
            .post(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| SymphonyError::TrackerApiRequest {
                detail: format!("failed to request installation token: {e}"),
            })?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            error!(status = %status, body = %body, "failed to get installation token");
            return Err(SymphonyError::TrackerApiStatus {
                status: status.as_u16(),
                body,
            });
        }

        response.json().await.map_err(|e| {
            SymphonyError::TrackerUnknownPayload {
                detail: format!("failed to parse installation token response: {e}"),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jwt_claims_serializes() {
        let claims = JwtClaims {
            iat: 1000,
            exp: 1600,
            iss: "12345".to_string(),
        };
        let json = serde_json::to_string(&claims).unwrap();
        assert!(json.contains("12345"));
        assert!(json.contains("1000"));
    }

    #[test]
    fn cached_token_expiry_check() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let provider_config = GitHubAppConfig {
            app_id: 1,
            installation_id: 1,
            private_key_pem: String::new(),
            api_endpoint: "https://api.github.com".to_string(),
        };

        // Can't create a real provider without a valid key, so test the logic directly.
        let future_expiry = now + 3600;
        let cached = CachedToken {
            token: "test-token".to_string(),
            expires_at: future_expiry,
        };
        assert!(now + REFRESH_BUFFER_SECS < cached.expires_at);

        let past_expiry = now + 100; // Within buffer
        let expired = CachedToken {
            token: "old-token".to_string(),
            expires_at: past_expiry,
        };
        assert!(!(now + REFRESH_BUFFER_SECS < expired.expires_at));

        // Suppress unused variable warning
        let _ = provider_config;
    }
}
