use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::DatabaseRepo;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Claims {
    /// Subject: user id
    pub sub: String,
    /// JWT ID (unique per token)
    pub jti: String,
    /// Token type: "access" or "refresh"
    pub typ: String,
    /// Expiry (Unix seconds)
    pub exp: i64,
    /// Issued at (Unix seconds)
    pub iat: i64,
    /// Device id (optional, set for refresh tokens)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub did: Option<String>,
}

pub struct JwtManager {
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
    access_ttl_secs: u64,
    refresh_ttl_secs: u64,
}

impl JwtManager {
    pub fn new(secret: &str, access_ttl_secs: u64, refresh_ttl_secs: u64) -> Self {
        let key_bytes = secret.as_bytes();
        Self {
            encoding_key: EncodingKey::from_secret(key_bytes),
            decoding_key: DecodingKey::from_secret(key_bytes),
            access_ttl_secs,
            refresh_ttl_secs,
        }
    }

    pub fn issue_access_token(&self, user_id: &str) -> Result<String> {
        let now = Utc::now().timestamp();
        let claims = Claims {
            sub: user_id.to_owned(),
            jti: Uuid::new_v4().to_string(),
            typ: "access".to_owned(),
            exp: now + self.access_ttl_secs as i64,
            iat: now,
            did: None,
        };
        encode(&Header::new(Algorithm::HS256), &claims, &self.encoding_key)
            .context("failed to issue access token")
    }

    pub fn issue_refresh_token(&self, user_id: &str, device_id: Option<&str>) -> Result<(String, Claims)> {
        let now = Utc::now().timestamp();
        let claims = Claims {
            sub: user_id.to_owned(),
            jti: Uuid::new_v4().to_string(),
            typ: "refresh".to_owned(),
            exp: now + self.refresh_ttl_secs as i64,
            iat: now,
            did: device_id.map(str::to_owned),
        };
        let token = encode(&Header::new(Algorithm::HS256), &claims, &self.encoding_key)
            .context("failed to issue refresh token")?;
        Ok((token, claims))
    }

    pub fn verify(&self, token: &str) -> Result<Claims> {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.validate_exp = true;
        validation.leeway = 0;
        decode::<Claims>(token, &self.decoding_key, &validation)
            .map(|d| d.claims)
            .map_err(|e| anyhow!("invalid token: {e}"))
    }

    #[allow(dead_code)]
    pub fn verify_access(&self, token: &str) -> Result<Claims> {
        let claims = self.verify(token)?;
        if claims.typ != "access" {
            return Err(anyhow!("expected access token, got {}", claims.typ));
        }
        Ok(claims)
    }

    pub fn verify_refresh(&self, token: &str) -> Result<Claims> {
        let claims = self.verify(token)?;
        if claims.typ != "refresh" {
            return Err(anyhow!("expected refresh token, got {}", claims.typ));
        }
        Ok(claims)
    }

    /// Rotate refresh token: revoke old jti, issue new access + refresh pair.
    pub async fn rotate(
        &self,
        db: &dyn DatabaseRepo,
        old_token: &str,
        device_id: Option<&str>,
    ) -> Result<(String, String)> {
        let claims = self.verify_refresh(old_token)?;

        // Issue new pair
        let access = self.issue_access_token(&claims.sub)?;
        let (refresh, new_claims) = self.issue_refresh_token(&claims.sub, device_id)?;

        // Atomically revoke old + store new
        db.rotate_refresh_token(
            &claims.jti,
            &new_claims.jti,
            &new_claims.sub,
            device_id,
            new_claims.exp,
        ).await?;

        Ok((access, refresh))
    }

    /// Store a freshly issued refresh token in DB (called on first login).
    pub async fn store_refresh_token(&self, db: &dyn DatabaseRepo, claims: &Claims) -> Result<()> {
        db.store_refresh_token(
            &claims.jti,
            &claims.sub,
            claims.did.as_deref(),
            claims.exp,
        ).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;
    use crate::db::sqlite::SqliteRepo;

    fn mgr() -> JwtManager {
        JwtManager::new("test-secret-key-1234567890abcdef", 3600, 86400 * 30)
    }

    #[test]
    fn test_issue_and_verify_access() {
        let m = mgr();
        let token = m.issue_access_token("user-1").unwrap();
        let claims = m.verify_access(&token).unwrap();
        assert_eq!(claims.sub, "user-1");
        assert_eq!(claims.typ, "access");
    }

    #[test]
    fn test_issue_and_verify_refresh() {
        let m = mgr();
        let (token, claims_in) = m.issue_refresh_token("user-1", Some("dev-1")).unwrap();
        let claims = m.verify_refresh(&token).unwrap();
        assert_eq!(claims.sub, "user-1");
        assert_eq!(claims.typ, "refresh");
        assert_eq!(claims.jti, claims_in.jti);
        assert_eq!(claims.did, Some("dev-1".to_string()));
    }

    #[test]
    fn test_wrong_type_rejected() {
        let m = mgr();
        let access = m.issue_access_token("user-1").unwrap();
        assert!(m.verify_refresh(&access).is_err());

        let (refresh, _) = m.issue_refresh_token("user-1", None).unwrap();
        assert!(m.verify_access(&refresh).is_err());
    }

    #[test]
    fn test_tampered_token_rejected() {
        let m = mgr();
        let token = m.issue_access_token("user-1").unwrap();
        let mut parts: Vec<&str> = token.splitn(3, '.').collect();
        parts[1] = "dGFtcGVyZWQ"; // base64 "tampered"
        let bad = parts.join(".");
        assert!(m.verify(&bad).is_err());
    }

    #[test]
    fn test_expired_token_rejected() {
        // ttl=1: expires 1 second after issuance
        let m = JwtManager::new("test-secret-key-1234567890abcdef", 1, 1);
        let token = m.issue_access_token("user-1").unwrap();
        std::thread::sleep(std::time::Duration::from_secs(2));
        assert!(m.verify(&token).is_err());
    }

    async fn insert_test_user(db: &SqliteRepo, user_id: &str) {
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO users (id, username, password_hash, role, created_at, updated_at)
             VALUES (?, ?, 'hash', 'user', ?, ?)",
        )
        .bind(user_id)
        .bind(user_id)
        .bind(now)
        .bind(now)
        .execute(&db.pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn test_rotate_valid() {
        let tmp = NamedTempFile::new().unwrap();
        let db = SqliteRepo::open(tmp.path().to_str().unwrap()).await.unwrap();

        insert_test_user(&db, "user-1").await;

        let m = mgr();
        let (refresh, claims) = m.issue_refresh_token("user-1", None).unwrap();
        m.store_refresh_token(&db, &claims).await.unwrap();

        let (new_access, new_refresh) = m.rotate(&db, &refresh, None).await.unwrap();
        assert!(!new_access.is_empty());
        assert!(!new_refresh.is_empty());

        // Old token should be revoked -- reuse must fail
        let err = m.rotate(&db, &refresh, None).await;
        assert!(err.is_err(), "reuse of old refresh token must fail");
    }

    #[tokio::test]
    async fn test_rotate_not_stored_fails() {
        let tmp = NamedTempFile::new().unwrap();
        let db = SqliteRepo::open(tmp.path().to_str().unwrap()).await.unwrap();
        insert_test_user(&db, "user-1").await;

        let m = mgr();
        let (refresh, _) = m.issue_refresh_token("user-1", None).unwrap();
        // Not stored in DB -> should fail
        let err = m.rotate(&db, &refresh, None).await;
        assert!(err.is_err());
    }
}
