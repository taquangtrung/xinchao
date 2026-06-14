# Installing xinchao

> **Safety first.** xinchao plugs into PAM, which authenticates `sudo` and login.
> A bad `/etc/pam.d` edit can lock you out. Keep a root shell open during every
> PAM change, and always use `sufficient` (not `required`) so the password stays a
> fallback.

## 1. Prerequisites

Ubuntu/Debian family. You need an IR camera whose illuminator actually fires; on
many laptops it is dark until activated. Check with `xinchao diagnose --capture
/tmp/ir.png`: if the frame is black, recognition cannot work yet.

```sh
./install-prerequisites.sh   # apt deps, models, and IR-emitter activation
```

## 2. Build and install

```sh
make install
```

Builds release binaries and, with `sudo`: installs the `xinchao` CLI and the PAM
module (`pam_xinchao.so`), writes `/etc/xinchao/config.toml` from the example if
absent, and downloads the models into `/etc/xinchao/models` (~260 MB first run).
Override paths with e.g. `make install PREFIX=/usr` or
`make install PAM_MODULE_DIR=/usr/lib/x86_64-linux-gnu/security`. Verify with
`xinchao config`.

## 3. Enroll your face

```sh
sudo xinchao add --user "$USER"     # capture frames, store embeddings (root-owned)
xinchao list                        # confirm enrollment
xinchao test --user "$USER"         # live match attempt, no auth side effects
```

Tune `threshold` in `/etc/xinchao/config.toml` (lower is stricter; prefer false
rejects over false accepts).

## 4. Test the PAM module safely

Use a throwaway service that cannot lock you out:

```sh
sudo apt install pamtester
printf 'auth sufficient pam_xinchao.so\nauth required pam_permit.so\n' \
  | sudo tee /etc/pam.d/xinchao-test
pamtester xinchao-test "$USER" authenticate
sudo rm /etc/pam.d/xinchao-test     # clean up when done
```

## 5. Enable it for a real service

**Open a second terminal with `sudo -s` first** so you can undo a bad edit. Add
the face line to the top of the service's auth stack (see
[`../packaging/pam.d/sudo.example`](../packaging/pam.d/sudo.example)):

```sh
sudoedit /etc/pam.d/sudo
#   auth    sufficient    pam_xinchao.so     # first auth line, above @include common-auth
sudo -k && sudo true                         # test: accepts your face, or falls back to password
```

For lock-screen unlock with no keystroke, see
[`HANDS_FREE_UNLOCK.md`](HANDS_FREE_UNLOCK.md).

## Uninstalling

```sh
# First remove any `pam_xinchao.so` lines you added to /etc/pam.d/*.
make uninstall            # removes the CLI, GUI, and PAM module
sudo rm -rf /etc/xinchao  # optional: also delete config, models, and enrollments
```
