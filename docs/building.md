# Build from source

You need [rustup](https://rustup.rs). The first `cargo` command in the checkout
installs the Rust version named in `rust-toolchain.toml`, the one CI uses.

On Linux the build also needs a C compiler, pkg-config and the development packages of
libxkbcommon, Wayland, X11 (xcb), fontconfig, FreeType, Vulkan and OpenSSL; on macOS,
the Xcode Command Line Tools. `tools/setup-dev.sh` installs them on Debian and Ubuntu
(apt), Fedora (dnf), Arch (pacman) and macOS; see
[CONTRIBUTING.md](../CONTRIBUTING.md#setting-up). To install them yourself,
`tools/setup-dev.sh --check` names the packages that are missing, and the script has
the list for each package manager.

**Windows:** MSVC toolchain and Visual Studio Build Tools; `tools\setup-dev.ps1`
installs them. See [docs/windows.md](windows.md).

## Without the browser

The quickest path, and what most CI jobs use:

```sh
git clone https://github.com/arakunhq/arakun
cd arakun
cargo build --release --no-default-features
./target/release/arakun
```

## With the browser (CEF)

See [Building with the browser](browser.md#building-with-the-browser-cef).

## The phone page

The page phones open (the phone remote) is a React app in [`phone/`](../phone), built
with Vite. The app embeds the built copy in `phone/dist`, and the Worker in `cloud/`
serves the same folder, so `phone/dist` is committed and `cargo build` needs no Node.
If you change `phone/`, test and build it with Node 24 or newer, and commit
`phone/dist` with your change:

```sh
cd phone
npm ci
npm test        # the encryption against the Rust side's vectors, and the screens
npm run build   # writes phone/dist
```

CI fails a pull request whose `phone/dist` is not what `npm run build` writes
(`phone/tools/check-dist.sh`).

## Install for your user (Linux)

`tools/install.sh` builds an optimized binary and installs it to `~/.local/bin`, with a
`.desktop` entry and icons. It prepares CEF with `tools/install-cef.sh` if it is missing
and `CEF_DIST` points at an unpacked distribution. It builds the `release-local` profile,
which rebuilds in seconds after a change; `--full` builds the slower `release` profile
that releases use.

```sh
tools/install.sh               # --full, --debug, --no-browser, --prefix DIR, --uninstall
```
