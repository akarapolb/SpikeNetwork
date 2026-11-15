//! Tenants and enrollment tokens (ADR 0023): how a relay learns "device X belongs to tenant T"
//! and nothing more.
//!
//! A **tenant** (an organisation using a shared relay) has an **admin key**, an Ed25519 key the
//! customer holds. The relay operator registers its public half. The admin key signs:
//! - an [`EnrollToken`]: "device X may join tenant T; redeem it before `not_after`". It is bound to
//!   one device id, so a leaked token admits nobody else. Redeeming it creates an enrollment that
//!   lasts until it is revoked.
//! - a [`Revocation`]: "device X no longer belongs to tenant T".
//!
//! The relay operator has a separate key that signs [`AdminRequest`]s (create a tenant, set its
//! limits, suspend, resume, delete, read usage).
//!
//! Nothing here names a person. Tokens carry a tenant id, a device id (which the relay sees anyway
//! when the device signs in), an expiry and a random token id. Every signature has its own context
//! string and length-prefixed fields, so no signature can be reused as another.

use crate::{Error, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use vodozemac::{Ed25519PublicKey, Ed25519SecretKey, Ed25519Signature};

const TOKEN_CTX: &[u8] = b"spike-enroll-v1";
const REVOKE_CTX: &[u8] = b"spike-enroll-revoke-v1";
const ADMIN_CTX: &[u8] = b"spike-relay-admin-v1";
const TOKEN_PREFIX: &str = "spike-enroll1:";
const REVOKE_PREFIX: &str = "spike-revoke1:";

/// How long a token may be redeemed for by default (ADR 0023: short-lived to redeem).
pub const DEFAULT_REDEEM_SECS: u64 = 7 * 24 * 3600;

/// A tenant id: 1 to 64 of `a-z`, `0-9`, `-`, `_`. Chosen by the operator; an opaque id is better
/// than the organisation's name, because the relay stores it next to every enrolled device.
pub fn is_valid_tenant_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

fn is_hex_of(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn frame(parts: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for p in parts {
        out.extend_from_slice(&(p.len() as u32).to_be_bytes());
        out.extend_from_slice(p);
    }
    out
}

/// Check a hex Ed25519 signature over `msg` against a hex public key.
pub fn verify_hex(pub_hex: &str, msg: &[u8], sig_hex: &str) -> Result<()> {
    let pk: [u8; 32] = hex::decode(pub_hex)?
        .try_into()
        .map_err(|_| Error::BadSignature)?;
    let sig: [u8; 64] = hex::decode(sig_hex)?
        .try_into()
        .map_err(|_| Error::BadSignature)?;
    let pk = Ed25519PublicKey::from_slice(&pk).map_err(|_| Error::BadSignature)?;
    let sig = Ed25519Signature::from_slice(&sig).map_err(|_| Error::BadSignature)?;
    pk.verify(msg, &sig).map_err(|_| Error::BadSignature)
}

/// An Ed25519 signing key used for enrollment: a tenant's admin key or a relay's operator key.
/// The relay only ever holds the public halves.
pub struct SigningKey(Ed25519SecretKey);

impl SigningKey {
    pub fn generate() -> Self {
        Self(Ed25519SecretKey::new())
    }

    pub fn from_hex(s: &str) -> Result<Self> {
        let b: [u8; 32] = hex::decode(s.trim())?
            .try_into()
            .map_err(|_| Error::Malformed)?;
        Ok(Self(Ed25519SecretKey::from_slice(&b)))
    }

    pub fn to_hex(&self) -> String {
        hex::encode(*self.0.to_bytes())
    }

    pub fn public_hex(&self) -> String {
        hex::encode(self.0.public_key().as_bytes())
    }

    fn sign(&self, msg: &[u8]) -> String {
        hex::encode(self.0.sign(msg).to_bytes())
    }

    /// Sign a token for `device_id` to join `tenant`, redeemable until `not_after` (Unix seconds).
    pub fn issue(&self, tenant: &str, device_id: &str, not_after: u64) -> Result<EnrollToken> {
        use rand::RngCore;
        let mut id = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut id);
        let mut t = EnrollToken {
            tenant: tenant.into(),
            device_id: device_id.into(),
            not_after,
            token_id: hex::encode(id),
            sig: String::new(),
        };
        t.check_shape()?;
        t.sig = self.sign(&t.signing_bytes());
        Ok(t)
    }

    /// Sign a revocation of `device_id` from `tenant`.
    pub fn revoke(&self, tenant: &str, device_id: &str) -> Result<Revocation> {
        let mut r = Revocation {
            tenant: tenant.into(),
            device_id: device_id.into(),
            sig: String::new(),
        };
        r.check_shape()?;
        r.sig = self.sign(&r.signing_bytes());
        Ok(r)
    }

    /// Sign an operator request. `issued_at` is Unix seconds; `nonce` must be fresh for each request.
    pub fn sign_admin(&self, op: &AdminOp, issued_at: u64) -> Result<AdminRequest> {
        use rand::RngCore;
        let mut nonce = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let body = serde_json::to_string(&AdminBody {
            op: op.clone(),
            issued_at,
            nonce: hex::encode(nonce),
        })?;
        let sig = self.sign(&admin_bytes(&body));
        Ok(AdminRequest { body, sig })
    }
}

/// "Device `device_id` may join tenant `tenant`; redeem before `not_after`."
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct EnrollToken {
    pub tenant: String,
    pub device_id: String,
    /// Unix seconds. After this the token cannot be redeemed; an enrollment made with it stays.
    pub not_after: u64,
    /// 32 hex characters, random. Lets an admin tell tokens apart; identifies nobody.
    pub token_id: String,
    pub sig: String,
}

/// Tokens are credentials: never print one by accident.
impl std::fmt::Debug for EnrollToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnrollToken")
            .field("tenant", &self.tenant)
            .field("not_after", &self.not_after)
            .finish_non_exhaustive()
    }
}

