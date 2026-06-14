# Hands-free face unlock

The lock screen dismisses on a face match with no keystroke. This is separate
from the PAM setup in [`INSTALL.md`](INSTALL.md): PAM accepts your face *at* a
password prompt; the daemon here unlocks an already-running session for you.

`make install` enables all of this. Two systemd units do the work:

- **`xinchao-ir-emitter.service`** keeps the IR illuminator lit (it resets to
  dark on power-cycle and resume), replaying the activation at boot and resume.
- **`xinchao-unlockd@<user>.service`** watches logind for that user's locked
  session and, on a face match, runs `loginctl unlock-session`. It only unlocks
  sessions owned by `<user>` against that user's enrollment, and treats any error
  as "stay locked".

## Setup by hand

```sh
sudo xinchao enable-ir --apply          # one time, looking at the camera: persist the emitter payload
sudo systemctl enable --now xinchao-ir-emitter.service
sudo systemctl enable --now "xinchao-unlockd@$USER"
journalctl -u "xinchao-unlockd@$USER" -f   # lock the screen and look at the camera to verify
```

If nothing unlocks, the journal shows why (usually dark frames: re-run
`enable-ir --apply`, or a non-match: lower `threshold` in
`/etc/xinchao/config.toml`).

## Optional: skip the boot login prompt

```sh
make enable-autologin     # LightDM autologin for your user
make disable-autologin    # restore the boot login prompt
```

Trade-off: with autologin on, anyone who powers on the machine reaches your
desktop, so the lock screen becomes the only face gate. Lock it when you step
away.

## Disabling

```sh
sudo systemctl disable --now "xinchao-unlockd@$USER" xinchao-ir-emitter.service
make disable-autologin    # if you enabled it
```
