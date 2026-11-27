# Browser tabs

Browser tabs are based on CEF (Chromium Embedded Framework), behind the optional
`browser` cargo feature. Release builds include it.

## Using them

Browser tabs based on CEF (Chromium Embedded Framework), behind
the optional `browser` cargo feature. Agents can drive pages on localhost through
`arakun page` (screenshot, console, click, type, eval). Links you click open in a
browser tab; turn off Settings › General › Open links in Arakun to open them in your
own browser instead. That covers links from sessions, the GitHub view and pins; a link
clicked inside a browser tab always stays in Arakun. Builds without the browser open
links in your own browser from the start. The choice is
saved in [`settings.json`](storage-and-env.md#the-data-folder). A link to a pull request or issue on the host `gh` is signed in to opens in
the GitHub tab instead, from a chat, a terminal (⌘-click, Ctrl-click elsewhere), the
palette, the pull request button or a session card's pull request chip; its "Open on
GitHub" opens the page. A tab shows its site's icon even before its page loads again:
the icons that loaded are kept, by site, in `~/.local/share/arakun/cache/favicons/`
(`%LOCALAPPDATA%\arakun\cache\favicons\` on Windows), never in redacted mode.
Inspect (F12) opens Chromium's DevTools in a window of its own. The browser opens no
remote debugging port unless `ARAKUN_CDP_PORT=<port>` is set, for tools such as
Playwright. That port has no password: any program on the machine can use it to drive
your signed-in browser and read your files, sandboxed agents and other users included.
Agents may not open or drive pages on it.

## Building with the browser (CEF)

The default `browser` feature needs CEF at build time and at run time.

- **Linux:** download the CEF 156.0.15 linux64 binary distribution (linuxarm64 on ARM;
  the minimal one is enough) from <https://cef-builds.spotifycdn.com/index.html>, unpack
  it, and run:

  ```sh
  tools/install-cef.sh /path/to/cef_binary_156.0.15_linux64
  cargo build --release
  ```

  This puts the runtime in `~/.local/share/arakun/cef` (loaded on the first browser tab)
  and links the build-time SDK at `.cef-sdk`, which `.cargo/config.toml` points `CEF_PATH`
  at.
- **macOS:** the build also needs CMake and Ninja (`brew install cmake ninja`), and the
  download needs Python 3.12 or newer (`brew install python`; the `python3` macOS ships
  is too old). Run:

  ```sh
  tools/setup-dev.sh --browser
  cargo build --release
  ```

  This downloads the CEF archive the release uses, checks its SHA-1, keeps it in
  `~/.local/share/arakun/cef-sdk` for every checkout, and links it at `.cef-sdk`. The
  `cef` crate's own download no longer works: the CDN's index is larger than it reads
  ("the response body is larger than request limit").
- **Windows:** see [docs/windows.md](windows.md). If the build's own CEF download
  fails with "the response body is larger than request limit", fetch the archive with
  `tools/fetch-cef.py` and point `CEF_PATH` at it, as the release workflow does (it has
  the archive names and checksums).

## Browser tabs on Ubuntu

Chromium keeps web pages in a sandbox built on user namespaces. Ubuntu 23.10 and later
block those for programs that have no AppArmor profile allowing them. There Arakun does not
start the browser, and a browser tab says why. To allow it, give `arakun` a profile like
the one Ubuntu ships for Chrome (`/etc/apparmor.d/chrome`), then restart Arakun. Run in a
checkout, it takes the build in `target/release`; anywhere else, the `arakun` on your
`PATH`:

```sh
arakun=target/release/arakun
[ -x "$arakun" ] || arakun=$(command -v arakun)
arakun=$(readlink -f "$arakun")
if [ -x "$arakun" ]; then
  # One profile per binary, so a checkout's does not replace an installed one's.
  profile=arakun-$(printf %s "$arakun" | cksum | cut -d' ' -f1)
  # Quoted, with AppArmor's pattern characters escaped, for paths with spaces.
  attach=$(printf %s "$arakun" | sed 's/[][\\*?{}^"]/\\&/g')
  sudo tee "/etc/apparmor.d/$profile" >/dev/null <<EOF
abi <abi/4.0>,
include <tunables/global>

profile $profile "$attach" flags=(unconfined) {
  userns,
}
EOF
  sudo apparmor_parser -r "/etc/apparmor.d/$profile"
else
  echo "no arakun found; run this in the checkout or put arakun on your PATH"
fi
```

Each binary gets a profile of its own, `/etc/apparmor.d/arakun-<number>`, named after its
path, so run this again if `arakun` moves. To remove one, run
`sudo apparmor_parser -R /etc/apparmor.d/arakun-<number>` and delete the file.