impl EnrollToken {
    fn check_shape(&self) -> Result<()> {
        if !is_valid_tenant_id(&self.tenant)
            || !crate::identity::is_valid_device_id(&self.device_id)
            || !is_hex_of(&self.token_id, 32)
        {
            return Err(Error::Malformed);
        }
        Ok(())
    }

    pub fn signing_bytes(&self) -> Vec<u8> {
        frame(&[
            TOKEN_CTX,
            self.tenant.as_bytes(),
            self.device_id.as_bytes(),
            &self.not_after.to_be_bytes(),
            self.token_id.as_bytes(),
        ])
    }

    /// Check the signature against the tenant's admin public key (hex). Expiry and which device
    /// presents it are for the caller to check.
    pub fn verify(&self, admin_pub_hex: &str) -> Result<()> {
        self.check_shape()?;
        verify_hex(admin_pub_hex, &self.signing_bytes(), &self.sig)
    }

    /// `spike-enroll1:` and the token as URL-safe base64 JSON: one line to paste or put in a QR code.
    pub fn encode(&self) -> String {
        let json = serde_json::to_vec(self).expect("token serializes");
        format!("{TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(json))
    }

    pub fn decode(s: &str) -> Result<Self> {
        let b64 = s
            .trim()
            .strip_prefix(TOKEN_PREFIX)
            .ok_or(Error::Malformed)?;
        if b64.len() > 2048 {
            return Err(Error::Malformed);
        }
        let json = URL_SAFE_NO_PAD.decode(b64).map_err(|_| Error::Malformed)?;
        let t: Self = serde_json::from_slice(&json)?;
        t.check_shape()?;
        Ok(t)
    }
}

/// "Device `device_id` no longer belongs to tenant `tenant`." Signed by the tenant's admin key.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Revocation {
    pub tenant: String,
    pub device_id: String,
    pub sig: String,
}

impl Revocation {
    fn check_shape(&self) -> Result<()> {
        if !is_valid_tenant_id(&self.tenant)
            || !crate::identity::is_valid_device_id(&self.device_id)
        {
            return Err(Error::Malformed);
        }
        Ok(())
    }

