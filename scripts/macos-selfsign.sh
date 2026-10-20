#!/usr/bin/env bash
#
# Self-sign a Trigo Whispr build on macOS so meeting system-audio capture
# receives real audio.
#
# Adapted from fly-on-the-wall's scripts/macos-selfsign.sh
# (https://github.com/swarnavspujari/fly-on-the-wall, MIT License,
# Copyright (c) 2026 WizzyWYG). See THIRD_PARTY_LICENSES.md.
#
# WHY
#   The Core Audio process tap used to record the other participants only
#   delivers audio when the app has a STABLE code-signing identity that macOS
#   can attach the "System Audio Recording" consent to. Unsigned or ad-hoc
#   signed builds (`bun run tauri dev`, or `tauri build` with
#   signingIdentity "-") get callbacks full of zeros: the recording "works"
#   but the system track is silent. A self-signed certificate is a free,
#   stable identity, enough to test on your own Mac. It does NOT replace a
#   Developer ID signature + notarization for distribution.
#
# WHAT IT DOES
#   1. Creates a self-signed code-signing identity in the login keychain if
#      none exists ("Trigo Whispr Dev"; use SIGN_IDENTITY to pick another
#      identity, e.g. a real Developer ID).
#   2. Signs the bundle with hardened runtime and src-tauri/Entitlements.plist.
#   3. Verifies the signature and prints the one-time consent step.
#
# USAGE
#   bash scripts/macos-selfsign.sh                    # signs the release bundle
#   bash scripts/macos-selfsign.sh "/Applications/Trigo Whispr.app"
#   SIGN_IDENTITY="Developer ID Application: You (TEAMID)" bash scripts/macos-selfsign.sh
#
set -euo pipefail

if [[ "$(uname)" != "Darwin" ]]; then
  echo "This script only runs on macOS (it uses codesign and security)." >&2
  exit 1
fi

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENTITLEMENTS="$REPO_ROOT/src-tauri/Entitlements.plist"
APP_PATH="${1:-$REPO_ROOT/src-tauri/target/release/bundle/macos/Trigo Whispr.app}"
IDENTITY="${SIGN_IDENTITY:-Trigo Whispr Dev}"
KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"

if [[ ! -d "$APP_PATH" ]]; then
  echo "App bundle not found: $APP_PATH" >&2
  echo "Build it first (bun run tauri build) or pass its path, e.g.:" >&2
  echo "  bash scripts/macos-selfsign.sh \"/Applications/Trigo Whispr.app\"" >&2
  exit 1
fi

# --- ensure the signing identity exists -----------------------------------
# SHA-1 hashes of the code-signing identities named (or hashed) "$1". Without
# `-v`, find-identity also lists identities whose certificate is not trusted
# yet, so an identity imported by an earlier run is never created twice.
identity_hashes() {
  security find-identity ${2:+"$2"} -p codesigning |
    sed -nE 's/^ *[0-9]+\) ([0-9A-F]{40}) "(.*)".*$/\1 \2/p' |
    awk -v id="$1" '{ h = $1; sub(/^[^ ]+ /, ""); if ($0 == id || h == id) print h }' |
    sort -u
}

is_trusted() {
  identity_hashes "$1" -v | grep -qx "$1"
}

# PEM of the certificate whose SHA-1 hash is "$1", for add-trusted-cert.
certificate_pem() {
  security find-certificate -a -Z -p |
    awk -v h="$1" '
      /^SHA-1 hash:/ { keep = ($3 == h); next }
      keep && /^-----BEGIN CERTIFICATE-----/ { pem = 1 }
      pem { print }
      /^-----END CERTIFICATE-----/ { pem = 0; keep = 0 }'
}

# Trust the certificate for code signing so `codesign --verify` accepts it.
# macOS asks for your password; without it signing still works for capture,
# macOS only shows the usual Gatekeeper prompt on first launch.
trust_certificate() {
  if ! security add-trusted-cert -r trustRoot -p codeSign -k "$KEYCHAIN" "$1"; then
    echo "  (could not trust the certificate — continuing; capture still works," >&2
    echo "   expect the Gatekeeper \"unknown developer\" prompt on first launch." >&2
    echo "   Re-run this script to try again: the same identity is reused.)" >&2
  fi
}

workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT

hashes="$(identity_hashes "$IDENTITY")"
count="$(printf '%s' "$hashes" | grep -c . || true)"

if (( count > 1 )); then
  echo "Several code-signing identities are named \"$IDENTITY\":" >&2
  printf '  %s\n' $hashes >&2
  echo "Keep one: delete the others with \`security delete-identity -Z <hash>\`," >&2
  echo "or pick one with SIGN_IDENTITY=<hash>." >&2
  exit 1
elif (( count == 1 )); then
  SIGN_HASH="$hashes"
  echo "Using existing signing identity: $IDENTITY ($SIGN_HASH)"
  if ! is_trusted "$SIGN_HASH"; then
    echo "Its certificate is not trusted for code signing yet; trusting it."
    certificate_pem "$SIGN_HASH" > "$workdir/cert.pem"
    if [[ -s "$workdir/cert.pem" ]]; then
      trust_certificate "$workdir/cert.pem"
    else
      echo "  (could not export the certificate to trust it — continuing.)" >&2
    fi
  fi
elif [[ -n "${SIGN_IDENTITY:-}" ]]; then
  echo "Signing identity not found in the keychain: $IDENTITY" >&2
  echo "Unset SIGN_IDENTITY to create the self-signed \"Trigo Whispr Dev\" identity." >&2
  exit 1
else
  echo "Creating a self-signed code-signing identity: $IDENTITY"
  # digitalSignature + codeSigning EKU is what codesign and TCC require.
  # Self-signed means it is its own root, so the identity stays stable.
  openssl req -x509 -newkey rsa:2048 -sha256 -days 3650 -nodes \
    -keyout "$workdir/key.pem" -out "$workdir/cert.pem" \
    -subj "/CN=$IDENTITY" \
    -addext "keyUsage=critical,digitalSignature" \
    -addext "extendedKeyUsage=critical,codeSigning" >/dev/null 2>&1
  # macOS `security` cannot read the default PKCS#12 encryption of OpenSSL 3
  # (Homebrew); `-legacy` restores it. LibreSSL (/usr/bin/openssl) has no
  # such flag and already writes the legacy format.
  p12_pass="trigo-$RANDOM$RANDOM"
  if ! openssl pkcs12 -export -legacy -inkey "$workdir/key.pem" -in "$workdir/cert.pem" \
      -out "$workdir/id.p12" -passout "pass:$p12_pass" >/dev/null 2>&1; then
    openssl pkcs12 -export -inkey "$workdir/key.pem" -in "$workdir/cert.pem" \
      -out "$workdir/id.p12" -passout "pass:$p12_pass" >/dev/null
  fi
  # Let codesign use the key without a keychain prompt.
  security import "$workdir/id.p12" -k "$KEYCHAIN" -P "$p12_pass" \
    -T /usr/bin/codesign >/dev/null
  trust_certificate "$workdir/cert.pem"
  SIGN_HASH="$(identity_hashes "$IDENTITY")"
  if [[ ! "$SIGN_HASH" =~ ^[0-9A-F]{40}$ ]]; then
    echo "The imported identity \"$IDENTITY\" was not found in the keychain." >&2
    exit 1
  fi
fi

# --- sign ------------------------------------------------------------------
echo "Signing: $APP_PATH"
echo "     as: $IDENTITY ($SIGN_HASH)"
# --force replaces the ad-hoc signature of the release build; --deep also
# signs the bundled libraries; --options runtime enables the hardened runtime.
codesign --force --deep --options runtime \
  --entitlements "$ENTITLEMENTS" --sign "$SIGN_HASH" "$APP_PATH"
codesign --verify --deep --strict --verbose=2 "$APP_PATH"

cat <<EOF

Signed. To record the other participants of a meeting:
  1. Launch the app (right-click → Open the first time if Gatekeeper warns).
  2. Start a meeting recording; macOS asks to allow System Audio Recording —
     Allow. You can change it later in System Settings → Privacy & Security →
     Screen & System Audio Recording → System Audio Recording Only.
  3. The meeting pill warns when the system track stays silent.

To be asked again: tccutil reset AudioCapture com.trigo.trigo-whispr
Re-run this script after every rebuild: a new build drops the signature.
EOF
