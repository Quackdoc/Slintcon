# TODO

- libseat was removed; restoring it needs upstream seat `Disable`/`Enable` dispatch.
- Prefer `https://github.com/verdiwm/saddle` over raw `libseat` FFI once it supports `seatd`.
- Font family is hardcoded; look at `slint::fontique` and overhaul the font system.
- Forced exit skips Slint's console restore; with libinput `Keyboard::engage` stores `K_OFF` as the mode to restore (treat it like `K_MEDIUMRAW`).
- On fbdev, forced exit via `die_by` leaves `KD_GRAPHICS`/`VT_PROCESS` when `upstream_restores` is set; add an `UPSTREAM_RESTORES` flag.
- Properly set Slint's automatic dumb-buffer to linuxfb fallback from kms to fbdev.

## AI generated todos

- Session integration: export `XDG_SEAT=seat0`/`XDG_VTNR` and run `login -p` so `pam_elogind` attaches the session to the VT (unverified)?
- Bell: BEL is ignored; flash the screen, beep via `KDMKTONE`/`KIOCSOUND`, or both.
- Idle: no screen blanking or DPMS; needs an idle timer and a blank method (DPMS/backlight on KMS, `FBIOBLANK` on fbdev).
- Multiple outputs: Slint's LinuxKMS backend drives one output; multi-monitor needs upstream support or our own backend.
- Autologin: an `--autologin USER` option (`login -f USER`).
- Login flag: a `--login` shortcut for `-- /bin/login` with the session environment.
- Focus reports (DECSET 1004): send `ESC [ I`/`ESC [ O`; on KMS/fbdev derive focus from the VT becoming active.
- Pixel mouse (DECSET 1016): report pixel positions; rio-vt has no 1016 bit, so it needs a patch or our own mode tracking.
- Mouse: hyperlinks (OSC 8), keyboard selection (vi mode), horizontal wheel reports, configurable selection colours.
- utmp/wtmp: `login` records `pts/N` instead of `ttyN`; dinit writes the VT's own records.
