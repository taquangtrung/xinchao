# Xin Chao

> **xin chào** = "hello" in Vietnamese.

A Windows Hello-style **IR face-unlock** for Linux: authenticate `login`, `sudo`,
screen unlock, and `polkit` with an infrared camera. Written in Rust, integrated
via PAM, and **additive**, the password always stays as fallback.

Recognition runs on ONNX Runtime (UltraFace detection + ArcFace embedding,
checksum-pinned, fetched on first use) and accepts only when several frames agree.
IR defeats photo and screen spoofing but is not full liveness detection, so treat
it as convenience-plus-security, not high-assurance.

> **Status: pre-1.0.** End-to-end recognition needs a working IR illuminator,
> which on some laptops is dark until activated. Test it in isolation before
> putting it in front of a real login.

## Layout

| Crate | Kind | Purpose |
|---|---|---|
| `xinchao` | lib + bin (`xinchao`) + cdylib (`pam_xinchao.so`) | Core pipeline, the CLI, and the PAM module |
| `xinchao-gui` | bin (`xinchao-gui`) | Native egui app for enrollment and management |

## Setup

```sh
./install-prerequisites.sh   # apt deps, models, IR-emitter tool + activation
cargo build && cargo test
```

`linux-enable-ir-emitter` activates the illuminator (dark by default on many
laptops). `make help` lists the build/install targets.

## CLI

```sh
xinchao diagnose --capture /tmp/ir.png   # find the IR node; check it isn't black
sudo xinchao add --user "$USER"          # enroll: capture frames, store embeddings
xinchao test --user "$USER"              # live recognition attempt (no auth effect)
xinchao config                           # effective config + permission status
xinchao remove --user "$USER"            # delete an enrollment
```

## PAM authentication

```sh
make install     # builds + installs CLI, GUI, pam_xinchao.so, config, and models
```

Then add `auth sufficient pam_xinchao.so` as the first auth line of the service
(see [`packaging/pam.d/`](packaging/pam.d/)). **Keep a root shell open while
editing `/etc/pam.d`**, and use `sufficient` (never `required`) so a camera
failure falls back to the password. Full reversible procedure:
[`docs/INSTALL.md`](docs/INSTALL.md). Keystroke-free lock-screen unlock:
[`docs/HANDS_FREE_UNLOCK.md`](docs/HANDS_FREE_UNLOCK.md).

## Graphical app

`xinchao-gui` is an unprivileged egui app: live IR preview, enrollment, PAM-service
toggles, and diagnostics. It shells root-owned writes out to `pkexec xinchao` and
is never in the auth path. Launch it by name after `make install`, or from your app
menu.

## Security model

Enforced in code:

- **Fail closed.** Any error (no camera, timeout, low confidence, parse/model
  failure) denies; only a verified match returns `PAM_SUCCESS`.
- **Password fallback** always available via `sufficient`.
- **Conservative threshold** favors false rejects over false accepts.
- **Root-owned config and models**; the PAM path refuses anything world-writable.
- **IR-only matching**, per-attempt timeout, no frames/embeddings in logs, attempts
  audited to `authpriv` syslog.

Crafted IR imagery can still spoof it; this is not high-assurance. Report security
issues privately.

## Acknowledgement

Xin Chao is inspired by [Howdy](https://github.com/boltgolt/howdy), the
Python-based Windows Hello-style IR face unlock that proved the idea works on
Linux. Xin Chao reimplements it in Rust: a small, memory-safe PAM module with a fail-closed,
root-owned-config security posture and hands-free lock-screen unlock built in.

## License

MIT (see [`LICENSE`](LICENSE)). Models download at install time under their own
licenses (Apache-2.0, MIT) and are not redistributed here.
