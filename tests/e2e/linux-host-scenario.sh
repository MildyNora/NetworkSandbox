#!/bin/sh
set -eu

binary=/opt/netsandbox
state=/var/lib/netsandbox-e2e
original=/etc/netsandbox-e2e.conf
added=/etc/netsandbox-e2e-added.conf

"$binary" --state-dir "$state" doctor
printf '%s\n' host-original > "$original"

python3 -m http.server 18080 --bind 0.0.0.0 >/tmp/netsandbox-http.log 2>&1 &
server_pid=$!
trap 'kill "$server_pid" 2>/dev/null || true' EXIT
server_ip=$(ip -4 -o address show dev eth0 | awk '{split($4, address, "/"); print address[1]}')

"$binary" --state-dir "$state" create real-linux --capture-connections false
"$binary" --state-dir "$state" circuit add-tcp \
    real-linux transport "$server_ip:18080"
"$binary" --state-dir "$state" circuit add \
    real-linux application -- \
    /usr/bin/python3 -c \
    "import urllib.request; assert urllib.request.urlopen('http://$server_ip:18080', timeout=3).status == 200"

"$binary" --state-dir "$state" exec real-linux -- \
    /bin/sh -c \
    'echo sandbox-change > /etc/netsandbox-e2e.conf; echo added > /etc/netsandbox-e2e-added.conf'

test "$(cat "$original")" = host-original
test ! -e "$added"

check_output=$("$binary" --state-dir "$state" check real-linux --timeout 3)
printf '%s\n' "$check_output"
test "$(printf '%s\n' "$check_output" | grep -c Preserved)" -eq 2

"$binary" --state-dir "$state" diff real-linux
"$binary" --state-dir "$state" plan real-linux
apply_output=$("$binary" --state-dir "$state" apply real-linux --yes)
printf '%s\n' "$apply_output"

test "$(cat "$original")" = sandbox-change
test "$(cat "$added")" = added
transaction=$(printf '%s\n' "$apply_output" | sed -n 's/^Rollback transaction: //p')
test -n "$transaction"

"$binary" --state-dir "$state" rollback "$transaction" --yes
test "$(cat "$original")" = host-original
test ! -e "$added"

printf 'LINUX_HOST_E2E_OK transaction=%s\n' "$transaction"
