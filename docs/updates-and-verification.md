# Updates and verification

How a release build updates itself, nightly builds, and how to check a download.

## Updates

A release build keeps itself up to date. Every 6 hours it asks GitHub for the latest
release. When there is a newer one, it downloads it, checks it against the release's
signed `SHA256SUMS`, and puts it next to the installed app. The update is installed when
you quit Arakun, so the next start runs it. Settings › General › Updates shows the version, has **Check now**,
and on Linux and macOS **Restart now**. Turn **Update automatically** off there to stop
the checks; `ARAKUN_UPDATES=0` turns them off for one run.

When a version has a serious problem, Arakun can ask you to update, or require it. Once
an hour, and a few seconds after it starts, a release build reads the `update-policy`
feature flag from Arakun's PostHog project. The request carries only the version, the
update channel, the operating system and the processor type; the id it sends is your
install's random id only if you agreed to usage counts, and a new one each start
otherwise. If the flag says *request*, Arakun downloads the latest release and asks you
to restart into it; **Later** or Escape puts it off until the next start. If it says
*force*, a dialog stays over the window, and Escape and shortcuts do nothing, until you
restart into the new version (on Windows: quit and open Arakun again) or quit. After a
failed download, both offer **Try again**.
Neither happens when no newer release exists. Maintainers set this per version in
PostHog: one condition set per version range on `$app_version`, with a semver operator,
and the variant to use.

From a terminal, `arakun update` downloads the latest release and installs it. If Arakun
is running, it is installed when Arakun quits: on Linux and macOS `arakun restart` does
that now; on Windows, quit Arakun and open it again. `arakun update --check` only says
whether there is a newer release.

## Nightly builds

Every night, if main changed, the release workflow builds it on every platform and
publishes it as a pre-release named after the next version, such as
`0.5.1-nightly.202611180300`. They are for maintainers and testers who want what is next
and will report what breaks. **Nightlies can break:** nobody tests them by hand, one can
crash, lose a session or change how your data is stored, and a release may not read what
a nightly wrote. Don't use them for work you can't redo.

To get them, turn on **Nightly builds** in Settings › General › Updates (it shows this
warning and asks again), or run `arakun update --channel nightly`. The updater then takes
whichever is newest of the nightlies and the releases, so a release that comes out still
reaches you. `arakun update --channel stable` (or turning the switch off) goes back to
releases: the nightly stays until a release newer than it comes out, or install the latest
release again to go back now. `arakun update --channel` says which channel you follow.

To install a nightly from scratch, pass `--nightly` to the install script:
`curl -fsSL https://arakun.dev/install | sh -s -- --nightly`, or on Windows
`$env:ARAKUN_VERSION = "nightly"` before `irm https://arakun.dev/install.ps1 | iex`. A
nightly installed this way follows nightlies. For a server alone,
`curl -fsSL https://arakun.dev/install-server | sh -s -- --nightly` installs the newest
nightly `arakun-server`. It does not update itself, so run the line again for a newer one.

