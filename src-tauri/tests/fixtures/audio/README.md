# Audio fixtures

Two seconds of a 440 Hz tone in every format the Transcriptions page imports,
plus a video without audio. Used by `src/audio_toolkit/audio/file.rs` tests.
Regenerate with ffmpeg:

```bash
T="-f lavfi -i sine=frequency=440:duration=2:sample_rate=44100"
V="-f lavfi -i color=c=black:s=32x32:r=5:d=2"
ffmpeg -f lavfi -i sine=frequency=440:duration=2:sample_rate=16000 -ac 1 -c:a pcm_s16le tone-16k-mono.wav
ffmpeg -f lavfi -i sine=frequency=440:duration=2:sample_rate=11025 -ac 2 -c:a pcm_s24le tone-11k-stereo-24bit.wav
ffmpeg $T -ac 2 -c:a libmp3lame -b:a 32k tone.mp3
ffmpeg $T -ac 2 -c:a aac -b:a 32k tone.m4a
ffmpeg -f lavfi -i sine=frequency=440:duration=2:sample_rate=22050 -ac 1 -c:a flac tone.flac
ffmpeg $T -ac 2 -c:a libvorbis -q:a 0 tone.ogg
ffmpeg -f lavfi -i sine=frequency=440:duration=2:sample_rate=48000 -ac 2 -c:a libopus -b:a 16k tone.opus
ffmpeg $V -f lavfi -i sine=frequency=440:duration=2:sample_rate=48000 -c:v libvpx -b:v 10k -c:a libopus -b:a 16k -shortest tone.webm
ffmpeg $V $T -c:v libx264 -preset ultrafast -crf 51 -c:a aac -b:a 32k -shortest tone.mp4
ffmpeg $V $T -c:v libx264 -preset ultrafast -crf 51 -c:a aac -b:a 32k -shortest tone.mov
ffmpeg $V $T -c:v libx264 -preset ultrafast -crf 51 -c:a libvorbis -q:a 0 -shortest tone.mkv
ffmpeg $V -c:v libx264 -preset ultrafast -crf 51 video-only.mp4
```
