//! Short-lived credentials for a TURN server (ADR 0028, phase 3), in the scheme coturn calls
//! `use-auth-secret` (the "TURN REST API"): the relay and the TURN server share a secret; the relay hands a
//! signed-in device a username that carries its own expiry, and the password is a MAC of that username. The
//! TURN server checks the MAC and the expiry without asking the relay anything. A stolen credential stops
//! working when it expires, and a device that is not signed in to the relay cannot get one.

use base64::{engine::general_purpose::STANDARD, Engine};
use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;

type HmacSha1 = Hmac<Sha1>;

/// HMAC-SHA1, as the TURN REST scheme and coturn define it.
pub fn hmac_sha1(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut m = HmacSha1::new_from_slice(key).expect("hmac takes any key length");
    m.update(data);
    m.finalize().into_bytes().to_vec()
}

/// A username and password valid until `now_secs + ttl_secs`. The part of the username after the colon is
/// only a label for the TURN server's own logs: the first 8 characters of the device id (never the full id).
pub fn credentials(
    secret: &[u8],
    device_id: &str,
    now_secs: u64,
    ttl_secs: u64,
) -> (String, String) {
    let label: String = device_id
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .take(8)
        .collect();
    let username = format!("{}:{}", now_secs + ttl_secs, label);
    let password = STANDARD.encode(hmac_sha1(secret, username.as_bytes()));
    (username, password)
}

/// What a TURN server does with a login: true if `password` matches `username` and the expiry in it has not passed.
pub fn verify(secret: &[u8], username: &str, password: &str, now_secs: u64) -> bool {
    let Some((expiry, _)) = username.split_once(':') else {
        return false;
    };
    let Ok(expiry) = expiry.parse::<u64>() else {
        return false;
    };
    if expiry < now_secs {
        return false;
    }
    let expected = STANDARD.encode(hmac_sha1(secret, username.as_bytes()));
    // Same length, so compare without an early exit.
    expected.len() == password.len()
        && expected
            .bytes()
            .zip(password.bytes())
            .fold(0u8, |a, (x, y)| a | (x ^ y))
            == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_sha1_matches_the_rfc_2202_vector() {
        let mac = hmac_sha1(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(hex::encode(mac), "effcdf6ae5eb2fa2d27416d5f184df9c259a7c79");
    }

    #[test]
    fn a_credential_verifies_until_it_expires_and_not_for_others() {
        let secret = b"shared secret";
        let (user, pass) = credentials(secret, &"ab".repeat(16), 1_000, 3_600);
        assert_eq!(user, "4600:abababab");
        assert!(verify(secret, &user, &pass, 1_000));
        assert!(verify(secret, &user, &pass, 4_600));
        assert!(!verify(secret, &user, &pass, 4_601), "expired");
        assert!(
            !verify(b"another secret", &user, &pass, 1_000),
            "wrong secret"
        );
        // changing the expiry (to live longer) breaks the MAC
        assert!(!verify(secret, "9999999:abababab", &pass, 1_000));
        assert!(!verify(secret, &user, "AAAA", 1_000));
        assert!(!verify(secret, "nocolon", &pass, 1_000));
        assert!(!verify(secret, "x:abababab", &pass, 1_000));
    }

    #[test]
    fn the_username_never_carries_more_than_a_short_label_of_the_device() {
        let (user, _) = credentials(b"s", &"0123456789abcdef".repeat(4), 0, 10);
        assert_eq!(user, "10:01234567");
    }
}