    pub fn signing_bytes(&self) -> Vec<u8> {
        frame(&[
            REVOKE_CTX,
            self.tenant.as_bytes(),
            self.device_id.as_bytes(),
        ])
    }

    pub fn verify(&self, admin_pub_hex: &str) -> Result<()> {
        self.check_shape()?;
        verify_hex(admin_pub_hex, &self.signing_bytes(), &self.sig)
    }

    pub fn encode(&self) -> String {
        let json = serde_json::to_vec(self).expect("revocation serializes");
        format!("{REVOKE_PREFIX}{}", URL_SAFE_NO_PAD.encode(json))
    }

    pub fn decode(s: &str) -> Result<Self> {
        let b64 = s
            .trim()
            .strip_prefix(REVOKE_PREFIX)
            .ok_or(Error::Malformed)?;
        if b64.len() > 2048 {
            return Err(Error::Malformed);
        }
        let json = URL_SAFE_NO_PAD.decode(b64).map_err(|_| Error::Malformed)?;
        let r: Self = serde_json::from_slice(&json)?;
        r.check_shape()?;
        Ok(r)
    }
}

/// What a tenant may use. 0 means unlimited (as for the relay-wide limits of ADR 0022).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq)]
pub struct TenantLimits {
    /// Devices that may be enrolled at once.
    #[serde(default)]
    pub devices: u64,
    /// Undelivered messages held for one enrolled device (the relay-wide cap still applies).
    #[serde(default)]
    pub queue_depth: u64,
    /// Attachment bytes all the tenant's devices may have stored at once.
    #[serde(default)]
    pub attachment_bytes: u64,
    /// Connections the tenant's devices may hold open at once.
    #[serde(default)]
    pub connections: u64,
    /// Requests per second all the tenant's connections together may sustain.
    #[serde(default)]
    pub req_rate: f64,
}

impl TenantLimits {
    /// Burst allowed above `req_rate`: four seconds' worth, at least 50 requests.
    pub fn req_burst(&self) -> f64 {
        (self.req_rate * 4.0).max(50.0)
    }
}

/// The names a `tenant_over_limit` error carries, one per limit.
pub mod limit_name {
    pub const DEVICES: &str = "devices";
    pub const QUEUE_DEPTH: &str = "queue_depth";
    pub const ATTACHMENT_BYTES: &str = "attachment_bytes";
    pub const CONNECTIONS: &str = "connections";
    pub const REQ_RATE: &str = "req_rate";
}

/// One thing the relay operator can ask the relay to do (ADR 0023, operator interface).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum AdminOp {
    CreateTenant {
        tenant: String,
        admin_pub: String,
        #[serde(default)]
        limits: TenantLimits,
    },
    SetLimits {
        tenant: String,
        limits: TenantLimits,
    },
    /// Replace a tenant's admin public key (the customer rotated it).
    SetAdminKey {
        tenant: String,
        admin_pub: String,
    },
    Suspend {
        tenant: String,
    },
    Resume {
        tenant: String,
    },
    /// Remove the tenant, its enrollments and everything stored for its devices.
    Delete {
        tenant: String,
    },
    /// Usage numbers for one tenant, or all if `tenant` is `None`.
    Usage {
        #[serde(default)]
        tenant: Option<String>,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AdminBody {
    #[serde(flatten)]
    pub op: AdminOp,
    pub issued_at: u64,
    pub nonce: String,
}

/// An operator request: the JSON `body` exactly as signed, and the operator key's signature.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AdminRequest {
    pub body: String,
    pub sig: String,
}

fn admin_bytes(body: &str) -> Vec<u8> {
    frame(&[ADMIN_CTX, body.as_bytes()])
}

