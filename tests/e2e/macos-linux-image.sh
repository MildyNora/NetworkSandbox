#!/bin/sh
set -eu

if [ "$#" -ne 2 ]; then
    echo "usage: $0 MAC_NETSANDBOX LINUX_GUEST_NETSANDBOX" >&2
    exit 2
fi

mac_binary_source=$1
guest_binary_source=$2
root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
suffix="$$"
source_image="netsandbox-e2e-source:${suffix}"
output_image="netsandbox-e2e-output:${suffix}"
service="netsandbox-e2e-http-${suffix}"
work=$(mktemp -d /tmp/netsandbox-image-e2e.XXXXXX)
state="$work/state"
tools="$work/tools"
mkdir -p "$state" "$tools"
cp "$mac_binary_source" "$tools/netsandbox"
chmod 755 "$tools/netsandbox"
if [ "${NETSANDBOX_E2E_EMBEDDED:-0}" != 1 ]; then
    cp "$guest_binary_source" "$tools/netsandbox-linux-guest"
    chmod 755 "$tools/netsandbox-linux-guest"
fi
mac_binary="$tools/netsandbox"

cleanup() {
    "$mac_binary" --state-dir "$state" discard linux-image --yes >/dev/null 2>&1 || true
    docker rm -f "$service" >/dev/null 2>&1 || true
    docker image rm "$output_image" "$source_image" >/dev/null 2>&1 || true
    rm -rf "$work"
}
trap cleanup EXIT INT TERM

docker build --tag "$source_image" "$root/tests/e2e"
docker run --rm -d --name "$service" --network bridge \
    "$source_image" python3 -m http.server 18081 --bind 0.0.0.0 >/dev/null
service_ip=$(docker inspect \
    --format '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' \
    "$service")
source_before=$(docker run --rm "$source_image" cat /etc/debian_version)

"$mac_binary" --state-dir "$state" create linux-image --capture-connections false
"$mac_binary" --state-dir "$state" mac linux-create \
    linux-image "$source_image"
printf 'exit\n' | "$mac_binary" --state-dir "$state" enter linux-image \
    >"$work/enter.out" 2>&1
grep -q '\[nsb:linux-image\]' "$work/enter.out"
"$mac_binary" --state-dir "$state" exec linux-image -- \
    /bin/sh -c 'echo too-late > /etc/netsandbox-late.conf'
if "$mac_binary" --state-dir "$state" mac linux-track \
    linux-image /etc/netsandbox-late.conf >"$work/track.out" 2>&1; then
    echo "tracking a previously changed file unexpectedly succeeded" >&2
    exit 3
fi
grep -q 'was already changed' "$work/track.out"
"$mac_binary" --state-dir "$state" mac linux-reset linux-image --yes
"$mac_binary" --state-dir "$state" mac linux-track \
    linux-image /etc/debian_version
"$mac_binary" --state-dir "$state" circuit add-tcp \
    linux-image transport "$service_ip:18081"
"$mac_binary" --state-dir "$state" circuit add \
    linux-image application -- \
    /usr/bin/python3 -c \
    "import urllib.request; assert urllib.request.urlopen('http://$service_ip:18081', timeout=3).status == 200"
"$mac_binary" --state-dir "$state" exec linux-image -- \
    /bin/sh -c 'echo netsandbox-modified > /etc/debian_version'
"$mac_binary" --state-dir "$state" check linux-image --timeout 3
"$mac_binary" --state-dir "$state" diff linux-image
"$mac_binary" --state-dir "$state" mac linux-diff linux-image

if "$mac_binary" --state-dir "$state" plan linux-image >"$work/plan.out" 2>&1; then
    echo "host apply unexpectedly accepted a Linux image environment" >&2
    exit 3
fi
grep -q 'cannot be applied to the Mac host' "$work/plan.out"

"$mac_binary" --state-dir "$state" mac linux-commit \
    linux-image "$output_image" --yes
test "$(docker image inspect "$output_image" --format '{{json .Config.Entrypoint}}')" = null
test "$(docker run --rm "$output_image" cat /etc/debian_version)" = netsandbox-modified
test "$(docker run --rm "$source_image" cat /etc/debian_version)" = "$source_before"

"$mac_binary" --state-dir "$state" mac linux-rollback linux-image --yes
"$mac_binary" --state-dir "$state" discard linux-image --yes
docker rm -f "$service" >/dev/null
docker image rm "$source_image" >/dev/null
rm -rf "$work"
trap - EXIT INT TERM

printf 'MACOS_LINUX_IMAGE_E2E_OK\n'
