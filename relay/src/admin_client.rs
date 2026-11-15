//! The operator's side of the operator interface (ADR 0023): sign an [`AdminOp`] with the operator
//! key and `POST` it to a relay's operator listener (`--metrics-listen`). Plain HTTP: keep that
//! listener on loopback or a private network, as for `/metrics`. The signature, a timestamp and a
//! one-time nonce are what authorise a request, so a captured one cannot be replayed or altered.

use spike_core::enroll::{AdminOp, SigningKey};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// What the relay answered.
#[derive(Debug)]
pub struct Answer {
    pub ok: bool,
    /// `result` on success, `error` (a sentence) otherwise.
    pub body: serde_json::Value,
}

/// Send one signed request to `addr` (`host:port` of the operator listener).
pub async fn send(addr: &str, key: &SigningKey, op: &AdminOp) -> Result<Answer, String> {
    let req = key
        .sign_admin(op, crate::tenancy::now_secs())
        .map_err(|e| e.to_string())?;
    let body = serde_json::to_string(&req).map_err(|e| e.to_string())?;
    let mut sock = tokio::net::TcpStream::connect(addr)
        .await
        .map_err(|e| format!("connecting to {addr}: {e}"))?;
    let head = format!(
        "POST /admin HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    sock.write_all(head.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    sock.write_all(body.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        sock.read_to_end(&mut raw),
    )
    .await
    .map_err(|_| "the relay did not answer within 10 seconds".to_string())?
    .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&raw);
    let json = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .ok_or("not an HTTP answer")?;
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|_| format!("unexpected answer: {json}"))?;
    let ok = v.get("ok").and_then(|o| o.as_bool()).unwrap_or(false);
    let body = if ok {
        v.get("result").cloned().unwrap_or_default()
    } else {
        v.get("error").cloned().unwrap_or_default()
    };
    Ok(Answer { ok, body })
}
