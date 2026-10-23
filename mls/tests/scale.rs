//! How the cost of one group change grows with the group (ADR 0025, "Scale not measured").
//! Run by hand: `cargo test -p spike-mls --release --test scale -- --ignored --nocapture`.

mod support;

use spike_core::vault::{KdfParams, Vault};
use spike_mls::group::{Device, Group, Processed};
use spike_mls::policy::Policy;
use spike_mls::state;
use std::time::{Duration, Instant};
use support::{directory, Person};

fn ms(d: Duration) -> String {
    format!("{:.1} ms", d.as_secs_f64() * 1000.0)
}

#[test]
#[ignore = "measurement, run by hand"]
fn cost_of_one_change_by_group_size() {
    const SIZES: [usize; 4] = [20, 100, 250, 500];
    let people: Vec<Person> = (0..SIZES[3] + 2).map(|_| Person::new(1)).collect();
    let refs: Vec<&Person> = people.iter().collect();
    let dir = directory(&refs);
    let devices: Vec<Device> = people.iter().map(|p| p.mls_device(0)).collect();
    let vault = Vault::create("pw", KdfParams::insecure_for_tests())
        .unwrap()
        .0;

    let mut policy = Policy::new(vec![people[0].owner_pub()]);
    policy.max_people = 500;
    // The admin is member 0; member 1 follows every commit as an ordinary member.
    let mut ga = Group::create(&devices[0], policy).unwrap();
    let mut gb: Option<Group> = None;
    let mut size = 1usize;

    println!("\n| members | add one: commit | welcome | create | observer applies | join | remove one: commit | observer applies | admin state sealed | seal |");
    println!("|---|---|---|---|---|---|---|---|---|---|");
    for target in SIZES {
        // Grow to target - 1 in batches of 50, so the last single add lands on `target`.
        while size < target - 1 {
            let n = (target - 1 - size).min(50);
            let kps: Vec<_> = (size..size + n).map(|i| devices[i].key_package()).collect();
            let out = ga.add(&devices[0], &dir, &kps).unwrap();
            ga.confirm(&devices[0]).unwrap();
            match gb.as_mut() {
                Some(g) => {
                    g.process(&devices[1], &dir, &out.commit).unwrap();
                }
                None => {
                    gb = Some(
                        Group::join(
                            &devices[1],
                            &dir,
                            out.welcome.as_ref().unwrap(),
                            out.join_bundle.as_ref().unwrap(),
                        )
                        .unwrap(),
                    );
                }
            }
            size += n;
        }
        let gbm = gb.as_mut().unwrap();

        // One more person joins.
        let newcomer = size;
        let kp = devices[newcomer].key_package();
        let t = Instant::now();
        let out = ga.add(&devices[0], &dir, &[kp]).unwrap();
        let create = t.elapsed();
        ga.confirm(&devices[0]).unwrap();
        let t = Instant::now();
        let r = gbm.process(&devices[1], &dir, &out.commit).unwrap();
        let apply = t.elapsed();
        assert!(matches!(r, Processed::Commit { .. }));
        let t = Instant::now();
        let _joined = Group::join(
            &devices[newcomer],
            &dir,
            out.welcome.as_ref().unwrap(),
            out.join_bundle.as_ref().unwrap(),
        )
        .unwrap();
        let join = t.elapsed();
        size += 1;
        assert_eq!(ga.members().len(), size);
        let (commit_len, welcome_len) = (out.commit.len(), out.welcome.as_ref().unwrap().len());

        // The state a device has to seal after that.
        let t = Instant::now();
        let sealed = state::save(&vault, "mls", &devices[0], &[&ga]).unwrap();
        let seal = t.elapsed();

        // And one person is removed.
        let victim = newcomer;
        let t = Instant::now();
        let rm = ga
            .remove(&devices[0], &[people[victim].device_bytes(0)])
            .unwrap();
        let rm_create = t.elapsed();
        ga.confirm(&devices[0]).unwrap();
        let t = Instant::now();
        gbm.process(&devices[1], &dir, &rm.commit).unwrap();
        let rm_apply = t.elapsed();
        size -= 1;

        println!(
            "| {target} | {} / {} B | {} B | {} | {} | {} | {} / {} B | {} | {} B | {} |",
            ms(create),
            commit_len,
            welcome_len,
            ms(create),
            ms(apply),
            ms(join),
            ms(rm_create),
            rm.commit.len(),
            ms(rm_apply),
            sealed.len(),
            ms(seal),
        );
    }
}
