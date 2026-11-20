"""Fixed offline executor. The device never supplies a checkout or commands."""
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import stat
import subprocess
import sys
import tempfile


BASE = "0ec59da6152c203b4da9fa3dea299700239fa56a"
LIMIT = 512
TRACKED, NEW = "tracked.txt", "note.txt"
DIRTY = " M tracked.txt\n?? note.txt\n"


def require(condition):
    if not condition:
        raise ValueError("fixture verification failed")


def git(*args):
    return subprocess.check_output(["git", *args], text=True)


def status(*paths):
    return git("status", "--porcelain=v1", "--untracked-files=all", "--", *paths)


def canonical(files):
    return json.dumps({"v": 1, "base": BASE, "files": files}, separators=(",", ":")).encode("ascii")


def check_files(files):
    require(isinstance(files, dict) and list(files) == [TRACKED, NEW])
    require(re.fullmatch(r"base line\nedited: [0-9a-f]{32}\n", files[TRACKED]))
    require(re.fullmatch(r"new: [0-9a-f]{32}\n", files[NEW]))


def snapshot():
    # Only open fixed root-level paths. Neither Git internals, ignored files
    # nor other untracked files (including the decoy .env) are ever read.
    files = {}
    for path in (TRACKED, NEW):
        with os.fdopen(os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK), "rb") as stream:
            s = os.fstat(stream.fileno())
            require(stat.S_ISREG(s.st_mode) and s.st_nlink == 1 and s.st_size <= LIMIT)
            files[path] = stream.read(LIMIT + 1).decode("ascii")
    check_files(files)
    return canonical(files)


def main(stage):
    os.chdir(tempfile.mkdtemp(dir="/tmp"))
    os.environ.update(HOME=os.getcwd(), GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL="/dev/null")
    for role in ("AUTHOR", "COMMITTER"):
        os.environ.update({f"GIT_{role}_NAME": "Fixture", f"GIT_{role}_EMAIL": "fixture@example.invalid",
                           f"GIT_{role}_DATE": "946684800 +0000"})
    git("init", "-q", "--template=", "--object-format=sha1", "-b", "main")
    Path(TRACKED).write_bytes(b"base line\n")
    git("add", "--", TRACKED)
    git("commit", "-qm", "fixture base")
    require(git("rev-parse", "HEAD").strip() == BASE and status() == "")

    if stage == "capture":
        require(sys.stdin.buffer.read(1) == b"")
        with open(TRACKED, "a") as stream:
            stream.write(f"edited: {secrets.token_hex(16)}\n")
        Path(NEW).write_text(f"new: {secrets.token_hex(16)}\n")
        Path(".env").write_text("FIXTURE_SECRET=must-not-leave-executor-a\n")
        require(status(TRACKED, NEW) == DIRTY and status(".env") == "?? .env\n")
        sys.stdout.buffer.write(snapshot())
    elif stage == "restore":
        raw = sys.stdin.buffer.read(LIMIT + 1)
        require(len(raw) <= LIMIT)
        files = json.loads(raw)["files"]
        check_files(files)
        require(raw == canonical(files))
        # This is a fresh private scratch directory. On any error the
        # executor is discarded; there is no partially restored workspace.
        Path(TRACKED).write_bytes(files[TRACKED].encode("ascii"))
        with open(NEW, "xb") as stream:
            stream.write(files[NEW].encode("ascii"))
        after, observed = status(), snapshot()
        require(after == DIRTY and observed == raw and not os.path.lexists(".env"))
        print(json.dumps({"before": "clean", "after": after, "sha256": hashlib.sha256(observed).hexdigest()}))
    else:
        raise ValueError("unknown step")


if __name__ == "__main__":
    main(sys.argv[1])
