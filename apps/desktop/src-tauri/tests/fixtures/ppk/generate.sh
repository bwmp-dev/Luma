#!/bin/sh
# Regenerates the PuTTY .ppk test fixtures in this directory.
#
# The keys are throwaway test material with a published passphrase; they exist
# only so the .ppk parser can be checked against files real PuTTY produced.
# Never reuse them for anything.
#
# The oracle chain is deliberately free of Luma code: OpenSSH's ssh-keygen
# generates the key, PuTTY's puttygen converts it to .ppk, and the test asserts
# that converting back yields the original key material. A bug in our parser
# cannot hide, because neither side of the comparison is ours.
#
# Run from this directory:
#     docker run --rm -v "$PWD:/out" debian:bookworm-slim sh /out/generate.sh
set -eu

PASSPHRASE='luma test passphrase'
OUT=/out

if ! command -v puttygen >/dev/null 2>&1; then
  apt-get update -qq
  apt-get install -y -qq putty-tools openssh-client >/dev/null
fi

WORK=$(mktemp -d)
printf '%s' "$PASSPHRASE" >"$WORK/pass"

# generate <fixture-name> <ssh-keygen-type> <bits-or-empty> <ppk-version> <plain|encrypted>
generate() {
  name=$1
  type=$2
  bits=$3
  version=$4
  mode=$5

  src="$WORK/$name"
  if [ -n "$bits" ]; then
    ssh-keygen -q -t "$type" -b "$bits" -N '' -C "luma-fixture-$name" -f "$src"
  else
    ssh-keygen -q -t "$type" -N '' -C "luma-fixture-$name" -f "$src"
  fi

  if [ "$mode" = encrypted ]; then
    puttygen "$src" -O private -o "$OUT/$name.ppk" \
      --ppk-param "version=$version" --new-passphrase "$WORK/pass"
  else
    puttygen "$src" -O private -o "$OUT/$name.ppk" --ppk-param "version=$version"
  fi

  # The OpenSSH key ssh-keygen produced, kept as the oracle for the test.
  cp "$src" "$OUT/$name.openssh"
  cp "$src.pub" "$OUT/$name.pub"
}

generate ed25519_v2_plain      ed25519 ''    2 plain
generate ed25519_v2_encrypted  ed25519 ''    2 encrypted
generate ed25519_v3_plain      ed25519 ''    3 plain
generate ed25519_v3_encrypted  ed25519 ''    3 encrypted
generate rsa2048_v2_encrypted  rsa     2048  2 encrypted
generate rsa2048_v3_plain      rsa     2048  3 plain
generate ecdsa_p256_v3_encrypted ecdsa 256   3 encrypted
generate ecdsa_p521_v2_plain   ecdsa   521   2 plain

rm -rf "$WORK"
echo "fixtures written to $OUT"
