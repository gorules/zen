import argparse
import json
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]

CORE = {
    "core/macros": "zen-macros",
    "core/types": "zen-types",
    "core/expression": "zen-expression",
    "core/template": "zen-tmpl",
    "core/engine": "zen-engine",
}

BINDINGS = {
    "nodejs": "bindings/nodejs",
    "python": "bindings/python",
}

TAG_PREFIX = {
    "rust": "zen-engine",
    "nodejs": "nodejs",
    "python": "python",
}

VERSION = re.compile(r"^\d+\.\d+\.\d+-beta\.\d+$")
PACKAGE_VERSION = re.compile(r'^version\s*=\s*"[^"]*"', re.MULTILINE)


def fail(message):
    print(f"error: {message}", file=sys.stderr)
    sys.exit(1)


def tags(pattern):
    out = subprocess.run(
        ["git", "tag", "--list", pattern], cwd=ROOT, check=True, capture_output=True, text=True
    ).stdout
    return [line.strip() for line in out.splitlines() if line.strip()]


def next_version(branch):
    match = re.fullmatch(r"release/(\d+)\.(\d+)", branch)
    if not match:
        fail(f"cannot derive a version from branch '{branch}' (expected release/<major>.<minor>)")
    base = f"{match.group(1)}.{match.group(2)}.0"
    numbers = []
    for prefix in TAG_PREFIX.values():
        for tag in tags(f"{prefix}-v{base}-beta.*"):
            found = re.fullmatch(rf"{re.escape(prefix)}-v{re.escape(base)}-beta\.(\d+)", tag)
            if found:
                numbers.append(int(found.group(1)))
    return f"{base}-beta.{max(numbers) + 1 if numbers else 0}"


def set_package_version(path, version):
    manifest = ROOT / path / "Cargo.toml"
    text = manifest.read_text()
    updated, count = PACKAGE_VERSION.subn(f'version = "{version}"', text, count=1)
    if count != 1:
        fail(f"no package version in {manifest}")
    manifest.write_text(updated)


def set_requirements(version):
    names = "|".join(re.escape(name) for name in CORE.values())
    requirement = re.compile(
        rf'^((?:{names})\s*=\s*\{{[^}}\n]*\bversion\s*=\s*")[^"]*(")', re.MULTILINE
    )
    for manifest in ROOT.glob("**/Cargo.toml"):
        if "target" in manifest.relative_to(ROOT).parts:
            continue
        text = manifest.read_text()
        updated = requirement.sub(rf"\g<1>{version}\g<2>", text)
        if updated != text:
            manifest.write_text(updated)


def set_package_json(path, version):
    package = ROOT / path / "package.json"
    text = package.read_text()
    updated, count = re.subn(
        r'^  "version": "[^"]*"', f'  "version": "{version}"', text, count=1, flags=re.MULTILINE
    )
    if count != 1 or json.loads(updated)["version"] != version:
        fail(f"no top-level version in {package}")
    package.write_text(updated)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--branch", required=True)
    parser.add_argument("--version", default="")
    parser.add_argument("--targets", required=True)
    args = parser.parse_args()

    targets = [t for t in args.targets.split(",") if t]
    unknown = [t for t in targets if t not in TAG_PREFIX]
    if not targets or unknown:
        fail(f"targets must be a non-empty subset of {sorted(TAG_PREFIX)}")

    version = args.version.strip() or next_version(args.branch)
    if not VERSION.fullmatch(version):
        fail(f"'{version}' is not a beta version like 2.2.0-beta.0")

    release_tags = [f"{TAG_PREFIX[t]}-v{version}" for t in targets]
    existing = [tag for tag in release_tags if tags(tag)]
    if existing:
        fail(f"tags already exist: {' '.join(existing)}")

    for path in CORE:
        set_package_version(path, version)
    set_requirements(version)
    for target in targets:
        if target in BINDINGS:
            set_package_version(BINDINGS[target], version)
    if "nodejs" in targets:
        set_package_json(BINDINGS["nodejs"], version)

    print(f"version={version}")
    print(f"tags={' '.join(release_tags)}")


if __name__ == "__main__":
    main()
