# Local test servers

Small Python servers used for the end-to-end tests in `tests/integration.rs`.
They listen on **127.0.0.1 only** and use throw-away credentials.

```bash
python3 -m venv .venv && . .venv/bin/activate
pip install pyftpdlib tftpy asyncssh bcrypt
python run_servers.py            # FTP :2121, SSH/SFTP :2222, TFTP :6969 (Ctrl+C to stop)
```

In another terminal:

```bash
export RTG_FTP=127.0.0.1:2121:tester:s3cret
export RTG_SSH=127.0.0.1:2222:tester:s3cret
export RTG_SSH_KEY=$PWD/scripts/test-servers/.data/client_ed25519 RTG_SSH_KEY_PASSPHRASE=keypass
export RTG_SSH_KEY2=$PWD/scripts/test-servers/.data/client_rsa
export RTG_TFTP=127.0.0.1:6969
cargo test --test integration -- --ignored --test-threads=1
```

The SSH server is [AsyncSSH](https://asyncssh.readthedocs.io/) (pure Python, no root
needed). Remote commands run as the current user inside `.data/sshroot`.

## Real OpenSSH `sshd`

[`openssh/start-sshd.sh`](openssh/start-sshd.sh) starts five `sshd` instances on
127.0.0.1/::1 ports 2201–2205: password (Ubuntu default), **keyboard-interactive only**
(`PasswordAuthentication no`, `KbdInteractiveAuthentication yes`), no SFTP subsystem,
modern-only algorithms (curve25519/post-quantum kex, ssh-ed25519, chacha20-poly1305) and
rsa-sha2-512 + aes256-gcm. CI (`openssh` job in `.github/workflows/ci.yml`) runs it as root
with a real user and PAM:

```bash
sudo useradd -m rtgtest && echo 'rtgtest:Rtg-test-Pa55word' | sudo chpasswd
sudo scripts/test-servers/openssh/start-sshd.sh /tmp/rtg-sshd
export RTG_SSH=localhost:2201:rtgtest:Rtg-test-Pa55word RTG_SSH_EXPECT_SERVER=OpenSSH
export RTG_SSH_KBD=127.0.0.1:2202:rtgtest:Rtg-test-Pa55word
export RTG_SSH_NOSFTP=127.0.0.1:2203:rtgtest:Rtg-test-Pa55word
export RTG_SSH_EXTRA="127.0.0.1:2204:rtgtest:Rtg-test-Pa55word;127.0.0.1:2205:rtgtest:Rtg-test-Pa55word"
cargo test --test integration ssh -- --ignored --test-threads=1 --nocapture
```

Without root, `sshd` can only log in the user running it and cannot use the system PAM
stack. Build the fake PAM in [`openssh/pamshim.c`](openssh/pamshim.c) (it asks one
`Password:` question and accepts `$SHIM_PASSWORD`, default `s3cret`) and preload it:

```bash
gcc -shared -fPIC -o /tmp/libpamshim.so scripts/test-servers/openssh/pamshim.c
LD_PRELOAD=/tmp/libpamshim.so scripts/test-servers/openssh/start-sshd.sh /tmp/rtg-sshd
export RTG_SSH=localhost:2201:$USER:s3cret   # etc.
scripts/test-servers/openssh/start-sshd.sh /tmp/rtg-sshd stop
```

(OpenSSH ≥ 9.8 split `sshd`; for an unpacked `.deb` set `SSHD=…/usr/sbin/sshd`, `SSH`,
`SSH_KEYGEN` and `SSHD_EXTRA="SshdSessionPath …"` accordingly.)
