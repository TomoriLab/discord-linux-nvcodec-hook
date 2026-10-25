# Discord Linux NVCodec Hook

An experimental Rust `LD_PRELOAD` hook for HEVC video in Discord's native Linux voice module. It uses NVIDIA NVDEC for received HEVC frames and changes the module's NVIDIA encoder preference so its existing CUDA NVENC candidate can be selected for transmission. It does not add an encoder, change codec negotiation, or modify Discord's installed files.

This is an unofficial reverse-engineering project. The hook depends on parts of Discord's private C++ ABI. **A Discord update can break it even when the required ELF symbols are still present.** Review and test each updated voice module before relying on the patch.

## Current validation status

The implementation was tested locally with Discord Linux host **1.0.166**, `discord_voice.node` SHA-256 `1a3fa65e21242ca84a27b6b654ba342c7ebf6c964fa3c1e1ee41682b9717fa24`, and NVIDIA driver **615.78.08**. Rust tests passed for HEVC Main and Main10 decoding. A separate harness loaded that actual voice module and received decoded callbacks for 256×144 Main, 256×144 Main10, and one 4096×2304 Main frame. The harness also confirmed that the CUDA NVENC preference patch matched the module's instructions.

**Not yet validated:** receiving a live Discord broadcast, selecting NVENC during a real outgoing broadcast, packet loss, latency, stream renegotiation, and behavior after a Discord update. The encoder preference log indicates that an instruction was patched; it does not prove that a transmitted stream used NVENC.

## What the hook changes

| Path | Behavior |
| --- | --- |
| HEVC receive | Intercepts `VideoDecoderFactory::Create`, decodes Annex B HEVC through NVDEC, copies the decoded surface to host memory, converts NV12 to I420, and delivers frames through the module's WebRTC callback. |
| Video transmit | Skips the module's NVIDIA Vulkan encoder candidate insertion so the existing NVIDIA CUDA candidate remains available. The module chooses the final encoder. |
| Other video codecs | Delegates decoder creation to the original factory. |

The decoder currently accepts progressive 4:2:0 HEVC within the GPU's reported limits. Main10/12 output is reduced to 8-bit I420. Copying frames to host memory and converting them adds CPU work; this is not a zero-copy decoder.

The hook reads the voice module's local ELF symbol table at load time and checks the decoder factory prologue and the encoder insertion call. It does not use fixed function offsets or a fixed Discord version path. If a required symbol or checked instruction sequence is missing, the hook skips the module and prints a reason to standard error. Stripped voice modules are unsupported. A matching symbol and instruction sequence **does not** verify the `EncodedImage` layout, decoder vtable, callback ABI, or other private structures.

Rust owns the decoder state and frame buffers. Calls into CUDA and Discord, vtable access, and instruction patching use `unsafe`. The private ABI prevents a general memory-safety guarantee across Discord releases.

## Requirements

- Linux x86-64. The hook reads `/proc/self/maps`, parses ELF64, and writes x86-64 jump instructions.
- An NVIDIA GPU and driver providing `libcuda.so.1` and `libnvcuvid.so.1` at runtime. Building also needs linkable `libcuda.so` and `libnvcuvid.so` development symlinks.
- Rust 1.99.1 and Cargo were used for validation. Earlier toolchains are unverified. The repository has no Cargo dependencies.
- Discord's user-installed executable at `${XDG_CONFIG_HOME:-$HOME/.config}/discord/Discord`. The voice module must retain the ELF symbols used by the hook.
- Bash and standard Linux utilities used by the build and installer scripts. Desktop installation also expects the system entry at `/usr/share/applications/discord.desktop`.

The repository includes generated FFI bindings and the `nv-codec-headers` declarations they came from. Building does not require proprietary CUDA Toolkit headers or bindgen. Bindgen CLI 0.72.1 is needed only to regenerate `src/ffi.rs`.

## Build

From the repository root:

```bash
./build.sh
```

