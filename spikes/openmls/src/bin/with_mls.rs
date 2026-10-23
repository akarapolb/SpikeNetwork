// Size accounting: a tiny program that creates a group and sends one message (openmls linked in).
use openmls_spike::*;
fn main() {
    let o = Owner::random();
    let mut c = Client::new(&o, [0; 16]);
    c.create_group();
    println!("{}", c.send(b"x").len());
    // keep the same non-MLS dependencies as the baseline
    let acct = vodozemac::olm::Account::new();
    println!("{:?}", acct.curve25519_key());
}
