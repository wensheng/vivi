# Vivi

`vivi` is the Vivid Protocol 1.1 image viewer and video player for
[Vivido](../vivido/). It requires a presenter that selects protocol 1.1; there is no protocol 1.0,
anchor-v1, or FFmpeg-packet-v0 fallback.

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
- Unsupported codecs, packetization, or declared color metadata values fail explicitly. When a
  video omits color metadata, Vivi warns and uses deterministic conventional defaults: BT.601 for
  SD or BT.709 for HD, with BT.709 transfer and limited range. Vivi never falls back to the retired
  FFmpeg packet profile.

The control dispatcher preserves correlated replies and routes credit, visibility, display,
keyframe-recovery, and source-loss events. Video production pauses or throttles while invisible and
resumes from an acceptable random-access unit after `NEED_KEYFRAME`.

## Build

Vivi requires Rust 2024 and FFmpeg development libraries for `libavformat`, `libavcodec`, and
`libavutil`.

```bash
cd vivi
cargo build
```

On macOS, install `ffmpeg` and `pkg-config` with Homebrew. On Debian or Ubuntu, install the matching
FFmpeg development packages.

## Usage

Inside Vivido:

```bash
vivi photo.png
vivi clip.mkv
vivi -z 1.5 photo.webp clip.mp4
```

For a remote login, use `vvssh`; see [Running Vivi over SSH](../docs/vivi-over-ssh.md). The wrapper
uses private stream-local forwarding and transfers the token through protected stdin setup, never
as an SSH or remote-shell argument.

Conformance traces can be generated without a presenter:

```bash
vivi --dry-run --verbose photo.png
vivi --trace-dir /tmp/vivi-trace --verbose clip.mkv
```

Each trace begins with the framing-1.0 `VIVD` preface and contains protocol-1.1 records.

## Limits

Audio, generic fragmentation, damage rectangles, shared memory, caching, source reconfiguration,
session resumption, multiplexer passthrough, and seek/loop APIs are outside the current profile.
Very large raw raster frames and access units that exceed their negotiated source ceiling are
rejected. Windows named-pipe transport remains pending.

The normative wire contract is [docs/vivid_protocol_spec.md](../docs/vivid_protocol_spec.md).
