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
