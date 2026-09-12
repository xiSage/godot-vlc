# Test media

`h264_64x64_1s.mp4` is 2.8 KB and exists so that the runtime can be tested
without downloading anything or shipping a large fixture.

- 64x64, 10 fps, 1 second, H.264 High profile, yuv420p, no audio.
- Produced with ffmpeg; the exact command is below, so it can be regenerated
  rather than trusted.

```sh
ffmpeg -f lavfi -i "testsrc=size=64x64:rate=10:duration=1" \
       -c:v libx264 -pix_fmt yuv420p -movflags +faststart \
       -y test/media/h264_64x64_1s.mp4
```

It is used by `scripts/acceptance_test.ps1`, which requires the *decode* path to
work end to end. That matters more than it looks: the failure this project
actually hit was a decoder plugin that could not be loaded, which LibVLC reports
once and then works around, so the visible symptom was "Codec `h264` is not
supported" rather than a missing library.
