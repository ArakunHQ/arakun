"""Downloads a CEF binary distribution, checks it, and lays it out for CEF_PATH.

    python tools/fetch-cef.py ARCHIVE SHA1 DEST

ARCHIVE is a file name from https://cef-builds.spotifycdn.com, such as
cef_binary_156.0.15+gcf671c0+chromium-156.0.8078.80_macosarm64_minimal.tar.bz2, and SHA1
its checksum there. DEST is replaced with the layout the `cef` crate's own download
leaves (download-cef's extract_target_archive): Release/, on Linux and Windows with
Resources/ merged in, plus CMakeLists.txt, cmake/, include/, libcef_dll/, CREDITS.html
and an archive.json naming the archive. Point CEF_PATH at it and cef-dll-sys builds
against it without downloading anything.

The release workflow uses it on macOS and Windows: the crate's own download first
fetches the CDN's index.json, which is bigger than its HTTP client accepts. The SHA-1 is
checked before anything is unpacked.
"""

import hashlib
import json
import shutil
import sys
import tarfile
import time
import urllib.parse
import urllib.request
from pathlib import Path

CDN = "https://cef-builds.spotifycdn.com"


def download(name: str, to: Path) -> None:
    url = f"{CDN}/{urllib.parse.quote(name)}"
    for attempt in range(3):
        try:
            with urllib.request.urlopen(url, timeout=60) as r, open(to, "wb") as f:
                shutil.copyfileobj(r, f, 1024 * 1024)
            return
        except OSError as e:
            if attempt == 2:
                sys.exit(f"fetch-cef: could not download {url}: {e}")
            time.sleep(5)


def sha1(path: Path) -> str:
    h = hashlib.sha1()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def main() -> None:
    if len(sys.argv) != 4:
        sys.exit("usage: fetch-cef.py ARCHIVE SHA1 DEST")
    name, want, dest = sys.argv[1], sys.argv[2].lower(), Path(sys.argv[3]).resolve()
    if not name.endswith(".tar.bz2") or "/" in name:
        sys.exit(f"fetch-cef: {name} is not a CEF archive name")
    work = dest.parent / f"{dest.name}.work"
    shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True)

    archive = work / name
    print(f"fetch-cef: downloading {name}")
    download(name, archive)
    have = sha1(archive)
    if have != want:
        shutil.rmtree(work)
        sys.exit(f"fetch-cef: {name} has SHA-1 {have}, expected {want}")

    print("fetch-cef: unpacking")
    with tarfile.open(archive, "r:bz2") as tar:
        tar.extractall(work, filter="data")
    archive.unlink()
    src = work / name.removesuffix(".tar.bz2")

    out = work / "cef"
    (src / "Release").rename(out)
    if "_macos" not in name:
        for entry in (src / "Resources").iterdir():
            entry.rename(out / entry.name)
    for part in ("CMakeLists.txt", "cmake", "include", "libcef_dll", "CREDITS.html"):
        (src / part).rename(out / part)
    # The fields download-cef writes and cef-dll-sys reads (the version comes from name).
    (out / "archive.json").write_text(
        json.dumps({"type": "minimal", "name": name, "sha1": want}, indent=2)
    )

    shutil.rmtree(dest, ignore_errors=True)
    out.rename(dest)
    shutil.rmtree(work)
    print(f"fetch-cef: {dest}")


if __name__ == "__main__":
    main()
