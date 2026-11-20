//! Organisations (ADR 0023, ADR 0035): enrolment tokens, organisation rules, and the admin key's
//! sign-in to a relay.
//!
//! Everything here is about *devices*, never people. A token says "whoever holds this may enrol one
//! device in tenant T until time X"; it names no person. Rules are a small signed document the app on
//! a member's device carries out; the relay stores them but cannot forge or roll them back, because a
//! device checks them against the admin key it pinned when it joined and refuses a lower version.
//!
//! Both formats sign the *exact bytes* of a JSON body (carried as a string), so a verifier never has
//! to re-serialise anything to check a signature.

use crate::identity::auth_audience;
use crate::owner::{verify_raw, OwnerKey};
use crate::{Error, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const ENROL_CTX: &[u8] = b"spike-enroll-v2";
const RULES_CTX: &[u8] = b"spike-rules-v1";
const ADMIN_AUTH_CTX: &[u8] = b"spike-admin-auth-v1";
/// What a token looks like when it is copied or put in a QR code.
pub const TOKEN_PREFIX: &str = "spikeorg1:";
/// Default and allowed lifetimes of a token (ADR 0035: 24 hours, 1 hour to 7 days).
pub const TOKEN_DEFAULT_SECS: u64 = 24 * 3600;
pub const TOKEN_MIN_SECS: u64 = 3600;
pub const TOKEN_MAX_SECS: u64 = 7 * 24 * 3600;
/// Longest organisation name a token may carry (it is display only).
pub const MAX_ORG_NAME: usize = 64;
/// Longest tenant id: short lower-case letters, digits and dashes, chosen by the operator.
pub const MAX_TENANT_ID: usize = 32;
/// Largest encoded rules document a relay accepts.
pub const MAX_RULES: usize = 4096;

/// A tenant id is what the operator named the organisation on the relay: `[a-z0-9-]{1,32}`.
pub fn is_valid_tenant_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_TENANT_ID
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn hex32(s: &str) -> Result<[u8; 32]> {
    hex::decode(s)?.try_into().map_err(|_| Error::Malformed)
}

fn hex64(s: &str) -> Result<[u8; 64]> {
    hex::decode(s)?.try_into().map_err(|_| Error::Malformed)
}

/// Short fingerprint of an admin key for people to compare: 20 hex digits in groups of five.
pub fn admin_fingerprint(admin_pub: &[u8; 32]) -> String {
    let d = Sha256::digest([b"spike-admin-fp-v1".as_slice(), admin_pub].concat());
    let h = hex::encode(&d[..10]);
    h.as_bytes()
        .chunks(5)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------------------------------
// Enrolment tokens

/// The signed content of an enrolment token.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TokenBody {
    pub v: u8,
    pub tenant: String,
    /// 16 random bytes, hex. The relay records it when the token is used, so it works once.
    pub token_id: String,
    /// Unix seconds after which the token cannot be used.
    pub not_after: u64,
    /// The organisation's relay (`wss://...`).
    pub relay: String,
    /// The name the organisation chose for itself. Display only, and shown as such.
    pub org_name: String,
    /// The admin public key (hex). The device pins it and trusts rules only from it.
    pub admin_pub: String,
}

/// An enrolment token: the body as exact JSON bytes and the admin key's signature over them.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct EnrolToken {
    pub body: String,
    pub sig: String,
}

