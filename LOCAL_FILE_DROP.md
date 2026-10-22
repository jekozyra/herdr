# Mac → saved-machine pane file drops (downstream Herdr 0.9.5)

This is a **local downstream build**, not an official Herdr release or an installable plugin. The implementation adapts the Apache-2.0 [herdr-native-upload](https://github.com/janaki-sasidhar/herdr-native-upload) patch from its 0.9.0 base to the 0.9.5 source tree. The customization is maintained on [`jekozyra/herdr`'s `master` branch](https://github.com/jekozyra/herdr), based on the upstream `v0.9.5` tag; `upstream/master` remains the official Herdr line. Do not open an unsolicited upstream PR.

## Behavior

With Herdr running on the **Mac** and an enabled saved SSH machine selected, drop a local regular file into its focused agent pane. Finder-dropped regular files, including images and PDFs, upload over outbound SSH to private unique directories under `~/.cache/herdr/uploads/` **on the remote**, and the remote path is inserted into that same pane's editor without Enter. Image-only clipboard pastes have no source file path and keep Herdr's existing image bridge. Local-workspace drops and `herdr --remote` are unchanged. The local machine requires SSH; the remote needs a running compatible Herdr server with `pane.get` and `pane.send_input`, plus Python 3.9+ in the noninteractive SSH PATH. No remote plugin, daemon, or modified remote Herdr binary is installed.

The default maximum is 100 MiB/file, 250 MiB/drop and 10 files. Input must consist only of existing absolute local file paths; a matching path pasted as text may also trigger an upload. Symlinks, directories, hidden filenames and special files are rejected by default. Transfer verifies SHA-256 before inserting paths. Uploads are in 0700 directories with 0600 files; completed uploads survive a failed insertion. On the **next upload**, marked directories older than 30 days are cleaned up. This is opportunistic expiry, not a scheduled job and not a backup guarantee. To retain a file, have Pi copy it into the project. The pane ID is captured at drop time and its terminal identity pinned on the first SSH query, before local file snapshots; a terminal replaced *before that first query* cannot be distinguished from the original. Esc cancels a running upload; inspect the prompt before retrying after an ambiguous failure.

## Build and smoke-test on the Mac

1. Keep the Mac's official Herdr 0.9.5 binary unchanged. Check that `rustup`, a C compiler, and Zig **0.16.0** are available; the checked-in `rust-toolchain.toml` requests Rust 1.96.1. Install Zig from [ziglang.org/download](https://ziglang.org/download/) for your Mac architecture; validate the release checksum and run `zig version`. Set `ZIG=/absolute/path/to/zig` if it is not on PATH. Do not install a different Zig version and assume it will work.
2. Clone your public fork on the Mac and build it separately from the official installation:

   ```sh
   git clone https://github.com/jekozyra/herdr.git "$HOME/herdr-upload-client"
   cd "$HOME/herdr-upload-client"
   ZIG="$HOME/.local/opt/zig-aarch64-macos-0.16.0/zig" cargo test --locked --bin herdr file_upload
   ZIG="$HOME/.local/opt/zig-aarch64-macos-0.16.0/zig" cargo test --locked --test client_mode native_file_drop
   python3 -m unittest scripts.test_file_upload_receiver
   ZIG="$HOME/.local/opt/zig-aarch64-macos-0.16.0/zig" cargo build --release --locked
   ```

   These Zig paths are for an Apple Silicon Mac with the verified 0.16.0 tarball installed at that location; adjust for another architecture or installation.

3. Before a normal session, test SSH authentication with `ssh -T -o BatchMode=yes devbox true` and check that remote `python3` is version 3.9+. Keep the official `herdr` binary available for rollback. With the normal local Herdr server running, launch the **custom Mac client** explicitly with `$HOME/herdr-upload-client/target/release/herdr client` (not `herdr --remote devbox`); select the saved `devbox` machine. Drop a *disposable* PDF into an idle Pi pane. Verify the inserted path is remote, the PDF bytes match, and Enter was not sent. Drop an image file separately to check that it now reaches the same upload cache. Paste a clipboard-only image to check that Herdr's existing image bridge still works. Only then consider putting the custom binary first in PATH.

**The Mac 0.9.5 build and real Finder → saved `devbox` → Pi PDF/image drops have been tested.** A new upstream version still requires its own Mac validation. Do not replace the official installation until the above succeeds. If the custom client fails, close it and launch the original Herdr binary. The remote session and official Mac binary remain untouched.

## Updates

Do **not** use GitHub's one-click fork sync or reset your customized `master` to upstream; either may bypass the validation needed to keep this feature working. Add the official remote once with `git remote add upstream https://github.com/herdrdev/herdr.git`, then `git fetch upstream --tags`. When a new upstream release arrives, make an integration branch from your fork's `master`, merge the desired upstream release tag there, resolve conflicts, run format/compile/tests and the real Mac drag-and-drop smoke test, and only then merge into your fork's `master`. Keep the previous known-good binary for rollback. Official Herdr updates do not update this branch and may replace a custom binary installed over the official one; keep them in separate paths until validated.

## Storage policy

`[file_upload]` in the Mac's Herdr config supports destination and limits. The default destination is `~/.cache/herdr/uploads/` and default `retention_seconds` is `2592000` (30 days). The remote receiver will not recursively delete unmarked directories: its cleanup targets directories with its own metadata marker, and only on the next upload. For a forced cleanup or additional restrictions, see `src/client/file_upload/receiver.py` and `~/.config/herdr/file-upload-policy.json` on the remote. A remote user can delete their own cache at any time; this is not an access-control boundary beyond SSH.