This runs `cargo build --release --offline --locked` and copies the result to `./libdiscord_nvenc_nvdec.so`. The library is a local build artifact and is ignored by Git. If linking fails with `cannot find -lcuda` or `cannot find -lnvcuvid`, check that the development symlinks are installed and visible to the linker. The runtime driver libraries alone do not satisfy those link names.

To build without copying the result to the repository root:

```bash
cargo build --release --offline --locked
```

## Run and install

For one launch from a terminal, after building:

```bash
./launch.sh
```

The launcher sets `LD_PRELOAD` for the user-installed Discord executable and passes through any arguments. Close an already running Discord instance first. Opening a second instance can hand off to the existing process, which would not have the hook loaded.

To use the patch from the desktop menu:

```bash
./install.sh install
```

The installer copies the library and launcher into `${XDG_DATA_HOME:-$HOME/.local/share}/discord-nvenc-nvdec-patch` and writes a user-specific `${XDG_DATA_HOME:-$HOME/.local/share}/applications/discord.desktop`. It backs up an existing user-specific Discord desktop entry before replacing it. It does not edit `/usr/share/discord`, `/usr/bin/discord`, or the voice module. Installation does not change a running Discord process. Launch Discord from the desktop entry after closing the old process.

A direct `/usr/bin/discord` launch bypasses the user desktop entry. Use `./launch.sh` for a terminal launch with the hook.

To remove the desktop integration and installed hook files:

```bash
./install.sh uninstall
```

The uninstall command restores the previous user-specific desktop entry when the installer backed one up. It refuses to replace a desktop entry whose installer marker has since been removed or changed. Running Discord processes are not stopped by either command.

## Check behavior and handle updates

Run `./launch.sh` in a terminal to read messages prefixed `discord-nvdec:`. `CUDA NVENC preference patch installed` means the instruction pattern matched and was patched. `HEVC selected for NVDEC decoding` means the custom decoder was created. Neither message alone confirms successful live video receive or outgoing hardware encoding. Confirm those paths with actual broadcasts and inspect the resulting video behavior.

When Discord updates, record the new host version and voice-module SHA-256:

```bash
discord_host=$(readlink -f "${XDG_CONFIG_HOME:-$HOME/.config}/discord/Discord")
sha256sum "${discord_host%/*}"/modules/discord_voice-*/discord_voice/discord_voice.node
```

Then verify that the hook still finds its symbols and patterns. Test both incoming HEVC video and outgoing video before treating the new module as compatible. If the module is stripped or a pattern changes, the hook leaves it unpatched. If its private object layouts change while the patterns remain, a crash or incorrect frame handling is possible. Remove the desktop integration with `./install.sh uninstall` if the updated module is not validated.

## Development checks

```bash
cargo fmt --check
cargo clippy --release --offline --locked -- -D warnings
cargo test --release --offline --locked -- --test-threads=1
```

The NVDEC tests need a working NVIDIA GPU and driver. `tests/fixtures` contains locally generated HEVC test patterns. To regenerate the committed bindings from the bundled headers:

```bash
./scripts/generate-bindings.sh
```

That development command requires bindgen CLI 0.72.1 and `rustfmt`. It preserves the provenance pointer at the top of `src/ffi.rs`.

## License and redistribution

The project's original code is under the [MIT License](LICENSE). The three bundled files under `include/ffnvcodec/` retain their own copyright and permission notices. `src/ffi.rs` was generated from those headers. Their exact source commit, file-level copyright lines, common permission and warranty text, and the relationship to the generated bindings are recorded in [Third-party notices](THIRD_PARTY_NOTICES.md). The project's MIT license entry in `Cargo.toml` does not replace those header notices.

For a source distribution, keep `LICENSE`, `THIRD_PARTY_NOTICES.md`, and the notices embedded in the bundled headers. For a binary distribution that omits the headers, provide `LICENSE` and `THIRD_PARTY_NOTICES.md` alongside the library. `build.sh` produces a library, not a release archive; it does not assemble those files for you.

Discord binaries, NVIDIA driver libraries, the proprietary CUDA Toolkit headers, FFmpeg, libx265, and bindgen are not included in this repository. Users supply the runtime libraries installed on their own systems. This project is not affiliated with Discord or NVIDIA.
