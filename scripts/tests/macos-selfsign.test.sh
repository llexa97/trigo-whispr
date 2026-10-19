#!/usr/bin/env bash
#
# Regression tests for scripts/macos-selfsign.sh. They run on any OS: uname,
# security, openssl and codesign are replaced by fakes that keep a fake
# keychain in a temporary directory, so the real keychain is never touched.
#
#   bash scripts/tests/macos-selfsign.test.sh
#
set -euo pipefail

SCRIPT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/macos-selfsign.sh"
ROOT="$(mktemp -d)"
trap 'rm -rf "$ROOT"' EXIT

FAKE_BIN="$ROOT/bin"
mkdir -p "$FAKE_BIN"

cat > "$FAKE_BIN/uname" <<'EOF'
#!/usr/bin/env bash
echo Darwin
EOF

# openssl: `req` writes a random fake certificate carrying the CN, `pkcs12`
# copies it into the "p12". Each new certificate gets a different hash.
cat > "$FAKE_BIN/openssl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
cmd="$1"; shift
out="" in="" subj=""
while (($#)); do
  case "$1" in
    -out) out="$2"; shift ;;
    -in) in="$2"; shift ;;
    -subj) subj="$2"; shift ;;
  esac
  shift
done
case "$cmd" in
  req)
    {
      echo "-----BEGIN CERTIFICATE-----"
      echo "${subj#/}"
      od -An -tx1 -N16 /dev/urandom | tr -d ' \n'; echo
      echo "-----END CERTIFICATE-----"
    } > "$out" ;;
  pkcs12) cp "$in" "$out" ;;
esac
EOF

# security: identities live in $FAKE_KEYCHAIN/ids/<sha1>/{name,cert.pem,trusted}.
cat > "$FAKE_BIN/security" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
ids="$FAKE_KEYCHAIN/ids"
mkdir -p "$ids"
log() { echo "$*" >> "$FAKE_KEYCHAIN/log"; }
hash_of() { sha1sum "$1" | cut -c1-40 | tr a-f A-F; }
list() { # $1 = 1 for valid identities only
  local n=0 h
  for d in "$ids"/*/; do
    [[ -d "$d" ]] || continue
    h="$(basename "$d")"
    if [[ "$1" == 1 && ! -e "$d/trusted" ]]; then continue; fi
    n=$((n + 1))
    if [[ -e "$d/trusted" ]]; then
      echo "  $n) $h \"$(cat "$d/name")\""
    else
      echo "  $n) $h \"$(cat "$d/name")\" (CSSMERR_TP_NOT_TRUSTED)"
    fi
  done
  echo "     $n identities found"
}
cmd="$1"; shift
case "$cmd" in
  find-identity)
    echo "Policy: Code Signing"
    if [[ " $* " == *" -v "* ]]; then
      echo "  Valid identities only"; list 1
    else
      echo "  Matching identities"; list 0
      echo; echo "  Valid identities only"; list 1
    fi ;;
  find-certificate)
    for d in "$ids"/*/; do
      [[ -d "$d" ]] || continue
      echo "SHA-256 hash: 0000"
      echo "SHA-1 hash: $(basename "$d")"
      cat "$d/cert.pem"
    done ;;
  import)
    h="$(hash_of "$1")"
    log "import $h"
    mkdir -p "$ids/$h"
    cp "$1" "$ids/$h/cert.pem"
    sed -n 's/^CN=//p' "$1" > "$ids/$h/name" ;;
  add-trusted-cert)
    for cert; do :; done
    h="$(hash_of "$cert")"
    log "trust $h"
    [[ -d "$ids/$h" ]] || { echo "unknown certificate" >&2; exit 1; }
    if [[ "${FAKE_TRUST:-allow}" == deny ]]; then exit 1; fi
    touch "$ids/$h/trusted" ;;
  *) echo "fake security: unsupported $cmd" >&2; exit 1 ;;
esac
EOF

cat > "$FAKE_BIN/codesign" <<'EOF'
#!/usr/bin/env bash
prev=""
for a; do
  [[ "$prev" == --sign ]] && echo "sign $a" >> "$FAKE_KEYCHAIN/log"
  prev="$a"
done
exit 0
EOF

chmod +x "$FAKE_BIN"/*

APP="$ROOT/Trigo Whispr.app"
mkdir -p "$APP"
failures=0

new_keychain() {
  export FAKE_KEYCHAIN="$ROOT/keychain-$1"
  mkdir -p "$FAKE_KEYCHAIN"
  : > "$FAKE_KEYCHAIN/log"
}

run_script() {
  PATH="$FAKE_BIN:$PATH" HOME="$ROOT" bash "$SCRIPT" "$APP" > "$ROOT/out" 2>&1
}

identity_count() { find "$FAKE_KEYCHAIN/ids" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' '; }
only_identity() { basename "$(find "$FAKE_KEYCHAIN/ids" -mindepth 1 -maxdepth 1 -type d)"; }
log_count() { grep -c "^$1" "$FAKE_KEYCHAIN/log" || true; }

check() { # description, condition...
  local what="$1"; shift
  if "$@"; then
    echo "ok   - $what"
  else
    echo "FAIL - $what"
    sed 's/^/       /' "$ROOT/out"
    failures=$((failures + 1))
  fi
}

# First import -> trust refused -> rerun: the identity is reused, not recreated.
new_keychain refused
FAKE_TRUST=deny run_script
check "first run creates one identity" test "$(identity_count)" = 1
first="$(only_identity)"
check "first run signs with its hash" grep -qx "sign $first" "$FAKE_KEYCHAIN/log"

FAKE_TRUST=deny run_script
check "rerun after refused trust creates no new identity" test "$(identity_count)" = 1
check "rerun keeps the same certificate" test "$(only_identity)" = "$first"
check "rerun imports nothing" test "$(log_count import)" = 1
check "rerun asks to trust the existing certificate again" \
  test "$(grep -c "^trust $first$" "$FAKE_KEYCHAIN/log")" = 2
check "rerun signs with the same hash" test "$(grep -c "^sign $first$" "$FAKE_KEYCHAIN/log")" = 2

run_script
check "trust accepted on a later run" test -e "$FAKE_KEYCHAIN/ids/$first/trusted"
check "still the same single identity" test "$(only_identity)" = "$first"

# SIGN_IDENTITY naming an existing but untrusted identity is found.
new_keychain named
FAKE_TRUST=deny run_script
first="$(only_identity)"
FAKE_TRUST=deny SIGN_IDENTITY="Trigo Whispr Dev" run_script || true
check "SIGN_IDENTITY finds an untrusted identity" test "$(grep -c "^sign $first$" "$FAKE_KEYCHAIN/log")" = 2

# Two identities with the same name: refuse instead of signing ambiguously.
new_keychain duplicate
FAKE_TRUST=deny run_script
FAKE_TRUST=deny PATH="$FAKE_BIN:$PATH" bash -c '
  d="$(mktemp -d)"
  openssl req -out "$d/c.pem" -subj "/CN=Trigo Whispr Dev"
  security import "$d/c.pem"
  rm -rf "$d"'
signs_before="$(log_count sign)"
status=0
run_script || status=$?
check "homonymous identities are rejected" test "$status" -ne 0
check "nothing is signed when ambiguous" test "$(log_count sign)" = "$signs_before"
check "the error lists the hashes" grep -q "Several code-signing identities" "$ROOT/out"

if ((failures)); then
  echo "$failures test(s) failed"
  exit 1
fi
echo "all macos-selfsign tests passed"
