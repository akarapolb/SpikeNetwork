"""Break each protection in cli/src/large.rs in turn and check a test in cli/tests/large.rs fails.
Run from the repository root: python cli/mutcheck_large.py"""
import os, shutil, subprocess

PATH = "cli/src/large.rs"
MUTS = [
    ("if dir.refresh(home, r, &missed).await? > 0 {", "if false && dir.refresh(home, r, &missed).await? > 0 {", "sync never fetches unknown members' lists"),
    ("if dir.refresh(home, &mut r, &missed).await? == 0 {", "if true {", "accept never fetches unknown members' lists"),
    ("                        rec.cursor = 0;\n                        break;", "                        break;", "cursor not reset after rotation"),
    ("            None => bail!(\n", "            None => continue,\n            Some(_) if false => bail!(\n", "add with no key package"),
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
