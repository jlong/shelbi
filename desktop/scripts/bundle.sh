#!/usr/bin/env bash
# Build Shelbi.app for distribution: release build, bundle, sign, notarize,
# staple, zip, sha256. macOS on Apple silicon (or any Mac with the arm64
# target installed); run from anywhere.
#
#   desktop/scripts/bundle.sh                 # build + bundle + sign/notarize when possible
#   desktop/scripts/bundle.sh --no-build      # reuse the last release build
#   desktop/scripts/bundle.sh --out DIR       # default: desktop/target/bundle
#
# What it produces, in the output directory:
#
#   Shelbi.app
#   Shelbi-<version>-arm64.zip           (ditto -c -k --keepParent, the stapled app)
#   Shelbi-<version>-arm64.zip.sha256    ("<sha256>  <zip name>", sha256sum format)
#   bundle-result.env                    (version=, zip=, signed=, notarized= for CI)
#
# Signing follows the 32pixels pattern: Developer ID, inside-out (nested code
# first, then the bundle; never --deep), hardened runtime, secure timestamp.
# Each step runs only when its credentials are present, and the script says
# which build it produced:
#
#   signing       SIGN_IDENTITY (default "Developer ID Application: 32pixels,
#                 LLC (6L5M94R7K7)") must be a valid codesigning identity in
#                 the keychain search list (or in $KEYCHAIN). Absent: the app
#                 is ad-hoc signed so it still runs locally, and the build is
#                 reported UNSIGNED.
#   notarization  either NOTARY_PROFILE (a `notarytool store-credentials`
#                 keychain profile), or an App Store Connect API key:
#                 NOTARY_KEY_ID + NOTARY_ISSUER_ID + the .p8 as a path in
#                 NOTARY_KEY_PATH or base64 in NOTARY_KEY_P8. Absent (or the
#                 build is unsigned): signed but not notarized, and said so.
#
# Present-but-broken credentials fail the script: a release never silently
# ships un-notarized because a secret was mistyped. REQUIRE_NOTARIZATION=1
# (the release workflow sets it once its secrets are all present) also fails
# a build that ends up anything less than signed, notarized, and stapled.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)" # desktop/
out="$here/target/bundle"
build=1
target="aarch64-apple-darwin"
identity="${SIGN_IDENTITY:-Developer ID Application: 32pixels, LLC (6L5M94R7K7)}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) out="$2"; shift 2 ;;
    --no-build) build=0; shift ;;
    -h|--help) sed -n '2,35p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "bundle.sh: unknown argument: $1" >&2; exit 2 ;;
  esac
done

# Loud in a terminal and as an annotation in GitHub Actions.
warn() {
  if [[ -n "${GITHUB_ACTIONS:-}" ]]; then echo "::warning::$*"; fi
  printf '\n\033[1;33mwarning:\033[0m %s\n\n' "$*" >&2
}
fail() {
  if [[ -n "${GITHUB_ACTIONS:-}" ]]; then echo "::error::$*"; fi
  printf '\033[1;31merror:\033[0m %s\n' "$*" >&2
  exit 1
}
step() { printf '\n==> %s\n' "$*"; }

[[ "$(uname -s)" == "Darwin" ]] || fail "bundle.sh builds a macOS app; run it on a Mac"

version="$(
  cargo metadata --no-deps --format-version 1 --manifest-path "$here/Cargo.toml" |
    /usr/bin/python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="shelbi-desktop"))'
)"
[[ -n "$version" ]] || fail "could not read the version from desktop/Cargo.toml"
zip_name="Shelbi-${version}-arm64.zip"
app="$out/Shelbi.app"
echo "Shelbi.app $version -> $out"

# ---- build ------------------------------------------------------------------
binary="$here/target/$target/release/shelbi-desktop"
if [[ "$build" == 1 ]]; then
  step "cargo build --release --target $target"
  (cd "$here" && cargo build --release --locked --target "$target")
fi
[[ -x "$binary" ]] || fail "no release binary at $binary (drop --no-build)"

