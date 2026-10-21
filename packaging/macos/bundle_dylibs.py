#!/usr/bin/env python3
"""Copy every non-system dylib an app bundle's binaries need into
Contents/Frameworks and point the binaries at those copies.

    bundle_dylibs.py etherquic.app

Executables in Contents/MacOS load from @executable_path/../Frameworks.
Libraries in Frameworks load each other from @loader_path. System
libraries (/usr/lib, /System) stay as they are. Each changed file is
signed ad hoc again, since arm64 macOS refuses an unsigned binary.
"""

import os
import re
import shutil
import subprocess
import sys

SYSTEM = ("/usr/lib/", "/System/")
MACH_MAGIC = (
    b"\xcf\xfa\xed\xfe",  # 64-le
    b"\xfe\xed\xfa\xcf",  # 64-be
    b"\xce\xfa\xed\xfe",  # 32-le
    b"\xfe\xed\xfa\xce",  # 32-be
    b"\xca\xfe\xba\xbe",  # fat
    b"\xbe\xba\xfe\xca",
)


def is_macho(path):
    if os.path.islink(path) or not os.path.isfile(path):
        return False
    with open(path, "rb") as fh:
        magic = fh.read(4)
    return magic in MACH_MAGIC


def run(*args):
    return subprocess.run(args, check=True, capture_output=True, text=True).stdout


def deps(path):
    out = run("otool", "-L", path).splitlines()[1:]
    names = []
    for line in out:
        line = line.strip()
        if not line:
            continue
        names.append(line.split(" (compatibility")[0].strip())
    return names


def install_id(path):
    out = run("otool", "-D", path).splitlines()
    return out[1].strip() if len(out) > 1 else None


def rpaths(path):
    out = run("otool", "-l", path)
    found = []
    for block in out.split("Load command"):
        if "LC_RPATH" in block:
            m = re.search(r"path (.+?) \(offset", block)
            if m:
                found.append(m.group(1))
    return found


def cellar_match(name):
    """A Homebrew Cellar file with the same basename.

    `opt` can point at a newer major while the binary still names the
    previous soname, which remains installed under Cellar.
    """
    leaf = os.path.basename(name)
    if not leaf:
        return None
    roots = []
    parts = name.split("/")
    if "opt" in parts:
        i = parts.index("opt")
        if i + 1 < len(parts):
            formula = parts[i + 1]
            roots.append(f"/opt/homebrew/Cellar/{formula}")
            roots.append(f"/usr/local/Cellar/{formula}")
    roots.extend(("/opt/homebrew/Cellar", "/usr/local/Cellar"))
    seen = set()
    for root in roots:
        if root in seen or not os.path.isdir(root):
            continue
        seen.add(root)
        for dirpath, _dirnames, filenames in os.walk(root):
            if leaf in filenames:
                candidate = os.path.join(dirpath, leaf)
                if os.path.exists(candidate):
                    return os.path.realpath(candidate)
    return None


def resolve(name, referrer, original_dir):
    """The file `name` refers to, seen from `referrer` before it moved."""
    if name.startswith("@rpath/"):
        leaf = name[len("@rpath/"):]
        for rp in rpaths(referrer):
            rp = rp.replace("@loader_path", original_dir).replace(
                "@executable_path", original_dir
            )
            candidate = os.path.join(rp, leaf)
            if os.path.exists(candidate):
                return os.path.realpath(candidate)
        for d in ("/opt/homebrew/lib", "/usr/local/lib"):
            candidate = os.path.join(d, leaf)
            if os.path.exists(candidate):
                return os.path.realpath(candidate)
        return cellar_match(leaf)
    if name.startswith("@loader_path/"):
        candidate = os.path.join(original_dir, name[len("@loader_path/"):])
        return os.path.realpath(candidate) if os.path.exists(candidate) else None
    if name.startswith("@executable_path/"):
        return None
    if os.path.exists(name):
        return os.path.realpath(name)
    return cellar_match(name)


def main():
    app = sys.argv[1]
    contents = os.path.join(app, "Contents")
    macos = os.path.join(contents, "MacOS")
    frameworks = os.path.join(contents, "Frameworks")
    os.makedirs(frameworks, exist_ok=True)
    # (path in bundle, directory it was built or copied from)
    queue = []
    for name in sorted(os.listdir(macos)):
        path = os.path.join(macos, name)
        if not is_macho(path):
            continue
        queue.append((path, macos))
    for name in sorted(os.listdir(frameworks)):
        path = os.path.join(frameworks, name)
        if os.path.isfile(path) and not os.path.islink(path):
            origin = sys.argv[2] if len(sys.argv) > 2 else frameworks
            queue.append((path, origin))
    done = set()
    while queue:
        path, origin = queue.pop()
        if path in done:
            continue
        done.add(path)
        in_frameworks = os.path.dirname(path) == frameworks
        prefix = "@loader_path/" if in_frameworks else "@executable_path/../Frameworks/"
        os.chmod(path, 0o755)
        changes = []
        if in_frameworks:
            changes += ["-id", "@loader_path/" + os.path.basename(path)]
        for name in deps(path):
            if name.startswith(SYSTEM) or name == install_id(path):
                continue
            if name.startswith(("@loader_path/", "@executable_path/")) and os.path.exists(
                os.path.join(frameworks, os.path.basename(name))
            ):
                leaf = os.path.basename(name)
            else:
                source = resolve(name, path, origin)
                if source is None:
                    leaf = os.path.basename(name)
                    if not os.path.exists(os.path.join(frameworks, leaf)):
                        print(f"warning: {path}: cannot find {name}", file=sys.stderr)
                        continue
                else:
                    leaf = os.path.basename(name)
                    target = os.path.join(frameworks, leaf)
                    if not os.path.exists(target):
                        shutil.copy2(source, target)
                        os.chmod(target, 0o755)
                        queue.append((target, os.path.dirname(source)))
            changes += ["-change", name, prefix + leaf]
        for rp in rpaths(path):
            if rp.startswith(("/opt/homebrew", "/usr/local", "/Users/")):
                changes += ["-delete_rpath", rp]
        if changes:
            subprocess.run(["install_name_tool", *changes, path], check=True,
                           capture_output=True)
        subprocess.run(["codesign", "--force", "-s", "-", path], check=True,
                       capture_output=True)
    for path in sorted(done):
        for name in deps(path):
            if name.startswith(("/opt/homebrew", "/usr/local", "/Users/")):
                print(f"error: {path} still loads {name}", file=sys.stderr)
                sys.exit(1)
    print(f"bundled {len(done)} binaries")


if __name__ == "__main__":
    main()
