# XHS-Downloader answers

All files here are hand-written with invented content. NOT MEASURED: nobody involved ran XHS-Downloader or
saw a real answer from it, so none of this has been compared with one.

They follow XHS-Downloader 2.8 as read on GitHub on 2026-11-12:

- `source/module/model.py`: the answer is `{"message", "params", "data"}`;
- `source/application/explore.py`: the Chinese keys inside `data`;
- `source/application/app.py` (`setup_routes`, `__extract_video`, `__extract_image`, `__download_files`):
  the video and image address lists, the cover address, and `data` being `null` when it fails;
- `source/application/image.py`: image addresses are `https://ci.xiaohongshu.com/<token>?imageView2/format/jpeg`
  with the default image format, `https://sns-img-bd.xhscdn.com/<token>` with `auto`.

`{BASE}` is replaced by the address of the test's local media server, `{ID}` by the note id.

- `video.json`: a video note; `images.json`: an image note, the second picture has a live part;
- `no-data.json`: `data` is `null`; `skipped.json`: `data` holds only a `message` (a download record);
- `unknown-type.json`: a type the plugin does not know.