# ---- bundle -----------------------------------------------------------------
step "assembling Shelbi.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/Shelbi"
sed "s/@VERSION@/$version/g" "$here/macos/Shelbi.app.plist" >"$app/Contents/Info.plist"
plutil -lint -s "$app/Contents/Info.plist"
printf 'APPL????' >"$app/Contents/PkgInfo"

# The icon: the committed 1024px master (assets/icon/shelbi-1024.png, the
# finished raster artwork and the icon's only source; see assets/icon/README.md)
# cut into an iconset, then compiled.
master="$here/assets/icon/shelbi-1024.png"
master_info="$(sips -g pixelWidth -g pixelHeight -g hasAlpha "$master")"
grep -q 'pixelWidth: 1024$' <<<"$master_info" &&
  grep -q 'pixelHeight: 1024$' <<<"$master_info" &&
  grep -q 'hasAlpha: yes$' <<<"$master_info" ||
  fail "$master must be a 1024x1024 PNG with alpha"
iconset="$(mktemp -d)/Shelbi.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
  sips -s format png -z "$size" "$size" "$master" --out "$iconset/icon_${size}x${size}.png" >/dev/null
  double=$((size * 2))
  sips -s format png -z "$double" "$double" "$master" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/Shelbi.icns"
rm -rf "$(dirname "$iconset")"

# ---- sign -------------------------------------------------------------------
keychain_args=()
if [[ -n "${KEYCHAIN:-}" ]]; then keychain_args=(--keychain "$KEYCHAIN"); fi

has_identity() {
  security find-identity -v -p codesigning ${KEYCHAIN:+"$KEYCHAIN"} 2>/dev/null |
    grep -qF "\"$identity\""
}

# Every Mach-O inside the bundle other than the main executable, deepest
# first, so nested code is sealed before whatever contains it. Today there is
# none (one statically linked binary), but a helper or framework added later
# must not be skipped or force a `--deep`.
nested_code() {
  find "$app/Contents" -type f ! -path "$app/Contents/MacOS/Shelbi" -print0 |
    while IFS= read -r -d '' f; do
      if file -b "$f" | grep -q 'Mach-O'; then
        printf '%s\t%s\n' "$(awk -F/ '{print NF}' <<<"$f")" "$f"
      fi
    done | sort -rn | cut -f2-
  find "$app/Contents" -depth -type d \( -name '*.framework' -o -name '*.app' -o -name '*.xpc' -o -name '*.appex' \) ! -path "$app"
}

signed="unsigned"
if has_identity; then
  step "signing with $identity"
  sign() {
    codesign --force --options runtime --timestamp --sign "$identity" \
      ${keychain_args[@]+"${keychain_args[@]}"} "$1"
  }
  while IFS= read -r nested; do
    [[ -n "$nested" ]] && sign "$nested"
  done < <(nested_code)
  sign "$app"
  codesign --verify --strict --verbose=2 "$app"
  signed="developer-id"
else
  warn "UNSIGNED BUILD: no \"$identity\" codesigning identity found${KEYCHAIN:+ in $KEYCHAIN}. Ad-hoc signing so the app runs on this Mac only; Gatekeeper will refuse it anywhere else."
  codesign --force --sign - "$app"
  signed="adhoc"
fi

# ---- notarize + staple ------------------------------------------------------
notary_args=()
key_file=""
# Always succeeds: under `set -e` an EXIT trap's failing last command becomes
# the script's exit status.
cleanup() { if [[ -n "$key_file" ]]; then rm -f "$key_file"; fi; }
trap cleanup EXIT
if [[ -n "${NOTARY_PROFILE:-}" ]]; then
  notary_args=(--keychain-profile "$NOTARY_PROFILE")
  [[ -n "${KEYCHAIN:-}" ]] && notary_args+=(--keychain "$KEYCHAIN")
