#!/usr/bin/env bash
# Start real OpenSSH sshd instances for tests/integration.rs (used by CI, works locally too).
#
#   start-sshd.sh <state-dir>         # start (or restart) all instances
#   start-sshd.sh <state-dir> stop    # stop them
#
# Instances (BASE_PORT defaults to 2201):
#   BASE+0  pw      PasswordAuthentication yes, KbdInteractiveAuthentication no (Ubuntu/Debian default)
#   BASE+1  kbd     PasswordAuthentication no,  KbdInteractiveAuthentication yes (password only via PAM
#                   keyboard-interactive – what libssh2's plain password auth cannot handle)
#   BASE+2  nosftp  no sftp subsystem (like many routers / embedded servers)
#   BASE+3  modern  only curve25519/post-quantum kex, ssh-ed25519 host key, chacha20-poly1305
#   BASE+4  rsagcm  only rsa-sha2-512 host key, ecdh-sha2-nistp256, aes256-gcm
#
# Environment:
#   SSHD         sshd binary (default /usr/sbin/sshd; must be an absolute path)
#   SSH          ssh client of the same version, used to query algorithms (default ssh)
#   SSH_KEYGEN   default ssh-keygen
#   SFTP_SERVER  sftp-server binary (auto-detected)
#   BASE_PORT    first port (default 2201)
#   SSHD_EXTRA   extra lines for every config (e.g. "SshdSessionPath /x/sshd-session")
#   AUTHORIZED_KEYS  authorized_keys file (default <state-dir>/authorized_keys)
#
# Authentication uses PAM (UsePAM yes). In CI the script runs as root with a real user.
# Locally, without root, preload the fake PAM in pamshim.c (accepts $SHIM_PASSWORD) – see README.md.
set -euo pipefail
dir=$(mkdir -p "$1" && cd "$1" && pwd)
action=${2:-start}
SSHD=${SSHD:-/usr/sbin/sshd}
SSH=${SSH:-ssh}
SSH_KEYGEN=${SSH_KEYGEN:-ssh-keygen}
BASE_PORT=${BASE_PORT:-2201}
AUTHORIZED_KEYS=${AUTHORIZED_KEYS:-$dir/authorized_keys}
names=(pw kbd nosftp modern rsagcm)

for n in "${names[@]}"; do
  if [[ -f $dir/sshd_$n.pid ]]; then kill "$(cat "$dir/sshd_$n.pid")" 2>/dev/null || true; rm -f "$dir/sshd_$n.pid"; fi
done
[[ $action == stop ]] && exit 0

if [[ -z ${SFTP_SERVER:-} ]]; then
  for c in "$(dirname "$SSHD")/../lib/openssh/sftp-server" /usr/lib/openssh/sftp-server /usr/libexec/openssh/sftp-server /usr/libexec/sftp-server; do
    [[ -x $c ]] && SFTP_SERVER=$(cd "$(dirname "$c")" && pwd)/sftp-server && break
  done
fi
: "${SFTP_SERVER:?sftp-server not found; set SFTP_SERVER}"
touch "$AUTHORIZED_KEYS"

for t in ed25519 ecdsa rsa; do
  [[ -f $dir/host_$t ]] || "$SSH_KEYGEN" -q -t "$t" -N '' -f "$dir/host_$t"
done

# Keep only algorithms this OpenSSH version knows.
supported() { # <kind> <comma separated candidates>
  local avail out=()
  avail=$("$SSH" -Q "$1" 2>/dev/null || true)
  IFS=, read -ra cand <<<"$2"
  for a in "${cand[@]}"; do grep -qx -- "$a" <<<"$avail" && out+=("$a"); done
  (IFS=,; echo "${out[*]}")
}
modern_kex=$(supported kex mlkem768x25519-sha256,sntrup761x25519-sha512,sntrup761x25519-sha512@openssh.com,curve25519-sha256,curve25519-sha256@libssh.org)

write_config() { # <name> <port> <extra lines>
  local f=$dir/sshd_$1.conf
  cat >"$f" <<CONF
Port $2
ListenAddress 127.0.0.1
ListenAddress ::1
HostKey $dir/host_ed25519
HostKey $dir/host_ecdsa
HostKey $dir/host_rsa
PidFile $dir/sshd_$1.pid
AuthorizedKeysFile $AUTHORIZED_KEYS .ssh/authorized_keys
StrictModes no
UsePAM yes
LogLevel VERBOSE
${SSHD_EXTRA:-}
$3
CONF
  # Repeated failed logins from 127.0.0.1 would otherwise get the tests penalised (OpenSSH >= 9.8).
  echo "PerSourcePenalties no" >>"$f"
  "$SSHD" -t -f "$f" 2>/dev/null || sed -i '/^PerSourcePenalties/d' "$f"
  "$SSHD" -t -f "$f"
}

sftp="Subsystem sftp $SFTP_SERVER"
write_config pw $((BASE_PORT + 0)) "PasswordAuthentication yes
KbdInteractiveAuthentication no
$sftp"
write_config kbd $((BASE_PORT + 1)) "PasswordAuthentication no
KbdInteractiveAuthentication yes
$sftp"
write_config nosftp $((BASE_PORT + 2)) "PasswordAuthentication yes
KbdInteractiveAuthentication yes"
write_config modern $((BASE_PORT + 3)) "PasswordAuthentication yes
KbdInteractiveAuthentication no
KexAlgorithms $modern_kex
HostKeyAlgorithms ssh-ed25519
Ciphers chacha20-poly1305@openssh.com
$sftp"
write_config rsagcm $((BASE_PORT + 4)) "PasswordAuthentication yes
KbdInteractiveAuthentication no
KexAlgorithms ecdh-sha2-nistp256
HostKeyAlgorithms rsa-sha2-512
Ciphers aes256-gcm@openssh.com
$sftp"

for n in "${names[@]}"; do
  "$SSHD" -E "$dir/sshd_$n.log" -f "$dir/sshd_$n.conf"
done
sleep 1
for n in "${names[@]}"; do
  [[ -f $dir/sshd_$n.pid ]] || { echo "sshd $n did not start:"; cat "$dir/sshd_$n.log"; exit 1; }
done
"$SSHD" -V 2>&1 || true
echo "sshd instances running on ports $BASE_PORT-$((BASE_PORT + 4)) (state in $dir)"
