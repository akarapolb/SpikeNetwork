//! The relay binary refuses an unsafe configuration instead of starting.

use std::process::Command;

fn relay() -> Command {
    Command::new(env!("CARGO_BIN_EXE_spike-relay"))
}

#[test]
fn refuses_plain_websocket_on_a_public_address() {
    let out = relay().args(["--listen", "0.0.0.0:0"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("refusing to serve plain WebSocket"), "{err}");
}

#[test]
fn the_group_mailbox_limits_are_flags() {
    let out = relay().arg("--help").output().unwrap();
    let help = String::from_utf8_lossy(&out.stdout);
    for flag in [
        "--group-max-entry",
        "--group-message-days",
        "--group-handshake-days",
        "--group-max-bytes",
        "--group-max-mailboxes-per-device",
        "--max-group-subs-per-conn",
    ] {
        assert!(help.contains(flag), "missing {flag}");
    }
}

#[test]
fn tls_flags_must_come_as_a_pair() {
    let out = relay()
        .args(["--tls-cert", "/nonexistent.pem"])
        .output()
        .unwrap();
    assert!(!out.status.success());
}
