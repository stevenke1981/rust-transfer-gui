# Changelog

## 0.1.1 - 2026-10-04

SSH connection fixes (reported: "cannot connect via SSH").

- Password logins now work on servers that only offer `keyboard-interactive`
  (OpenSSH with `PasswordAuthentication no` + `KbdInteractiveAuthentication yes`, the usual
  PAM setup): the offered methods are queried first and the password is sent through
  keyboard-interactive when `password` is not available or is rejected.
- Servers without an SFTP subsystem (routers, embedded/dropbear builds) no longer fail to
  connect; the SSH terminal works and the file browser reports that SFTP is unavailable.
- Every resolved address (IPv6 and IPv4) is tried with a connect timeout instead of only the
  first one (e.g. `localhost` → `::1` first on Windows); applies to FTP too.
- Detailed connection log and errors: TCP peer, server software version, negotiated
  kex/host key/cipher/MAC, offered authentication methods, and errors prefixed with the
  failing stage (`DNS:`, `TCP:`, `SSH handshake:`, `Auth:`) including the libssh2 error
  name/code and a hint. PuTTY `.ppk` and `.pub` files picked as private keys are detected.
- CI: new end-to-end job against real OpenSSH `sshd` (password, keyboard-interactive only,
  no SFTP, strict modern algorithms, rsa-sha2-512/aes-gcm); scripts in
  `scripts/test-servers/openssh/`.

## 0.1.0 - 2026-10-02

- First release: FTP, SFTP/SSH and TFTP desktop client with Windows installer.