impl EnrolToken {
    /// Sign a new token for one device. `valid_secs` is clamped to [`TOKEN_MIN_SECS`]..[`TOKEN_MAX_SECS`].
    pub fn issue(
        admin: &OwnerKey,
        tenant: &str,
        relay: &str,
        org_name: &str,
        valid_secs: u64,
        now: u64,
    ) -> Result<(Self, TokenBody)> {
        if !is_valid_tenant_id(tenant) {
            return Err(Error::BadCard(
                "tenant id must be 1 to 32 of a-z, 0-9 and -",
            ));
        }
        let org_name: String = org_name.trim().chars().take(MAX_ORG_NAME).collect();
        let mut id = [0u8; 16];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut id);
        let body = TokenBody {
            v: 1,
            tenant: tenant.into(),
            token_id: hex::encode(id),
            not_after: now + valid_secs.clamp(TOKEN_MIN_SECS, TOKEN_MAX_SECS),
            relay: relay.trim().into(),
            org_name,
            admin_pub: hex::encode(admin.public()),
        };
        let json = serde_json::to_string(&body).map_err(|_| Error::Malformed)?;
        let sig = admin.sign_raw(&[ENROL_CTX, json.as_bytes()].concat());
        Ok((
            Self {
                body: json,
                sig: hex::encode(sig),
            },
            body,
        ))
    }

    /// `spikeorg1:` + base64url of the JSON: what is copied and put in the QR code.
    pub fn encode(&self) -> String {
        format!(
            "{TOKEN_PREFIX}{}",
            B64.encode(serde_json::to_vec(self).expect("serialises"))
        )
    }

    pub fn decode(s: &str) -> Result<Self> {
        let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        let rest = s.strip_prefix(TOKEN_PREFIX).ok_or(Error::BadCard(
            "not an organisation token (it should start with spikeorg1:)",
        ))?;
        let raw = B64.decode(rest).map_err(|_| Error::Malformed)?;
        if raw.len() > 2048 {
            return Err(Error::TooLarge { max: 2048 });
        }
        serde_json::from_slice(&raw).map_err(|_| Error::Malformed)
    }

    /// Check the signature against the token's own admin key and parse the body. Does not check
    /// expiry (see [`TokenBody::expired`]) or that the key is the tenant's (the relay does that).
    pub fn open(&self) -> Result<TokenBody> {
        let body: TokenBody = serde_json::from_str(&self.body).map_err(|_| Error::Malformed)?;
        if body.v != 1
            || !is_valid_tenant_id(&body.tenant)
            || body.org_name.chars().count() > MAX_ORG_NAME
        {
            return Err(Error::BadCard("unsupported organisation token"));
        }
        let id = hex::decode(&body.token_id)?;
        if id.len() != 16 {
            return Err(Error::Malformed);
        }
        let pk = hex32(&body.admin_pub)?;
        if !verify_raw(
            &pk,
            &[ENROL_CTX, self.body.as_bytes()].concat(),
            &hex64(&self.sig)?,
        ) {
            return Err(Error::BadSignature);
        }
        Ok(body)
    }
}

impl TokenBody {
    pub fn expired(&self, now: u64) -> bool {
        now > self.not_after
    }
    pub fn admin_key(&self) -> Result<[u8; 32]> {
        hex32(&self.admin_pub)
    }
}

// ---------------------------------------------------------------------------------------------
// Rules

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BackupRule {
    Allowed,
    KeyRequired,
    Off,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentRule {
    Any,
    VerifiedOnly,
    Off,
}

/// The rules an organisation sets for the app on its members' devices (ADR 0035). A missing field
/// means "no rule". New kinds of rule need a new ADR: each is a power over a person's phone.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct Rules {
    pub v: u8,
    pub tenant: String,
    /// Only goes up. A device refuses a version lower than the one it holds.
    pub version: u64,
    pub issued_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_idle_minutes: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_messages_days: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backups: Option<BackupRule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents: Option<AgentRule>,
}

impl Rules {
    fn check(&self) -> Result<()> {
        if self.v != 1 || !is_valid_tenant_id(&self.tenant) || self.version == 0 {
            return Err(Error::BadCard("unsupported organisation rules"));
        }
        if matches!(self.lock_idle_minutes, Some(0) | Some(1441..))
            || matches!(self.keep_messages_days, Some(0) | Some(36501..))
        {
            return Err(Error::BadCard("rule out of range"));
        }
        Ok(())
    }
}

/// Rules as exact JSON bytes plus the admin key's signature.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SignedRules {
    pub body: String,
    pub sig: String,
}

impl SignedRules {
    pub fn sign(admin: &OwnerKey, rules: &Rules) -> Result<Self> {
        rules.check()?;
        let body = serde_json::to_string(rules).map_err(|_| Error::Malformed)?;
        let sig = admin.sign_raw(&[RULES_CTX, body.as_bytes()].concat());
        Ok(Self {
            body,
            sig: hex::encode(sig),
        })
    }

    pub fn encode(&self) -> String {
        serde_json::to_string(self).expect("serialises")
    }

    pub fn decode(s: &str) -> Result<Self> {
        if s.len() > MAX_RULES {
            return Err(Error::TooLarge { max: MAX_RULES });
        }
        serde_json::from_str(s).map_err(|_| Error::Malformed)
    }

