#!/usr/bin/env python3
"""Start throw-away FTP (2121), SSH/SFTP (2222) and TFTP (6969) servers on 127.0.0.1."""
import asyncio
import os
import threading

import asyncssh
import tftpy
from pyftpdlib.authorizers import DummyAuthorizer
from pyftpdlib.handlers import FTPHandler
from pyftpdlib.servers import FTPServer

USER, PASSWORD = "tester", "s3cret"
DATA = os.path.join(os.path.dirname(os.path.abspath(__file__)), ".data")
FTP_ROOT, TFTP_ROOT, SSH_ROOT = (os.path.join(DATA, d) for d in ("ftproot", "tftproot", "sshroot"))
for d in (FTP_ROOT, TFTP_ROOT, SSH_ROOT):
    os.makedirs(d, exist_ok=True)


def ftp():
    a = DummyAuthorizer()
    a.add_user(USER, PASSWORD, FTP_ROOT, perm="elradfmwMT")
    a.add_anonymous(FTP_ROOT)
    h = FTPHandler
    h.authorizer = a
    h.passive_ports = range(30000, 30100)
    FTPServer(("127.0.0.1", 2121), h).serve_forever()


def tftp():
    tftpy.TftpServer(TFTP_ROOT).listen("127.0.0.1", 6969)


def keys():
    def p(n):
        return os.path.join(DATA, n)

    if not os.path.exists(p("host_key")):
        asyncssh.generate_private_key("ssh-ed25519").write_private_key(p("host_key"))
    if not os.path.exists(p("client_ed25519")):
        k = asyncssh.generate_private_key("ssh-ed25519")
        k.write_private_key(p("client_ed25519"), passphrase="keypass")
        k.write_public_key(p("client_ed25519.pub"))
        r = asyncssh.generate_private_key("ssh-rsa", key_size=2048)
        r.write_private_key(p("client_rsa"), format_name="pkcs1-pem")
        r.write_public_key(p("client_rsa.pub"))
    pubs = open(p("client_ed25519.pub")).read() + open(p("client_rsa.pub")).read()
    return p("host_key"), asyncssh.import_authorized_keys(pubs)


HOST_KEY, AUTH_KEYS = keys()


class Server(asyncssh.SSHServer):
    def begin_auth(self, username):
        return True

    def password_auth_supported(self):
        return True

    def validate_password(self, username, password):
        return username == USER and password == PASSWORD

    def public_key_auth_supported(self):
        return True

    def validate_public_key(self, username, key):
        return username == USER and AUTH_KEYS.validate(key, "127.0.0.1", "127.0.0.1") is not None


async def handle(process):
    if not process.command:
        process.stderr.write("interactive shells are not supported\n")
        process.exit(1)
        return
    p = await asyncio.create_subprocess_shell(
        process.command, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE, cwd=SSH_ROOT
    )
    out, err = await p.communicate()
    process.stdout.write(out.decode(errors="replace"))
    process.stderr.write(err.decode(errors="replace"))
    process.exit(p.returncode)


class Sftp(asyncssh.SFTPServer):
    def __init__(self, chan):
        super().__init__(chan, chroot=SSH_ROOT)


async def ssh():
    await asyncssh.create_server(
        Server, "127.0.0.1", 2222, server_host_keys=[HOST_KEY], process_factory=handle, sftp_factory=Sftp
    )
    print("FTP 127.0.0.1:2121 | SSH/SFTP 127.0.0.1:2222 | TFTP 127.0.0.1:6969  (user tester / s3cret)", flush=True)
    await asyncio.Future()


if __name__ == "__main__":
    threading.Thread(target=ftp, daemon=True).start()
    threading.Thread(target=tftp, daemon=True).start()
    try:
        asyncio.run(ssh())
    except KeyboardInterrupt:
        pass
