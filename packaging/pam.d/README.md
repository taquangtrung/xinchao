# PAM drop-in snippets

These are **examples**, not files to copy verbatim over your system's PAM config.
Editing PAM wrong can lock you out of `sudo` and login. Read
[`docs/INSTALL.md`](../../docs/INSTALL.md) first, and keep a root shell open while
you work.

## The one line that matters

To add face auth to a service, put this as the **first `auth` line** of its
`/etc/pam.d/<service>` file:

```
auth    sufficient    pam_xinchao.so
```

`sufficient` is deliberate: a face match satisfies `auth` and the stack returns
success; any failure (no match, no camera, timeout, Ctrl-C) falls through to the
lines below, which are your normal password prompt. The password is never
removed, so this is purely additive.

Do **not** use `required` or `requisite`: that would make a face check mandatory
and a camera failure would block the password fallback.

## Files here

- [`sudo.example`](sudo.example) - a complete annotated `/etc/pam.d/sudo` showing
  where the line goes relative to the usual `@include` lines.

## polkit

For graphical privilege prompts, add the same line to the top of
`/etc/pam.d/polkit-1`. Test with `pkexec true` (with a root shell open) before
relying on it.

## Verifying safely

Before touching `sudo`, test the module against a throwaway service with
`pamtester` (see [`docs/INSTALL.md`](../../docs/INSTALL.md)), so a misconfiguration
can never affect a real login path.
