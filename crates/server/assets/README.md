# Demo asset

`demo.h264` is the built-in test pattern used by `gamebridge --demo`. It is a
synthetic clip generated with:

```sh
ffmpeg -f lavfi -i testsrc=size=640x360:rate=30 -t 6 \
       -c:v libx264 -profile:v baseline -pix_fmt yuv420p \
       -x264-params aud=1 -crf 30 -f h264 demo.h264
```

It contains no third-party content. Regenerate it with the command above if the
demo needs to change.