impl AdminRequest {
    /// Check the signature against the operator's public key and parse the body. Freshness
    /// (`issued_at`, `nonce`) is for the caller to check.
    pub fn open(&self, operator_pub_hex: &str) -> Result<AdminBody> {
        if self.body.len() > 16 * 1024 {
            return Err(Error::Malformed);
        }
        verify_hex(operator_pub_hex, &admin_bytes(&self.body), &self.sig)?;
        let b: AdminBody = serde_json::from_str(&self.body)?;
        if !is_hex_of(&b.nonce, 32) {
            return Err(Error::Malformed);
        }
        Ok(b)
    }
}

/// Per-tenant numbers the operator can read. Counts and bytes only, never device ids.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct TenantUsage {
    pub tenant: String,
    /// `active` or `suspended`.
    pub state: String,
    pub limits: TenantLimits,
    pub devices: u64,
    pub connections: u64,
    pub queued_messages: u64,
    pub queued_bytes: u64,
    pub attachment_bytes: u64,
    /// Requests refused because a tenant limit was reached, since the relay started.
    pub refused_over_limit: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;

    fn device() -> String {
        Identity::generate().unwrap().device_id()
    }

    #[test]
    fn a_token_round_trips_and_verifies_only_under_its_admin_key() {
        let admin = SigningKey::generate();
        let d = device();
        let t = admin.issue("acme", &d, 1_000).unwrap();
        let back = EnrollToken::decode(&t.encode()).unwrap();
        assert_eq!(back, t);
        back.verify(&admin.public_hex()).unwrap();
        assert!(back.verify(&SigningKey::generate().public_hex()).is_err());
    }

    #[test]
    fn changing_any_field_breaks_the_signature() {
        let admin = SigningKey::generate();
        let t = admin.issue("acme", &device(), 1_000).unwrap();
        let pk = admin.public_hex();
        let mut x = t.clone();
        x.tenant = "other".into();
        assert!(x.verify(&pk).is_err());
        let mut x = t.clone();
        x.device_id = device();
        assert!(x.verify(&pk).is_err());
        let mut x = t.clone();
        x.not_after += 1;
        assert!(x.verify(&pk).is_err());
        let mut x = t.clone();
        x.token_id = "0".repeat(32);
        assert!(x.verify(&pk).is_err());
    }

    #[test]
    fn signatures_are_not_interchangeable_between_kinds() {
        let admin = SigningKey::generate();
        let d = device();
        let r = admin.revoke("acme", &d).unwrap();
        r.verify(&admin.public_hex()).unwrap();
        // A token's signature does not make a revocation, and the reverse.
        let t = admin.issue("acme", &d, 5).unwrap();
        let forged = Revocation {
            sig: t.sig.clone(),
            ..r.clone()
        };
        assert!(forged.verify(&admin.public_hex()).is_err());
        assert_eq!(Revocation::decode(&r.encode()).unwrap(), r);
    }

    #[test]
    fn malformed_tokens_are_refused() {
        let admin = SigningKey::generate();
        assert!(admin.issue("Acme Corp", &device(), 5).is_err());
        assert!(admin.issue("acme", "not-a-device", 5).is_err());
        assert!(EnrollToken::decode("spike-enroll1:!!!").is_err());
        assert!(EnrollToken::decode("something else").is_err());
    }

    #[test]
    fn admin_requests_verify_under_the_operator_key_only() {
        let op = SigningKey::generate();
        let req = op
            .sign_admin(
                &AdminOp::Suspend {
                    tenant: "acme".into(),
                },
                77,
            )
            .unwrap();
        let body = req.open(&op.public_hex()).unwrap();
        assert_eq!(body.issued_at, 77);
        assert!(matches!(body.op, AdminOp::Suspend { .. }));
        assert!(req.open(&SigningKey::generate().public_hex()).is_err());
        let tampered = AdminRequest {
            body: req.body.replace("suspend", "delete"),
            sig: req.sig.clone(),
        };
        assert!(tampered.open(&op.public_hex()).is_err());
    }

    #[test]
    fn debug_output_hides_the_token() {
        let admin = SigningKey::generate();
        let t = admin.issue("acme", &device(), 5).unwrap();
        let shown = format!("{t:?}");
        assert!(!shown.contains(&t.sig) && !shown.contains(&t.token_id));
    }
}