    /// Verify against the admin key and tenant the device pinned, and parse.
    pub fn open(&self, admin_pub: &[u8; 32], tenant: &str) -> Result<Rules> {
        if !verify_raw(
            admin_pub,
            &[RULES_CTX, self.body.as_bytes()].concat(),
            &hex64(&self.sig)?,
        ) {
            return Err(Error::BadSignature);
        }
        let r: Rules = serde_json::from_str(&self.body).map_err(|_| Error::Malformed)?;
        r.check()?;
        if r.tenant != tenant {
            return Err(Error::BadCard("rules are for another organisation"));
        }
        Ok(r)
    }

    /// What a device does with rules a relay hands it: verify, then refuse anything older than what
    /// it already holds. Returns `None` when the offered version is the one it has.
    pub fn accept(
        &self,
        admin_pub: &[u8; 32],
        tenant: &str,
        held_version: u64,
    ) -> Result<Option<Rules>> {
        let r = self.open(admin_pub, tenant)?;
        if r.version < held_version {
            return Err(Error::Rollback {
                seen: held_version,
                offered: r.version,
            });
        }
        Ok((r.version > held_version).then_some(r))
    }
}

// ---------------------------------------------------------------------------------------------
// Admin sign-in and leaving

fn ctx_message(ctx: &[u8], tenant: &str, nonce: &[u8], audience: &str) -> Vec<u8> {
    let audience = auth_audience(audience);
    let mut m = Vec::new();
    m.extend_from_slice(ctx);
    for part in [tenant.as_bytes(), audience.as_bytes()] {
        m.extend_from_slice(&(part.len() as u16).to_be_bytes());
        m.extend_from_slice(part);
    }
    m.extend_from_slice(nonce);
    m
}

/// The admin key's answer to a relay challenge. Its own context, so it can never be confused with a
/// device's sign-in, and it names the tenant.
pub fn sign_admin_auth(admin: &OwnerKey, tenant: &str, nonce: &[u8], audience: &str) -> [u8; 64] {
    admin.sign_raw(&ctx_message(ADMIN_AUTH_CTX, tenant, nonce, audience))
}

