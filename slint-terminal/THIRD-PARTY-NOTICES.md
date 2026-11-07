# Third-party notices

`slint-terminal` itself is MIT licensed (see `LICENSE`). It contains or can
embed the following third-party material.

## Rio terminal (MIT)

`src/key.rs` is adapted from `librio/src/key.rs` in
<https://github.com/raphamorim/rio>.

    MIT License

    Copyright (c) 2022-present Raphael Amorim

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

## Iosevka Term SS12 (SIL Open Font License 1.1)

Embedded only with the `bundled-font` feature. The font files and their
license are in `assets/fonts/`; the license (`LICENSE-Iosevka.md`) must be
shipped with any distribution that includes them.

Copyright (c) 2015-2026, Renzhi Li (aka. Belleve Invis, belleve@typeof.net)

## Dependencies

Dependencies keep their own licenses, listed in their packages. This crate
builds on Slint, which is GPL-3.0-only, royalty-free or commercially licensed
at the choice of whoever builds an application (see `readme.md`).
