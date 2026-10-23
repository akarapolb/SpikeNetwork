"""Break each protection in cli/src/large.rs in turn and check a test in cli/tests/large.rs fails.
Run from the repository root: python cli/mutcheck_large.py"""
import os, shutil, subprocess

PATH = "cli/src/large.rs"
MUTS = [
    ("                        rec.cursor = 0;\n                        break;", "                        break;", "cursor not reset after rotation"),
    ("            None => bail!(\n", "            None => continue,\n            Some(_) if false => bail!(\n", "add with no key package"),
    ("                            for list in l.lists.iter().take(64) {\n                                dir.apply(list);\n                            }", "                            let _ = &l;", "lists announced in the log are ignored"),
    ("    let mut announced = news.is_empty();", "    let mut announced = true;", "a commit does not announce the lists it depends on first"),
    ("if owner != me && i_am_admin && !rec.pending_leavers.contains(&owner) {", "if false && owner != me && i_am_admin && !rec.pending_leavers.contains(&owner) {", "an admin does not remove someone who left"),
    ("    if !missing.is_empty() {", "    if false && !missing.is_empty() {", "own new devices are never added"),
    ("    if !stale.is_empty() {", "    if false && !stale.is_empty() {", "own revoked devices are never removed"),
    ("                used_last_resort |= last_resort;", "                let _ = last_resort;", "use of the reusable key package is not reported"),
    ("(Status::Invited, Some(true)) => {", "(Status::Invited, Some(true)) if false => {", "own devices' invitations are not accepted"),
    ("            if gone || !i_am_admin {", "            if false {", "a leaver notice is never dropped"),
]


def run():
    r = subprocess.run("cargo test -p spike-cli --test large 2>&1", shell=True, capture_output=True, text=True)
    out = r.stdout
    failed = [l for l in out.splitlines() if l.startswith("test ") and l.endswith("FAILED")]
    return "could not compile" not in out, failed


ok, f = run()
print("control:", "clean" if ok and not f else f)
for old, new, name in MUTS:
    src = open(PATH, encoding="utf8").read()
    if old not in src:
        print("SKIP (pattern missing):", name)
        continue
    shutil.copy(PATH, PATH + ".bak")
    open(PATH, "w", encoding="utf8").write(src.replace(old, new, 1))
    try:
        ok, f = run()
    finally:
        shutil.move(PATH + ".bak", PATH)
        os.utime(PATH, None)
    print(("CAUGHT " if (f or not ok) else "MISSED ") + name, [x[5:60] for x in f][:3] if ok else "(compile error)")
ok, f = run()
print("after restore:", "clean" if ok and not f else f)
