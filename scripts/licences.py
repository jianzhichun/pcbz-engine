"""Write THIRD-PARTY-LICENSES.txt for pcbcraft-cli: every registry or git crate built into it
(normal dependencies, any target), its declared licence and the licence files it ships.
Usage (inside the upstream checkout): python licences.py <out file>. Stdlib only."""
import json
import pathlib
import subprocess
import sys

NOTICE_NAMES = ("LICENSE", "LICENCE", "COPYING", "NOTICE", "UNLICENSE", "COPYRIGHT")


def main():
    out = pathlib.Path(sys.argv[1])
    tree = subprocess.run(
        ["cargo", "tree", "-p", "pcbcraft-cli", "-e", "normal", "--target", "all", "--prefix", "none", "--locked", "-f", "{p}"],
        check=True, capture_output=True, text=True,
    ).stdout
    used = set()
    for line in tree.splitlines():
        parts = line.replace(" (*)", "").split()
        if len(parts) >= 2 and parts[1].startswith("v"):
            used.add((parts[0], parts[1][1:]))
    meta = json.loads(subprocess.run(["cargo", "metadata", "--format-version", "1", "--locked"], check=True, capture_output=True, text=True).stdout)
    lines = ["Third-party software built into pcbcraft-cli", "=" * 46, ""]
    count = 0
    for pkg in sorted(meta["packages"], key=lambda p: (p["name"], p["version"])):
        if (pkg["name"], pkg["version"]) not in used or pkg.get("source") is None:
            continue  # workspace crates are the project's own code (LICENSE-MIT / LICENSE-APACHE)
        count += 1
        root = pathlib.Path(pkg["manifest_path"]).parent
        lines += [f"{pkg['name']} {pkg['version']}  ({pkg.get('license') or 'see files below'})", f"  {pkg.get('repository') or ''}"]
        files = sorted(p for p in root.iterdir() if p.is_file() and p.name.upper().startswith(NOTICE_NAMES))
        if pkg.get("license_file"):
            extra = root / pkg["license_file"]
            if extra.is_file() and extra not in files:
                files.append(extra)
        for f in files:
            lines += ["", f"--- {f.name} ---", f.read_text(encoding="utf-8", errors="replace").rstrip()]
        if not files and not pkg.get("license"):
            raise SystemExit(f"{pkg['name']} {pkg['version']}: no licence declared or shipped")
        lines += ["", ""]
    out.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"{count} third-party crates written to {out}")


if __name__ == "__main__":
    main()
