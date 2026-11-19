//! `spike org-admin` and `spike org-token` with the real binary: the token printed on standard output is a
//! valid ADR 0035 enrolment token, and the QR code drawn on standard error encodes exactly that text.

use std::process::{Command, Output};

fn spike(home: &std::path::Path, args: &[&str]) -> Output {
    let out = Command::new(env!("CARGO_BIN_EXE_spike"))
        .arg("--home")
        .arg(home)
        .args(args)
        .env("SPIKE_PASSPHRASE", "test passphrase for org qr")
        .env_remove("SPIKE_RELAY")
        .output()
        .expect("run spike");
    assert!(
        out.status.success(),
        "spike {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

#[test]
fn org_token_prints_the_token_and_draws_the_same_text_as_a_qr_code() {
    let home = std::env::temp_dir().join(format!("spike-org-qr-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    spike(&home, &["init"]);

    let admin = spike(
        &home,
        &[
            "org-admin",
            "--tenant",
            "northwind",
            "--name",
            "Northwind",
            "--relay",
            "wss://relay.example",
        ],
    );
    let admin_pub = String::from_utf8(admin.stdout).unwrap();
    assert_eq!(
        admin_pub.trim().len(),
        64,
        "the admin public key is printed as 64 hex digits"
    );

    let out = spike(
        &home,
        &["org-token", "--label", "Ana's phone", "--qr", "always"],
    );
    let token = String::from_utf8(out.stdout).unwrap().trim().to_string();
    assert!(token.starts_with(spike_core::tenant::TOKEN_PREFIX));
    let body = spike_cli::org::inspect(&token).expect("a valid enrolment token");
    assert_eq!(body.tenant, "northwind");

    let err = String::from_utf8(out.stderr).unwrap();
    let drawn: String = err
        .lines()
        .filter(|l| l.starts_with("\x1b[30;107m"))
        .map(|l| format!("{l}\n"))
        .collect();
    assert!(!drawn.is_empty(), "a QR code is drawn with --qr always");
    assert_eq!(
        spike_cli::qr::read_terminal(&drawn),
        spike_cli::qr::matrix(&token).unwrap(),
        "the drawn code is the printed token"
    );

    // Not a terminal and the default `auto`: the text only, no drawing.
    let plain = spike(&home, &["org-token"]);
    assert!(!String::from_utf8(plain.stderr)
        .unwrap()
        .contains("\x1b[30;107m"));
    let _ = std::fs::remove_dir_all(&home);
}
