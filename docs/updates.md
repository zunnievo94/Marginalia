# In-app updates

Release builds can update themselves: *Settings → Updates → Check for
updates*, or the notice shown when Marginalia starts. The app downloads the
new version, checks its signature and restarts into it.

## How it works

- The release workflow signs the update files with a private key and
  uploads them with `latest.json`, which lists the newest version and a
  download for each system.
- The app asks
  `https://github.com/<owner>/<repo>/releases/latest/download/latest.json`
  and only installs a file whose signature matches the public key built into
  it.
- Builds made without the key (local builds, forks) have no updater and say
  so in Settings.

What updates itself:

| Installed from | Updates with |
| --- | --- |
| Windows `-setup.exe` | the new `-setup.exe` |
| macOS `.dmg` | `Marginalia-macos-*.app.tar.gz` |
| Linux `.AppImage` | the new `.AppImage` (the file is replaced in place) |
| Linux `.deb` | the new `.deb` when it is signed, asking for your password |

The first version with the updater has to be installed by hand once; from
then on the app updates itself.

## Setting up the signing key (once, for maintainers)

The key is free (it is a [minisign](https://jedisct1.github.io/minisign/)
key, not a code-signing certificate). Make it on your own computer and never
commit it.

1. Generate it (needs Node.js):

   ```sh
   npx @tauri-apps/cli@2 signer generate -w ~/.tauri/marginalia.key
   ```

   Choose a password. This writes `~/.tauri/marginalia.key` (private) and
   `~/.tauri/marginalia.key.pub` (public). Back the private key up: if it is
   lost, installed apps can no longer update and everyone must reinstall by
   hand once.

2. In the repository on GitHub, *Settings → Secrets and variables → Actions*:
   - **Secrets** tab, *New repository secret*:
     - `TAURI_SIGNING_PRIVATE_KEY`: the whole content of `marginalia.key`
     - `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`: the password
   - **Variables** tab, *New repository variable*:
     - `TAURI_UPDATER_PUBKEY`: the whole content of `marginalia.key.pub`

3. Run the release workflow as usual. The release then also has `*.sig`
   files, `Marginalia-macos-*.app.tar.gz` and `latest.json`.
