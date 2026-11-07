# slint-terminal

A terminal emulator widget for [Slint](https://slint.dev). the host application 
owns the window and the input source, and the widget owns the terminal grid, the pty, 
rendering (CPU or wgpu) and key, mouse and clipboard handling.

Import `Terminal` and `TerminalApi` from `"@SlintTerminal"` in your `.slint`
file, re-export `TerminalApi` from your window, then call
`slint_terminal::attach`.

## Features

- `wgpu`: GPU renderer (needs Slint's wgpu support in the application).
- `bundled-font`: embed Iosevka Term SS12 (~10 MB, SIL OFL).

## License

The crate is MIT licensed (see `LICENSE`); third-party material is listed in
`THIRD-PARTY-NOTICES.md`.

The crate depends on Slint, which is available under GPL-3.0-only, a
royalty-free license or a commercial license. MIT covers this crate's source
only. An application that links Slint must comply with the Slint license its
author chooses. 

See <https://slint.dev/terms-and-conditions>.