elif [[ -n "${NOTARY_KEY_ID:-}" && -n "${NOTARY_ISSUER_ID:-}" && (-n "${NOTARY_KEY_PATH:-}" || -n "${NOTARY_KEY_P8:-}") ]]; then
  if [[ -n "${NOTARY_KEY_PATH:-}" ]]; then
    key_file="" # the caller's file: never deleted
    key_path="${NOTARY_KEY_PATH/#\~/$HOME}"
  else
    key_file="$(mktemp -t notary-key).p8"
    printf '%s' "$NOTARY_KEY_P8" | base64 --decode >"$key_file" ||
      fail "NOTARY_KEY_P8 is not valid base64"
    key_path="$key_file"
  fi
  [[ -s "$key_path" ]] || fail "notary API key not found or empty: $key_path"
  notary_args=(--key "$key_path" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER_ID")
fi

notarized="false"
if [[ ${#notary_args[@]} -eq 0 ]]; then
  if [[ "$signed" == "developer-id" ]]; then
    warn "NOT NOTARIZED: no notary credentials (NOTARY_PROFILE, or NOTARY_KEY_ID + NOTARY_ISSUER_ID + NOTARY_KEY_PATH/NOTARY_KEY_P8). The app is signed but Gatekeeper will block it on other Macs until it is notarized."
  fi
elif [[ "$signed" != "developer-id" ]]; then
  warn "NOT NOTARIZED: notary credentials are set but the app isn't Developer ID signed, and Apple only notarizes signed apps."
else
  step "notarizing (notarytool submit --wait)"
  submission="$(mktemp -d)/Shelbi.zip"
  ditto -c -k --keepParent "$app" "$submission"
  result="$(xcrun notarytool submit "$submission" "${notary_args[@]}" --wait --output-format json)" ||
    fail "notarytool submit failed: $result"
  rm -rf "$(dirname "$submission")"
  echo "$result"
  status="$(/usr/bin/python3 -c 'import json,sys; print(json.loads(sys.argv[1]).get("status",""))' "$result")"
  if [[ "$status" != "Accepted" ]]; then
    id="$(/usr/bin/python3 -c 'import json,sys; print(json.loads(sys.argv[1]).get("id",""))' "$result")"
    [[ -n "$id" ]] && xcrun notarytool log "$id" "${notary_args[@]}" || true
    fail "notarization finished with status '$status'"
  fi
  step "stapling"
  xcrun stapler staple "$app"
  xcrun stapler validate "$app"
  spctl --assess --type exec --verbose=2 "$app"
  notarized="true"
fi

if [[ "${REQUIRE_NOTARIZATION:-}" == 1 && "$notarized" != true ]]; then
  fail "REQUIRE_NOTARIZATION=1 but the build is $signed and not notarized (is SIGN_IDENTITY \"$identity\" the imported certificate's name?)"
fi

# ---- smoke ------------------------------------------------------------------
# The bundled binary starts and knows its version (`--print-version` exits
# before any window; its CLI check may fail on a build machine, that's fine).
first_line="$("$app/Contents/MacOS/Shelbi" --print-version 2>/dev/null | head -1 || true)"
[[ "$first_line" == "Shelbi $version" ]] ||
  fail "the bundled app printed '$first_line', expected 'Shelbi $version'"

# ---- zip + sha256 -----------------------------------------------------------
step "zipping"
rm -f "$out/$zip_name" "$out/$zip_name.sha256"
ditto -c -k --keepParent "$app" "$out/$zip_name"
(cd "$out" && shasum -a 256 "$zip_name" >"$zip_name.sha256")
sha="$(cut -d' ' -f1 "$out/$zip_name.sha256")"

cat >"$out/bundle-result.env" <<EOF
version=$version
zip=$out/$zip_name
sha256=$sha
signed=$signed
notarized=$notarized
EOF
if [[ -n "${GITHUB_OUTPUT:-}" ]]; then cat "$out/bundle-result.env" >>"$GITHUB_OUTPUT"; fi

case "$signed/$notarized" in
  developer-id/true) summary="SIGNED + NOTARIZED + STAPLED (ready to distribute)" ;;
  developer-id/false) summary="SIGNED, NOT NOTARIZED (Gatekeeper blocks it on other Macs)" ;;
  *) summary="UNSIGNED (ad-hoc; runs on this Mac only)" ;;
esac
printf '\n%s\n  %s\n  %s\n  sha256 %s\n' "$summary" "$app" "$out/$zip_name" "$sha"
