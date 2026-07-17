# Vivi

`vivi` is the Vivid Protocol 1.1 image viewer and media player for
[Vivido](../vivido/). It requires a presenter that selects protocol 1.1; there is no protocol 1.0,
anchor-v1, or FFmpeg-packet-v0 fallback.

Vivi streams encoded embedded audio and MP3, M4A, and WAV access units to Vivido. Vivido decodes
and plays them through the system default CoreAudio, ALSA, or WASAPI device. This same path carries
sound from a remote Linux Vivi over SSH to a local Windows or macOS Vivido. A local Vivi can fall
back to direct CPAL output when an older presenter does not negotiate audio.

Vivido exposes a private endpoint and per-window token to its child shell:

```text
VIVID_ENDPOINT=unix:/private/path/to/endpoint.sock
VIVID_TOKEN=<64 hexadecimal characters>
```

Media records use that endpoint rather than stdout. Stdout carries only normal terminal output and
the bounded, authenticated marker-v2 sequence used for inline placement. Under tmux or screen,
Vivi suppresses the marker and places the node at the reported grid cursor instead.

## Media paths

- PNG and JPEG are sent as their original encoded bytes with exact dimensions, length, and SHA-256.
- Other still formats decode to straight-alpha RGBA8. Vivi sends zstd only when the complete zstd
  record is smaller than the raw record.
- Video is inspected in a first pass to determine exact metadata and maximum access-unit size, then
  reopened and streamed as portable H.264/HEVC Annex B, VP9 frames, or AV1 low-overhead units.
- The first supported audio stream is sent in container order with video. Vivido resamples it for
  the default device and uses played audio frames as the A/V clock. If video audio cannot be
  negotiated, decoded, or output, Vivi warns and continues playing video silently.
- MP3, AAC/ALAC M4A, and common PCM WAV files keep the Vivid session open until decoder and device
  buffers have drained. Remote audio-only playback fails clearly when the presenter is too old.
- Unsupported codecs, packetization, or declared color metadata values fail explicitly. When a
  video omits color metadata, Vivi warns and uses deterministic conventional defaults: BT.601 for
  SD or BT.709 for HD, with BT.709 transfer and limited range. Vivi never falls back to the retired
  FFmpeg packet profile.

The control dispatcher preserves correlated replies and routes credit, visibility, display,
keyframe-recovery, and source-loss events. Video production pauses or throttles while invisible and
resumes from an acceptable random-access unit after `NEED_KEYFRAME`.

## Build

Vivi requires Rust 2024 and FFmpeg development libraries for `libavformat`, `libavcodec`,
`libavutil`, and `libswresample`. Direct compatibility playback uses CPAL (CoreAudio on macOS,
ALSA on Linux, and WASAPI on Windows).

```bash
cd vivi
cargo build
```

On macOS, install `ffmpeg` and `pkg-config` with Homebrew. On Debian or Ubuntu, install
`pkg-config`, the matching FFmpeg development packages including `libswresample-dev`, and
`libasound2-dev`.

On Windows, build with the MSVC Rust toolchain from a Visual Studio Developer Command Prompt:

```powershell
$env:VCPKG_ROOT = "C:\path\to\vcpkg"
vcpkg install ffmpeg:x64-windows
$env:VCPKG_DEFAULT_TRIPLET = "x64-windows"
$env:PATH = "$env:VCPKG_ROOT\installed\x64-windows\bin;$env:PATH"
cargo build --release
```

The build script derives the FFmpeg ABI layout from the selected vcpkg headers and requires the
`swresample` import library. Keep that triplet's `bin` directory on `PATH` when running Vivi so its
FFmpeg DLLs can be found.

## Usage

Inside Vivido:

```bash
vivi photo.png
vivi clip.mkv
vivi song.mp3
vivi -z 1.5 photo.webp clip.mp4
```

For a remote login, use `vvssh`; see [Running Vivi over SSH](../docs/vivi-over-ssh.md). The wrapper
uses private stream-local forwarding and transfers the token through protected stdin setup, never
as an SSH or remote-shell argument. Audio uses the same Vivid tunnel; see
[Remote Linux audio over SSH](docs/ssh-linux-audio.md).

Conformance traces can be generated without a presenter:

```bash
vivi --dry-run --verbose photo.png
vivi --trace-dir /tmp/vivi-trace --verbose clip.mkv
```

Each trace begins with the framing-1.0 `VIVD` preface and contains protocol-1.1 records.
Dry-run and trace modes emit deterministic audio control/media records and never open an audio
output device.

`--no-wait` preserves immediate video submission and skips local video audio. It is invalid for an
audio-only file, because local playback cannot continue after Vivi exits.

## Limits

Audio device selection, volume controls, generic fragmentation, damage rectangles, shared memory,
caching, source reconfiguration, session resumption, multiplexer passthrough, and seek/loop APIs
are outside the current profile.
Very large raw raster frames and access units that exceed their negotiated source ceiling are
rejected. Windows uses a private loopback TCP endpoint; non-loopback TCP endpoints are rejected by
`vvssh`.

The normative wire contract is [docs/vivid_protocol_spec.md](../docs/vivid_protocol_spec.md).
