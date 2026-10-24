//! A small TURN server for tests and demos (ADR 0028): it accepts exactly the logins the relay issues from a
//! shared secret (the same scheme as coturn's `use-auth-secret`), so a call can be proved to work through a
//! TURN relay without installing coturn. It is a stand-in, not the thing to run in production: for that, use
//! coturn (docs/turn.md).
//!
//!   cargo run -p spike-relay --example turn_test_server -- --public-ip 127.0.0.1 --secret SECRET [--port 3478]

use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::UdpSocket;
use turn::auth::{generate_auth_key, AuthHandler};
use turn::relay::relay_static::RelayAddressGeneratorStatic;
use turn::server::config::{ConnConfig, ServerConfig};
use turn::server::Server;
use turn::Error;
use webrtc_util::vnet::net::Net;

const REALM: &str = "spike";

struct RestAuth {
    secret: Vec<u8>,
}

impl AuthHandler for RestAuth {
    fn auth_handle(&self, username: &str, realm: &str, _src: SocketAddr) -> Result<Vec<u8>, Error> {
        let (expiry, _) = username.split_once(':').ok_or(Error::ErrFakeErr)?;
        let expiry: u64 = expiry.parse().map_err(|_| Error::ErrFakeErr)?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        if expiry < now {
            return Err(Error::ErrFakeErr);
        }
        // The password the relay derived for this username; the long-term key is MD5(user:realm:password).
        use base64::Engine;
        let password = base64::engine::general_purpose::STANDARD.encode(spike_core::turn::hmac_sha1(&self.secret, username.as_bytes()));
        Ok(generate_auth_key(username, realm, &password))
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let mut args = std::env::args().skip(1);
    let (mut ip, mut secret, mut port) = (String::from("127.0.0.1"), String::new(), String::from("3478"));
    while let Some(a) = args.next() {
        match a.as_str() {
            "--public-ip" => ip = args.next().unwrap_or_default(),
            "--secret" => secret = args.next().unwrap_or_default(),
            "--port" => port = args.next().unwrap_or_default(),
            _ => {}
        }
    }
    if secret.is_empty() {
        eprintln!("usage: turn_test_server --public-ip IP --secret SECRET [--port 3478]");
        std::process::exit(2);
    }
    let conn = Arc::new(UdpSocket::bind(format!("0.0.0.0:{port}")).await?);
    println!("turn test server listening on {} (realm {REALM})", conn.local_addr()?);
    let server = Server::new(ServerConfig {
        conn_configs: vec![ConnConfig {
            conn,
            relay_addr_generator: Box::new(RelayAddressGeneratorStatic {
                relay_address: IpAddr::from_str(&ip).map_err(|_| Error::ErrFakeErr)?,
                address: "0.0.0.0".to_owned(),
                net: Arc::new(Net::new(None)),
            }),
        }],
        realm: REALM.to_owned(),
        auth_handler: Arc::new(RestAuth { secret: secret.into_bytes() }),
        channel_bind_timeout: Duration::from_secs(0),
        alloc_close_notify: None,
    })
    .await?;
    tokio::signal::ctrl_c().await.ok();
    server.close().await?;
    Ok(())
}
