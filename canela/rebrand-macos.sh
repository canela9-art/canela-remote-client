#!/usr/bin/env bash
# Convierte el .dmg de RustDesk en CanelaRemote con la configuración adentro.
#   canela/rebrand-macos.sh <rustdesk.dmg> <custom.txt> <salida.dmg>
#
# Por qué no es cosmético: en macOS RustDesk arma las rutas de su servicio con el nombre
# de la app (/Applications/{APP}.app/Contents/MacOS/{APP}). Con custom.txt diciendo
# "CanelaRemote", el bundle TIENE que llamarse CanelaRemote.app y su ejecutable
# CanelaRemote, o el LaunchAgent/LaunchDaemon apuntan a rutas que no existen.
set -euo pipefail
DMG="$1"; CUSTOM="$2"; OUT="$3"
APP_NAME="${APP_NAME:-CanelaRemote}"
BUNDLE_ID="${BUNDLE_ID:-com.pixelcanela.canelaremote}"
SCHEME="$(echo "$APP_NAME" | tr '[:upper:]' '[:lower:]')"
W="$(mktemp -d)"; MNT="$W/mnt"; SRC="$W/src"; mkdir -p "$MNT" "$SRC"
trap 'hdiutil detach "$MNT" -quiet 2>/dev/null || true; rm -rf "$W"' EXIT

hdiutil attach "$DMG" -nobrowse -readonly -mountpoint "$MNT" -quiet
ORIG="$(find "$MNT" -maxdepth 1 -name '*.app' | head -1)"
[ -n "$ORIG" ] || { echo "no hay .app dentro de $DMG"; exit 1; }
APP="$SRC/$APP_NAME.app"
ditto "$ORIG" "$APP"
hdiutil detach "$MNT" -quiet

PL="$APP/Contents/Info.plist"; PB=/usr/libexec/PlistBuddy
OLD_EXE="$($PB -c 'Print :CFBundleExecutable' "$PL")"
# entitlements originales (si la app venía firmada ad-hoc por Xcode) para conservarlos
ENT="$W/ent.plist"; codesign -d --entitlements :- "$APP" > "$ENT" 2>/dev/null || true

[ "$OLD_EXE" != "$APP_NAME" ] && mv "$APP/Contents/MacOS/$OLD_EXE" "$APP/Contents/MacOS/$APP_NAME"
$PB -c "Set :CFBundleExecutable $APP_NAME" "$PL"
$PB -c "Set :CFBundleName $APP_NAME" "$PL"
$PB -c "Delete :CFBundleDisplayName" "$PL" 2>/dev/null || true
$PB -c "Add :CFBundleDisplayName string $APP_NAME" "$PL"
$PB -c "Set :CFBundleIdentifier $BUNDLE_ID" "$PL"
$PB -c "Set :CFBundleURLTypes:0:CFBundleURLName $BUNDLE_ID" "$PL" 2>/dev/null || true
$PB -c "Set :CFBundleURLTypes:0:CFBundleURLSchemes:0 $SCHEME" "$PL"
cp "$CUSTOM" "$APP/Contents/Resources/custom.txt"

# Sello ad-hoc nuevo, de adentro hacia afuera. OJO: el build sin firmar de RustDesk trae el
# ejecutable ad-hoc CON hardened runtime y los frameworks SIN él → dyld rechaza FlutterMacOS
# ("different Team IDs") y la app no abre ni siquiera la original. `codesign --force` conserva
# esa marca, por eso primero se quita la firma de cada pieza. Firma real (Developer ID) +
# notarización reemplazan esto cuando haya cuenta de Apple Developer.
for piece in "$APP"/Contents/Frameworks/*.framework "$APP"/Contents/Frameworks/*.dylib "$APP"/Contents/MacOS/*; do
  [ -e "$piece" ] || continue
  [ "$piece" = "$APP/Contents/MacOS/$APP_NAME" ] && continue
  codesign --remove-signature "$piece" 2>/dev/null || true
  codesign --force --sign - "$piece"
done
codesign --remove-signature "$APP" 2>/dev/null || true
if [ -s "$ENT" ] && grep -q "<dict>" "$ENT"; then
  codesign --force --sign - --entitlements "$ENT" "$APP"
else
  codesign --force --sign - "$APP"
fi
codesign --verify --deep --strict "$APP"

# Prueba de humo: si la arquitectura coincide con la de esta máquina, la app tiene que arrancar.
BIN="$APP/Contents/MacOS/$APP_NAME"
if lipo -archs "$BIN" 2>/dev/null | grep -qw "$(uname -m)"; then
  V="$(perl -e 'alarm 30; exec @ARGV' "$BIN" --version 2>&1 | tail -1)"
  echo "$V" | grep -qE '^[0-9]+\.[0-9]+' || { echo "✗ la app no arranca: $V"; exit 1; }
  echo "  arranca: $APP_NAME $V"
fi

ln -s /Applications "$SRC/Applications"
rm -f "$OUT"
hdiutil create -volname "$APP_NAME" -srcfolder "$SRC" -format UDZO -quiet "$OUT"
echo "✓ $OUT ($APP_NAME.app, $BUNDLE_ID, esquema $SCHEME://)"
