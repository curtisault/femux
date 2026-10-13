# ADR-021: Protocol schema additions for 0.7.0

Date: 2026-10-12 · Status: accepted · Requirements: PROTO-010, GFX-001..004, UI-052

Images live in the daemon (REQ-GFX-001): libghostty-vt parses Kitty graphics per pane, and femux-vt decodes sixels into Kitty images before libghostty-vt sees them (REQ-GFX-002), so the protocol only knows Kitty images. Clients draw them on the host terminal. All changes are additive except the new required fields of `ImagePlacement`, which no client used before 0.7.0 (frames always carried an empty list).

| # | Change | Why |
| --- | --- | --- |
| 1 | `TermCaps` gains optional `cell_w`, `cell_h`: the client's cell size in pixels. | Image placements are sized in cells from pixel sizes, and programs such as `kitten icat` read the pty's pixel size (`TIOCGWINSZ`), which the daemon now sets from it. |
| 2 | `ImagePlacement` gains `image_id`, `generation`, the source rectangle `src_x/src_y/src_w/src_h`, `x_off/y_off` and `z`; `id` is the Kitty placement id. | A client must know which image to show, which part of it, whether its pixels changed, and whether it goes above or below text (REQ-UI-052). |
| 3 | New family `image`: `image.read {pane_id, image_id, offset}` returns `{format, width, height, generation, size, offset, data}` with at most 6 MiB of base64 per answer; `image.list {pane_id?}` lists stored images. | A 4K RGBA image is 33 MB, more than a frame may carry (REQ-PROTO-004), so pixels are read in chunks, once per generation. Base64 is what the Kitty protocol sends to the host anyway, so the client forwards it unchanged. `image.list` serves `femux graphics list` and memory checks (REQ-GFX-004). |
| 4 | `image.read` takes `shm` and `zlib`; the answer carries `shm` (a POSIX shared memory name) or `zlib` (compressed `data`). | Video sends a new image per frame. A client on the daemon's machine asks for shared memory and hands the name to its host (Kitty `t=s`), so pixels are copied once instead of base64-encoded three times. A remote client asks for zlib, which its host inflates itself (Kitty `o=z`), so less crosses the SSH link. |
