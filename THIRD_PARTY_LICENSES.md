# Third-party licenses

Source code vendored into this repository from other projects, with the
license it is distributed under. Each vendored file names its origin in a
header comment.

## fly-on-the-wall

- Upstream: https://github.com/swarnavspujari/fly-on-the-wall (commit
  `5f6f56d3ed86b835ab2f1598c8ff444ed5d1bd93`)
- Adapted files (from `crates/fly-audio/src/`):
  - `lib.rs` → `src-tauri/src/meeting/capture/mod.rs`
  - `cpal_backend.rs` → `src-tauri/src/meeting/capture/cpal_backend.rs`
  - `mix.rs` → `src-tauri/src/meeting/capture/mix.rs`
  - `win_volume.rs` → `src-tauri/src/meeting/capture/win_volume.rs`

```text
MIT License

Copyright (c) 2026 WizzyWYG

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