Nightly `SHA256SUMS` are signed with their own key,
[assets/nightly-key.pub](../assets/nightly-key.pub), which the updater accepts for nightly
versions only: it can't sign a release. The install scripts and `releases/latest` never
see nightlies. The newest 14 are kept on the
[releases page](https://github.com/arakunhq/arakun/releases).

## What the updater keeps and checks

The updater keeps its files in Arakun's data folder (`~/.local/share/arakun` on Linux
and macOS, `%LOCALAPPDATA%\arakun` on Windows): `update.json` (the version waiting to be
installed, its checksum, and the last one you were told about), `update.lock` (held while
one updater runs), `update-channel.lock` (held while the
channel changes or an update is installed) and `update-download/` (the download, removed when it is done). The new version waits next to
the installed one as `.arakun.update` (`.Arakun.app.update` on macOS, `.arakun.exe.update`
on Windows, with its `.pdb` as `.arakun.exe.update.pdb`), unpacked first as `.arakun.incoming`.
**Update automatically** and the channel (`stable` or `nightly`, once you pick one;
without it a nightly build follows nightlies and a release follows releases) are kept in
[`settings.json`](storage-and-env.md#the-data-folder) as `general.automatic_updates` and
`general.update_channel`.

Arakun updates itself where it runs from, so that folder and the binary must be yours
(the install scripts' folders are). A binary that belongs to another user, such as one a
package manager installed, is left alone: update it the way it was installed. A build
from source never updates itself: it has its own way of installing and the browser
runtime it was built against. Pull and build it again instead.

An update replaces the binary (the whole `Arakun.app` on macOS). The browser runtime
(CEF) comes along only when there is none yet, such as on a release from before the
browser. A release with another version of CEF is not installed by the updater, since
Windows keeps a running browser's files locked; it says so, and the install script
installs it once Arakun is closed.

What is checked: `SHA256SUMS` is signed with Arakun's release key, the download matches
the checksum in it, and the new binary starts and says the version it should. The
signature shows the release was made by someone who holds that key, which only the
release workflow can use: someone who can change a release on GitHub but has no key
can't make the updater install anything. A missing or bad signature stops the update.
The public key is built into Arakun and is
[assets/release-key.pub](../assets/release-key.pub). A second key, kept offline, is
accepted too ([assets/release-key-backup.pub](../assets/release-key-backup.pub)), so
releases can still be signed if the first one is ever lost.

The install scripts check the signature only when a verifier is installed: `minisign`,
or on Linux and macOS also OpenSSL 3 (macOS ships LibreSSL, so there it usually takes
`brew install minisign`). Without one they check only the checksum, and say so.
Releases up to v0.3.0 are not signed. The scripts install one only when you ask for it
with `--version` (`ARAKUN_VERSION` on Windows); an unsigned latest release is refused.

## Manual download

Download the archive or disk image for your platform from
[GitHub Releases](https://github.com/arakunhq/arakun/releases) and check it against
`SHA256SUMS`. Release builds include the browser: each carries CEF (Chromium), a few
hundred MB unpacked. To check that `SHA256SUMS` is signed with Arakun's release key
(releases after v0.3.0), download `SHA256SUMS.minisig` next to it and run
[minisign](https://jedisct1.github.io/minisign/):

```sh
minisign -Vm SHA256SUMS -P RWRHh4aSkL+9oL41eNxaL791lQYzu/un+QvVqfckIj7839aB8lQBoxyn
```

If a release says it is signed with the backup key, check it with that one instead:
`-P RWSxlRjKJX7FQmC0+on+hpDPmZ/ShKCW4KzyJhbnm0bAc6JGTI7KV+xg`.

Each download also has a build provenance attestation. It shows that this repository's
release workflow built it, and from which commit. Check it with the
[GitHub CLI](https://cli.github.com):

```sh
gh attestation verify arakun-<version>-linux-x86_64.tar.gz -R arakunhq/arakun
```

**Linux:** extract `arakun-<version>-linux-x86_64.tar.gz` (on ARM,
`arakun-<version>-linux-arm64.tar.gz`). `arakun` runs from there, or
put it somewhere on your `PATH`, for example `~/.local/bin`, with its `cef` folder next to
it (or in `~/.local/share/arakun/cef`). Without that folder, everything but browser tabs
works. It needs glibc 2.35 or newer (Ubuntu 22.04+,
Debian 12+, Fedora 36+); on older systems, build from source. You also need a Vulkan
driver and the usual desktop libraries (libxkbcommon, Wayland or X11, fontconfig,
freetype). Without a graphics driver it can use, as in a VM or remote desktop without a
GPU, `arakun` prints `could not open its window` with what to install, and exits with
code 1; started from a launcher, it shows the same as a desktop notification. Mesa's software Vulkan driver works without a GPU, only slower:
`mesa-vulkan-drivers` on Debian, Ubuntu and Fedora, `vulkan-swrast` on Arch. It also
needs a Wayland or X11 desktop: with neither `WAYLAND_DISPLAY` nor `DISPLAY` set, or with
one that does not answer (an SSH session, a machine without a desktop), it says so the
same way and exits with code 1. There, `arakun server serve` runs sessions without the
window. On Ubuntu 23.10 and later, browser tabs also need the AppArmor profile in
[Browser tabs on Ubuntu](browser.md#browser-tabs-on-ubuntu).

**Windows:** extract `arakun-<version>-windows-x86_64.zip` and run `arakun.exe`. Keep the
files next to it together: `arakun-helper.exe` and the CEF runtime are for browser tabs,
and `arakun.pdb` gives crash reports Arakun's function names.

**macOS:** open `arakun-<version>-macos-arm64.dmg` and drag `Arakun.app` into
`Applications`. The app and the disk image are signed with a Developer ID and notarized by
Apple. v0.2.1 has `arakun-v0.2.1-macos-arm64.tar.gz` instead: extract it and move
`Arakun.app` to `Applications`. v0.1.0 has no macOS build.
