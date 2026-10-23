// Size accounting baseline: what the repo already links (vodozemac + ed25519), no MLS.
use ed25519_dalek::{Signer, SigningKey};
fn main() {
    let k = SigningKey::from_bytes(&rand::random::<[u8; 32]>());
    println!("{:?}", k.sign(b"x").to_bytes()[0]);
    let acct = vodozemac::olm::Account::new();
    println!("{:?}", acct.curve25519_key());
}
