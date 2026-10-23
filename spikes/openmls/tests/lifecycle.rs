//! Q3 + Q5: group lifecycle with owner-signed device credentials.
use openmls_spike::*;

fn all(_: &[u8; 32]) -> bool {
    true
}

#[test]
fn lifecycle_add_send_remove() {
    // Alice (2 devices), Bob, Carol, Dave: 5 leaves.
    let (oa, ob, oc, od) = (Owner::random(), Owner::random(), Owner::random(), Owner::random());
    let known = vec![oa.public(), ob.public(), oc.public(), od.public()];
    let trusted = move |k: &[u8; 32]| known.contains(k);

    let mut a1 = Client::new(&oa, [1; 16]);
    let mut a2 = Client::new(&oa, [2; 16]);
    let mut b = Client::new(&ob, [3; 16]);
    let mut c = Client::new(&oc, [4; 16]);
    let mut d = Client::new(&od, [5; 16]);

    a1.create_group();
    // Add a2, b, c in one commit; one Welcome for all three.
    let kps = [a2.key_package(), b.key_package(), c.key_package()];
    let (_commit, welcome) = a1.add(&kps);
    let tree = a1.tree();
    for m in [&mut a2, &mut b, &mut c] {
        m.join(&welcome, tree.clone(), &trusted).unwrap();
    }

    // Everyone can talk.
    let m1 = b.send(b"hello group");
    for r in [&mut a1, &mut a2, &mut c] {
        match r.process(&m1, &trusted).unwrap() {
            Processed::App(t) => assert_eq!(t, b"hello group"),
            _ => panic!(),
        }
    }

    // Add Dave (second commit, second Welcome); a1 commits, others process.
    let (commit, welcome) = a1.add(&[d.key_package()]);
    for r in [&mut a2, &mut b, &mut c] {
        r.process(&commit, &trusted).unwrap();
    }
    d.join(&welcome, a1.tree(), &trusted).unwrap();

    // Remove Carol (leaf index 3: a1=0, a2=1, b=2, c=3).
    let c_idx = c.group.as_ref().unwrap().own_leaf_index();
    assert_eq!(c_idx.u32(), 3);
    let commit = a1.remove(&[c_idx]);
    for r in [&mut a2, &mut b, &mut d] {
        assert!(matches!(r.process(&commit, &trusted).unwrap(), Processed::Commit { removed_me: false }));
    }
    // Carol processes her own removal, and learns she is out.
    assert!(matches!(c.process(&commit, &trusted).unwrap(), Processed::Commit { removed_me: true }));
    assert!(!c.group.as_ref().unwrap().is_active());

    // A later message: remaining members decrypt, Carol cannot.
    let m2 = d.send(b"after removal");
    for r in [&mut a1, &mut a2, &mut b] {
        match r.process(&m2, &trusted).unwrap() {
            Processed::App(t) => assert_eq!(t, b"after removal"),
            _ => panic!(),
        }
    }
    assert!(c.process(&m2, &trusted).is_err(), "removed member must not decrypt");
    println!("lifecycle ok; epoch {}", a1.group.as_ref().unwrap().epoch().as_u64());
}

#[test]
fn credential_binding_is_enforced() {
    let (oa, ob) = (Owner::random(), Owner::random());
    let mallory = Owner::random(); // not trusted
    let known = vec![oa.public(), ob.public()];
    let trusted = move |k: &[u8; 32]| known.contains(k);

    let mut a = Client::new(&oa, [1; 16]);
    a.create_group();

    // 1. Unknown owner: Welcome is rejected by the joiner, commit by existing members.
    let mut m = Client::new(&mallory, [9; 16]);
    let (_c, w) = a.add(&[m.key_package()]);
    assert!(m.join(&w, a.tree(), &|_: &[u8; 32]| false).is_err());

    // 2. Leaf claims owner B but the identity was not signed by B for THIS leaf key:
    //    take B's genuine certificate for another key and splice it onto a new leaf.
    a = Client::new(&oa, [1; 16]); // fresh group without mallory
    a.create_group();
    let mut b = Client::new(&ob, [2; 16]);
    let stolen_ident = b.cwk.credential.clone();
    let mut evil = Client::new(&oa, [3; 16]); // fresh signature key
    evil.cwk.credential = stolen_ident; // credential from b, key from evil
    let kp = evil.key_package();
    let mut obs = Client::new(&oa, [4; 16]);
    // obs joins first so it can process the next commit
    let (_c, w) = a.add(&[obs.key_package()]);
    obs.join(&w, a.tree(), &trusted).unwrap();
    let (commit, _w) = a.add(&[kp]); // a (committer) is not validating here: obs must
    let err = obs.process(&commit, &trusted).err().expect("must reject spliced credential");
    assert!(err.contains("owner signature"), "got: {err}");
    let _ = &mut b;
    let _ = all;
}
