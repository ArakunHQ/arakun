#!/usr/bin/env python3
"""Uploads an Android App Bundle to a Google Play track.

android-release.yml runs it after it signs the bundle. It talks to the Google
Play Developer API (v3) with a service account's key, using only the standard
library and `openssl` to sign the token request, so no third-party action or
package sees the key:

    PLAY_SERVICE_ACCOUNT_JSON="$(cat key.json)" \\
      python3 android/tools/play-upload.py app-release.aab --track internal

The service account needs the "Release to testing tracks" permission (or
"Release apps to production" for that track) on the app in Play Console. Play
takes an app's first bundle only by hand in Play Console; this uploads the ones
after it. An app not yet reviewed takes only `--status draft`.
"""

import argparse
import base64
import json
import os
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

PACKAGE = "com.arakun.android"
API = "https://androidpublisher.googleapis.com/androidpublisher/v3/applications/" + PACKAGE
UPLOAD = "https://androidpublisher.googleapis.com/upload/androidpublisher/v3/applications/" + PACKAGE
SCOPE = "https://www.googleapis.com/auth/androidpublisher"


def b64url(data):
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


def token(account):
    """An access token for the service account, from a JWT it signs."""
    now = int(time.time())
    header = b64url(json.dumps({"alg": "RS256", "typ": "JWT"}).encode())
    claims = b64url(json.dumps({
        "iss": account["client_email"],
        "scope": SCOPE,
        "aud": account["token_uri"],
        "iat": now,
        "exp": now + 3600,
    }).encode())
    signing_input = f"{header}.{claims}".encode()
    # The private key goes to openssl through a file only this user can read.
    with tempfile.NamedTemporaryFile("w", suffix=".pem", delete=False) as f:
        os.chmod(f.name, 0o600)
        f.write(account["private_key"])
        key = f.name
    try:
        sig = subprocess.run(["openssl", "dgst", "-sha256", "-sign", key],
                             input=signing_input, capture_output=True, check=True).stdout
    finally:
        os.remove(key)
    jwt = f"{signing_input.decode()}.{b64url(sig)}"
    body = urllib.parse.urlencode({
        "grant_type": "urn:ietf:params:oauth:grant-type:jwt-bearer",
        "assertion": jwt,
    }).encode()
    return call("POST", account["token_uri"], body, "application/x-www-form-urlencoded")["access_token"]


def call(method, url, body=None, kind="application/json", auth=None):
    req = urllib.request.Request(url, data=body, method=method)
    if body is not None:
        req.add_header("Content-Type", kind)
    if auth:
        req.add_header("Authorization", "Bearer " + auth)
    try:
        with urllib.request.urlopen(req, timeout=600) as res:
            text = res.read()
    except urllib.error.HTTPError as e:
        # Google's error body says what is wrong; it holds no secret.
        sys.exit(f"{method} {url.split('?')[0]}: {e.code} {e.read().decode(errors='replace')}")
    return json.loads(text) if text else {}


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("bundle", help="the signed .aab")
    ap.add_argument("--track", default="internal", help="internal, alpha, beta or production")
    ap.add_argument("--status", default="completed", choices=["completed", "draft"],
                    help="draft for an app Play has not reviewed yet")
    ap.add_argument("--name", help="the release's name in Play Console")
    args = ap.parse_args()

    raw = os.environ.get("PLAY_SERVICE_ACCOUNT_JSON")
    if not raw:
        sys.exit("PLAY_SERVICE_ACCOUNT_JSON is not set")
    auth = token(json.loads(raw))

    # Everything happens in an edit, which goes live only on commit.
    edit = call("POST", f"{API}/edits", b"{}", auth=auth)["id"]
    with open(args.bundle, "rb") as f:
        bundle = call("POST", f"{UPLOAD}/edits/{edit}/bundles?uploadType=media", f.read(),
                      "application/octet-stream", auth=auth)
    code = bundle["versionCode"]
    release = {"versionCodes": [str(code)], "status": args.status}
    if args.name:
        release["name"] = args.name
    call("PUT", f"{API}/edits/{edit}/tracks/{args.track}",
         json.dumps({"track": args.track, "releases": [release]}).encode(), auth=auth)
    call("POST", f"{API}/edits/{edit}:commit", b"", auth=auth)
    print(f"Uploaded version code {code} to the {args.track} track ({args.status}).")


if __name__ == "__main__":
    main()
