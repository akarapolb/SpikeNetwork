import subprocess, shutil, sys

MUTS = [
    ("mls/src/group.rs", "                self.validate(dir, &sender, &sc)?;\n", "", "no commit validation before merge"),
    ("mls/src/credential.rs", "    if !verify_raw(", "    if false && !verify_raw(", "owner signature not checked"),
    ("mls/src/credential.rs", "    if list_version > list.version {", "    if false && list_version > list.version {", "stale directory accepted"),
    ("mls/src/credential.rs", "    if !named {", "    if !named && std::env::var_os(\"NEVER_SET\").is_some() {", "revoked device accepted"),
    ("mls/src/group.rs", "        policy::authorize(&self.policy, &committer.owner, &self.roster(), &changes)?;\n", "", "receiver skips authority rules"),
    ("mls/src/group.rs", "        if result.is_err() {\n            dev.restore(before);\n            let id", "        if false {\n            dev.restore(before);\n            let id", "no restore after refused commit"),
    ("mls/src/group.rs", "                let rotates = sc.remove_proposals().next().is_some();\n                let removed_me", "                let rotates = false;\n                let removed_me", "no rotation on removal (receiver)"),
    ("mls/src/group.rs", "        let rotates = pending.remove_proposals().next().is_some();\n", "        let rotates = false;\n", "no rotation on removal (committer)"),
    ("mls/src/group.rs", "        if result.is_err() {\n            dev.restore(before);\n        }\n        result\n    }\n\n    fn join_inner", "        result\n    }\n\n    fn join_inner", "no restore after refused welcome"),
    ("mls/src/group.rs", "            let id = credential::verify_leaf(dir, &m.credential, &m.signature_key)?;\n            roster.insert(id.owner, id.device);", "            let id = credential::parse(&m.credential)?.0;\n            roster.insert(id.owner, id.device);", "welcome members not verified"),
    ("mls/src/group.rs", "        credential::verify_leaf(dir, &member.credential, &member.signature_key)?;\n", "        let _ = &member;\n", "committer's own credential not checked"),
    ("mls/src/policy.rs", "            if after.has_device(device) {", "            if false && after.has_device(device) {", "duplicate devices allowed"),
    ("mls/src/policy.rs", "            Change::Update { old, new } if old != new => return Err(PolicyError::IdentityChanged),", "", "identity change allowed"),
]

def run():
    r = subprocess.run("cargo test -p spike-mls 2>&1", shell=True, capture_output=True, text=True)
    out = r.stdout
    failed = [l for l in out.splitlines() if l.startswith("test ") and l.endswith("FAILED")]
    compiled = "could not compile" not in out
    return compiled, failed

ok, f = run()
print("control:", "clean" if ok and not f else ("FAILS %s" % f))
for path, old, new, name in MUTS:
    src = open(path, encoding="utf8").read()
    if old not in src:
        print("SKIP (pattern missing):", name)
        continue
    shutil.copy(path, path + ".bak")
    open(path, "w", encoding="utf8").write(src.replace(old, new, 1))
    try:
        ok, f = run()
    finally:
        shutil.move(path + ".bak", path)
        import os; os.utime(path, None)
    print(("CAUGHT " if (f or not ok) else "MISSED ") + name, [x[5:70] for x in f][:3] if ok else "(compile error)")
ok, f = run()
print("after restore:", "clean" if ok and not f else f)
