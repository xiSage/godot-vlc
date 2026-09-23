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

## `h264_64x64_4s_4chapters.mp4`

`h264_64x64_4s_4chapters.mp4` is 57 KB and exists because the chapter API cannot
be exercised without a file that names its chapters. It is what
`src/acceptance.rs` plays to read `get_full_chapter_descriptions` and to see the
chapter and title events arrive.

- 64x64, 10 fps, 4 seconds, H.264 High profile, yuv420p, AAC-LC 48 kHz mono.
- Four chapters of one second each, named `Opening Scene`, `Second Part`,
  `Third Part` and `Closing Part`.
- SHA-256 `EB51C31FA4B4550E4E2EC38587819652FEA19989794F49F9A8BB23F0943DAC22`.

The names live in the `chpl` box, which is one of the three chapter sources the
MP4 demuxer reads (the others are a GoPro `HMMT` box and an Apple chapter track).
`mp4info`-style box searches show `chpl`, `chap`, `tref`, `udta` and `moov` in
this file and no `HMMT`, so LibVLC takes the `chpl` path.

It is made in two steps: a chapter list in ffmpeg's metadata format, then one
ffmpeg run over generated video and audio. `$TMP` is wherever that list is
written; nothing about it is special.

```sh
cat > "$TMP/chapters.txt" <<'EOF'
;FFMETADATA1
[CHAPTER]
TIMEBASE=1/1000
START=0
END=1000
title=Opening Scene
[CHAPTER]
TIMEBASE=1/1000
START=1000
END=2000
title=Second Part
[CHAPTER]
TIMEBASE=1/1000
START=2000
END=3000
title=Third Part
[CHAPTER]
TIMEBASE=1/1000
START=3000
END=4000
title=Closing Part
EOF

ffmpeg -loglevel error -y \
       -f lavfi -i "testsrc2=size=64x64:rate=10:duration=4" \
       -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=4" \
       -i "$TMP/chapters.txt" \
       -map 0:v -map 1:a -map_metadata 2 -map_chapters 2 \
       -c:v libx264 -pix_fmt yuv420p -c:a aac -movflags +faststart -shortest \
       test/media/h264_64x64_4s_4chapters.mp4
```

The audio track is there even though nothing listens to it: ffmpeg writes the
chapter list as a `chpl` box either way, but a file with a single stream is a
different thing to demux than the two-stream files the rest of the suite plays.
