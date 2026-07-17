# Remote Linux Audio Through Vivido

Vivi audio uses the Vivid media connection. It does not require PulseAudio, an ALSA device, or an
audio server on the SSH host:

```text
remote Linux Vivi encoded audio -> vvssh -> local Vivido -> FFmpeg -> CPAL -> speakers
```

The supported remote arrangement is a Linux SSH destination with Vivido running locally on
Windows, macOS, or Linux. Vivido selects the current system-default output device. There is no
remote device selection or volume option.

## Setup

Install Vivi and its FFmpeg build dependencies on the Linux host. For Debian or Ubuntu:

```sh
sudo apt update
sudo apt install pkg-config libavformat-dev libavcodec-dev libavutil-dev libswresample-dev
```

Start `vvssh` from a shell inside the local Vivido window:

```sh
vvssh user@linux-host
```

The wrapper creates the private Vivid reverse forward, transfers the capability token through its
protected setup channel, and exports `VIVID_REMOTE=1` in the login shell. No extra `-R` option,
`PULSE_SERVER`, `.asoundrc`, or PulseAudio daemon is needed.

On Windows, `vvssh` also exports `VIVID_ANCHOR_TRANSPORT=conpty`. This selects the marker envelope
required for images and video to anchor through the Windows pseudoconsole while Vivi itself runs on
Linux.

Inside the remote shell, verify the binding and play media:

```sh
test -S "${VIVID_ENDPOINT#unix:}" && printf 'Vivid SSH forward is ready\n'
test "$VIVID_REMOTE" = 1 && printf 'Remote audio mode is active\n'
vivi clip.mp4
vivi song.mp3
vivi recording.m4a
```

Embedded sound and audio-only access units are decoded by local Vivido and heard on its default
speakers. `--no-wait` remains silent for video and is invalid for audio-only playback.

## Compatibility and failures

- If an older presenter lacks `audio-access-unit-v1`, remote video continues silently with a
  warning.
- Remote audio-only playback fails with a presenter-upgrade error. Vivi never opens an audio device
  on the SSH host while `VIVID_REMOTE=1` is set.
- A local, non-SSH Vivi may use its direct CPAL compatibility path if audio negotiation fails.
- `SOURCE_LOST` for a linked audio source stops sound but does not stop its video source.

If video appears but sound does not, run Vivi with `--verbose` and check its negotiation or
source-loss message. Also verify that Vivido can open a default local device and that the FFmpeg
runtime libraries used to build Vivido are discoverable. On Windows, the selected vcpkg triplet's
`bin` directory must be on `PATH`.

The SSH server must allow stream-local forwarding. See
[Running Vivi over SSH](../../docs/vivi-over-ssh.md) for server settings and tunnel diagnostics.