pub fn verify_admin_auth(
    admin_pub: &[u8; 32],
    tenant: &str,
    nonce: &[u8],
    audience: &str,
    sig: &[u8; 64],
) -> bool {
    verify_raw(
        admin_pub,
        &ctx_message(ADMIN_AUTH_CTX, tenant, nonce, audience),
        sig,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    #[test]
    fn a_token_round_trips_and_verifies() {
        let k = OwnerKey::generate();
        let (t, body) = EnrolToken::issue(
            &k,
            "northwind",
            "wss://relay.example",
            "Northwind",
            TOKEN_DEFAULT_SECS,
            NOW,
        )
        .unwrap();
        let s = t.encode();
        assert!(s.starts_with(TOKEN_PREFIX));
        let back = EnrolToken::decode(&format!("  {s}\n")).unwrap();
        let opened = back.open().unwrap();
        assert_eq!(opened, body);
        assert_eq!(opened.not_after, NOW + 24 * 3600);
        assert!(!opened.expired(NOW + 24 * 3600));
        assert!(opened.expired(NOW + 24 * 3600 + 1));
        assert_eq!(opened.admin_key().unwrap(), k.public());
    }

    #[test]
    fn token_lifetime_is_clamped_and_ids_differ() {
        let k = OwnerKey::generate();
        let (_, a) = EnrolToken::issue(&k, "t", "wss://r", "T", 10, NOW).unwrap();
        let (_, b) = EnrolToken::issue(&k, "t", "wss://r", "T", 999 * 24 * 3600, NOW).unwrap();
        assert_eq!(a.not_after, NOW + TOKEN_MIN_SECS);
        assert_eq!(b.not_after, NOW + TOKEN_MAX_SECS);
        assert_ne!(a.token_id, b.token_id);
    }

    #[test]
    fn a_tampered_token_is_refused() {
        let k = OwnerKey::generate();
        let (t, _) = EnrolToken::issue(
            &k,
            "northwind",
            "wss://relay.example",
            "Northwind",
            3600,
            NOW,
        )
        .unwrap();
        // Change the expiry inside the signed body.
        let mut bad = t.clone();
        bad.body = bad
            .body
            .replace(&format!("{}", NOW + 3600), &format!("{}", NOW + 999_999));
        assert!(matches!(bad.open(), Err(Error::BadSignature)));
        // Swap in another admin key (re-signing is impossible without the key).
        let other = OwnerKey::generate();
        let mut swapped = t.clone();
        swapped.body = swapped
            .body
            .replace(&hex::encode(k.public()), &hex::encode(other.public()));
        assert!(swapped.open().is_err());
        assert!(EnrolToken::decode("spike2:abc").is_err());
        assert!(EnrolToken::decode("spikeorg1:!!!").is_err());
    }

    #[test]
    fn bad_tenant_ids_are_refused() {
        let k = OwnerKey::generate();
        for bad in ["", "North", "a b", "x".repeat(33).as_str(), "a/b"] {
            assert!(
                EnrolToken::issue(&k, bad, "wss://r", "T", 3600, NOW).is_err(),
                "{bad}"
            );
        }
    }

    fn rules(version: u64) -> Rules {
        Rules {
            v: 1,
            tenant: "northwind".into(),
            version,
            issued_at: NOW,
            lock_idle_minutes: Some(5),
            keep_messages_days: Some(90),
            backups: Some(BackupRule::KeyRequired),
            agents: Some(AgentRule::VerifiedOnly),
        }
    }

    #[test]
    fn rules_verify_against_the_pinned_key_and_tenant_only() {
        let k = OwnerKey::generate();
        let s = SignedRules::sign(&k, &rules(3)).unwrap();
        let back = SignedRules::decode(&s.encode()).unwrap();
        assert_eq!(back.open(&k.public(), "northwind").unwrap(), rules(3));
        assert!(matches!(
            back.open(&OwnerKey::generate().public(), "northwind"),
            Err(Error::BadSignature)
        ));
        assert!(back.open(&k.public(), "other").is_err());
        let mut forged = back.clone();
        forged.body = forged
            .body
            .replace("\"keep_messages_days\":90", "\"keep_messages_days\":1");
        assert!(forged.open(&k.public(), "northwind").is_err());
    }

    #[test]
    fn rules_cannot_be_rolled_back_and_same_version_is_a_no_op() {
        let k = OwnerKey::generate();
        let v3 = SignedRules::sign(&k, &rules(3)).unwrap();
        assert!(v3.accept(&k.public(), "northwind", 2).unwrap().is_some());
        assert!(v3.accept(&k.public(), "northwind", 3).unwrap().is_none());
        assert!(matches!(
            v3.accept(&k.public(), "northwind", 4),
            Err(Error::Rollback {
                seen: 4,
                offered: 3
            })
        ));
    }

    #[test]
    fn out_of_range_rules_are_refused() {
        let k = OwnerKey::generate();
        let mut r = rules(1);
        r.lock_idle_minutes = Some(0);
        assert!(SignedRules::sign(&k, &r).is_err());
        let mut r = rules(1);
        r.version = 0;
        assert!(SignedRules::sign(&k, &r).is_err());
        let empty = Rules {
            v: 1,
            tenant: "northwind".into(),
            version: 1,
            issued_at: NOW,
            ..Default::default()
        };
        let s = SignedRules::sign(&k, &empty).unwrap();
        assert_eq!(s.open(&k.public(), "northwind").unwrap().backups, None);
    }

    #[test]
    fn admin_auth_is_bound_to_tenant_audience_and_context() {
        let k = OwnerKey::generate();
        let sig = sign_admin_auth(&k, "northwind", b"nonce", "relay.example:443");
        assert!(verify_admin_auth(
            &k.public(),
            "northwind",
            b"nonce",
            "relay.example",
            &sig
        ));
        assert!(!verify_admin_auth(
            &k.public(),
            "other",
            b"nonce",
            "relay.example",
            &sig
        ));
        assert!(!verify_admin_auth(
            &k.public(),
            "northwind",
            b"nonce",
            "evil.example",
            &sig
        ));
        assert!(!verify_admin_auth(
            &k.public(),
            "northwind",
            b"other",
            "relay.example",
            &sig
        ));
        // A device sign-in answer is not an admin sign-in, even made by the same key bytes.
        let raw = k.sign_raw(b"nonce");
        assert!(!verify_admin_auth(
            &k.public(),
            "northwind",
            b"nonce",
            "relay.example",
            &raw
        ));
    }

    #[test]
    fn fingerprints_are_grouped() {
        let fp = admin_fingerprint(&[7u8; 32]);
        assert_eq!(fp.len(), 23);
        assert_eq!(fp.split(' ').count(), 4);
    }
}
