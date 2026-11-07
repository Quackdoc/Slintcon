# slintcon

A terminal emulator made with slint. Supports both fbdev and kms meaning it can
be used as a suitable alternative to agetty and kmscon without relying on wayland or
other external libraries. 

fbdev is the prefered mode since kms can not launch graphical sessions yet.

Run it on a VT 8:

```sh
sudo openvt -s -w -c 8 -l -- ./target/release/slintcon --kms
```

`--kms` selects the LinuxKMS backend (DRM). 
`--fbdev` selects the legacy framebuffer (`/dev/fb*`) This is the prefered mode
  of operation for a tty alternative and most closely mimicks one. This is also
  the only mode of operation that allows seemless launching of graphical sessions
  on top of it currently.
`--cpu` renders the terminal internals using cpu.
`--software` goes a step further and renders slint using software backend.

## AI Disclosure.

AI was used for some refactoring, AI was also exclusively used for the documentaton.
AI was also used for adding things like keymaps and other typing heavy non logic stuff.

## Mouse and clipboard

The mouse selects text (drag, double click for a word, triple click for a
line), copies it to the primary selection and pastes it with the middle
button; programs that use the mouse get it, and Shift takes it back.
Ctrl+Shift+C/V use the clipboard, which on a Wayland or X11 desktop is the
system one. 

## Keyboard

On KMS/Fbdev The keyboard is read from the VT like on a real console
(`K_MEDIUMRAW` plus the kernel's own console keymap), not from evdev, so keys
follow the active VT and stop when a compositor started from slintcon takes the
keyboard. The layout is therefore the console keymap (`loadkeys`,
`/etc/vconsole.conf`). 

`--slint-keyboard` reads slint instead, and slintcon
falls back to it (with a warning) when it can't take the VT keyboard. It relies
on slint's libinput feature.

## Login prompts on VTs (boot service)

`scripts/install-dinit.sh` installs a dinit template service plus one instance
per VT, the same shape dinit-rc uses for getty:

```
/etc/dinit.d/slintcon          template; `$1` is the VT name (`tty6`)
/etc/dinit.d/boot.d/slintcon@tty6   link that starts the instance at boot
```

```sh
sudo scripts/install-dinit.sh                 # --fbdev on tty7 tty8 tty9
sudo scripts/install-dinit.sh --kms 7 8       # choose backend and VTs
sudo scripts/install-dinit.sh --uninstall 7 8 9
sudo dinitctl start slintcon@tty7             # without rebooting
```

## Font

The font size is an absolute size in physical pixels. On `--kms`/`--fbdev`
the default is the largest zoom level whose grid still fits 80 columns across
the screen (the smallest level if none does), so the grid is as close to 80
columns as the levels allow; the row count follows from the screen height.

slintcon prefers Iosevka Term SS12. If it is not installed it falls back to the
first available of JetBrains Mono, DejaVu Sans Mono, Liberation Mono, etc., and
finally the system's generic monospace font. Build with
`--features bundled-font` to embed Iosevka Term SS12 (~10 MB, SIL OFL; license
in `assets/fonts/`) so it is used regardless of installed fonts.

## License

slintcon is GPL-3.0-only (see `LICENSE`), because it links Slint under its GPL
license. The `slint-terminal` widget crate is MIT licensed (see
`slint-terminal/LICENSE`). Third-party notices are in
`slint-terminal/THIRD-PARTY-NOTICES.md`; with the default `wgpu` feature
Slint's Skia renderer (BSD-3-Clause, Google) is also linked into the binary.
