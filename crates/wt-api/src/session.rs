//! Invite tokens, the signed session cookie and CSRF tokens (Spec 06, Invite token access).

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};
use wt_core::ids::TesterId;

/// The session cookie's name.
pub const COOKIE: &str = "wt_session";
/// How long a session lasts.
pub const LIFETIME_S: i64 = 30 * 24 * 3600;

/// The MAC that signs cookies and CSRF tokens.
type HmacSha256 = Hmac<Sha256>;

/// A fresh invite token: 32 random bytes from the operating system, URL-safe.
pub fn new_token() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// The SHA-256 of a token, which is all the database keeps.
pub fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// The key that signs cookies and CSRF tokens.
#[derive(Clone)]
pub struct Keys {
    key: Vec<u8>,
}

impl std::fmt::Debug for Keys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Keys(..)")
    }
}

impl Keys {
    /// Keys from `WT_SESSION_SECRET`.
    pub fn new(secret: &[u8]) -> Self {
        Self {
            key: secret.to_vec(),
        }
    }

    /// A random key, for `dev` without `WT_SESSION_SECRET`: sessions end when the server stops.
    pub fn random() -> Result<Self, getrandom::Error> {
        let mut key = vec![0u8; 32];
        getrandom::fill(&mut key)?;
        Ok(Self { key })
    }

    /// The HMAC of a message under the site key.
    fn mac(&self, message: &str) -> HmacSha256 {
        // An HMAC key may have any length, so this never fails.
        let mut mac = <HmacSha256 as KeyInit>::new_from_slice(&self.key)
            .unwrap_or_else(|_| unreachable!("HMAC takes keys of any length"));
        mac.update(message.as_bytes());
        mac
    }

    /// A message's tag, URL-safe base64.
    fn sign(&self, message: &str) -> String {
        URL_SAFE_NO_PAD.encode(self.mac(message).finalize().into_bytes())
    }

    /// Whether `tag` is the message's tag, compared in constant time.
    fn verify(&self, message: &str, tag: &str) -> bool {
        URL_SAFE_NO_PAD
            .decode(tag)
            .is_ok_and(|tag| self.mac(message).verify_slice(&tag).is_ok())
    }

    /// The cookie value: tester id and issue time, signed.
    pub fn cookie_value(&self, tester: TesterId, issued_at: i64) -> String {
        let body = format!("{tester}.{issued_at}");
        let tag = self.sign(&body);
        format!("{body}.{tag}")
    }

    /// The tester a cookie value names, when its signature holds and it is under 30 days old.
    pub fn verify_cookie(&self, value: &str, now: i64) -> Option<TesterId> {
        let (body, tag) = value.rsplit_once('.')?;
        if !self.verify(body, tag) {
            return None;
        }
        let (tester, issued) = body.split_once('.')?;
        let issued: i64 = issued.parse().ok()?;
        if now < issued || now - issued > LIFETIME_S {
            return None;
        }
        tester.parse().ok()
    }

    /// The CSRF token for a session: an HMAC of its cookie value.
    pub fn csrf(&self, cookie_value: &str) -> String {
        self.sign(&format!("csrf:{cookie_value}"))
    }

    /// Whether `token` is the CSRF token for this session.
    pub fn check_csrf(&self, cookie_value: &str, token: &str) -> bool {
        self.verify(&format!("csrf:{cookie_value}"), token)
    }
}

/// The value of one cookie in a `Cookie` header.
pub fn cookie<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header
        .split(';')
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use sqlx::types::Uuid;

    fn tester() -> TesterId {
        TesterId::from_uuid(Uuid::from_u128(7))
    }

    #[test]
    fn a_signed_cookie_verifies_and_a_tampered_one_does_not() {
        let k = Keys::new(b"secret");
        let v = k.cookie_value(tester(), 1000);
        assert_eq!(k.verify_cookie(&v, 2000), Some(tester()));
        let other = TesterId::from_uuid(Uuid::from_u128(8));
        let forged = v.replacen(&tester().to_string(), &other.to_string(), 1);
        assert_eq!(k.verify_cookie(&forged, 2000), None);
        assert_eq!(Keys::new(b"other").verify_cookie(&v, 2000), None);
    }

    #[test]
    fn a_cookie_expires_after_thirty_days() {
        let k = Keys::new(b"secret");
        let v = k.cookie_value(tester(), 0);
        assert!(k.verify_cookie(&v, LIFETIME_S).is_some());
        assert!(k.verify_cookie(&v, LIFETIME_S + 1).is_none());
    }

    #[test]
    fn csrf_tokens_belong_to_one_session() {
        let k = Keys::new(b"secret");
        let a = k.cookie_value(tester(), 1);
        let b = k.cookie_value(tester(), 2);
        let t = k.csrf(&a);
        assert!(k.check_csrf(&a, &t));
        assert!(!k.check_csrf(&b, &t));
        assert!(!k.check_csrf(&a, "nonsense"));
    }

    #[test]
    fn tokens_are_long_random_and_hashed() {
        let a = new_token().unwrap();
        assert_eq!(a.len(), 43);
        assert_ne!(a, new_token().unwrap());
        assert_eq!(token_hash(&a).len(), 32);
    }

    #[test]
    fn cookies_are_found_by_name() {
        assert_eq!(cookie("a=1; wt_session=x.y.z; b=2", COOKIE), Some("x.y.z"));
        assert_eq!(cookie("a=1", COOKIE), None);
    }
}
