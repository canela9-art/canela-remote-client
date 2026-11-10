#!/usr/bin/env bash
# Convierte el .deb de RustDesk (Flutter) en CanelaRemote con la configuración adentro.
#   canela/rebrand-linux.sh <rustdesk.deb> <custom.txt> <salida.deb>
#   Opcionales (variables de entorno):
#     APP_NAME=CanelaRemote          tiene que ser el app-name del custom.txt (se verifica)
#     DISPLAY_NAME="CanelaRemote"    nombre en el menú de aplicaciones (técnico: "CanelaRemote Técnico")
#     ICON=<png 256x256>             ícono de la variante (si no, el del .deb)
#     VERSION=1.4.9.29               versión del paquete (si no, la del .deb)
#     PACKAGE=canelaremote           nombre del paquete (técnico: canelaremote-tecnico)
#     CONFLICTS=canelaremote-tecnico la otra variante: se excluyen y una reemplaza a la otra
#                                    (mismas rutas y servicio; sin esto apt no cambia de variante)
#     COMPANY, MAINTAINER, HOMEPAGE, SOURCE_URL, SUMMARY
# Corre en Linux (dpkg-deb, sed de GNU): en el job canela-linux o en un contenedor Debian.
#
# Por qué no es cosmético: en Linux RustDesk arma con el nombre de la app (custom.txt) en
# minúsculas el servicio systemd ({app}.service), /etc/{app}/, /etc/pam.d/{app}, ~/.config/{app}
# y los procesos que busca ("{app} --server"). Con custom.txt diciendo CanelaRemote dentro del
# paquete rustdesk, el servicio no arranca. Aquí todo pasa a {app}: paquete, ejecutable,
# librería, servicio, lanzadores, íconos y rutas de /etc. La línea legal AGPL va en
# /usr/share/doc/{app}/copyright (y en Acerca de, que ya la trae).
# El runner tiene que traer el parche 34 (abre lib<ejecutable>.so): con uno viejo, aborta.
set -euo pipefail
IN="${1:?.deb de entrada}"; CUSTOM="${2:?custom.txt}"; OUT="${3:?.deb de salida}"
APP_NAME="${APP_NAME:-CanelaRemote}"
DISPLAY_NAME="${DISPLAY_NAME:-$APP_NAME}"
COMPANY="${COMPANY:-Canela Soluciones SRL}"
MAINTAINER="${MAINTAINER:-$COMPANY <info@canelasoluciones.com>}"
HOMEPAGE="${HOMEPAGE:-https://canelasoluciones.com}"
SOURCE_URL="${SOURCE_URL:-https://github.com/canela9-art/canela-remote-client}"
SUMMARY="${SUMMARY:-soporte remoto de $COMPANY}"
OLD=rustdesk
SLUG="$(printf '%s' "$APP_NAME" | tr '[:upper:]' '[:lower:]')"
PACKAGE="${PACKAGE:-$SLUG}"
CONFLICTS="${CONFLICTS:-}"

fail() { echo "✗ $*" >&2; exit 1; }
# El servicio, /etc/{app} y los pkill usan el nombre en minúsculas TAL CUAL: sin espacios ni acentos.
[[ "$SLUG" =~ ^[a-z0-9][a-z0-9.+-]+$ ]] || fail "APP_NAME '$APP_NAME' no sirve en Linux (solo letras, números, . + -)"
[[ "$PACKAGE" =~ ^[a-z0-9][a-z0-9.+-]+$ ]] || fail "PACKAGE '$PACKAGE' no es un nombre de paquete Debian"
CUSTOM_APP="$(python3 -c '
import base64, json, sys
d = open(sys.argv[1]).read().strip()
b = base64.b64decode(d + "=" * (-len(d) % 4))
print(json.loads(b[64:]).get("app-name", ""))' "$CUSTOM")" || fail "no se pudo leer $CUSTOM"
[ "$CUSTOM_APP" = "$APP_NAME" ] || fail "el custom.txt dice app-name '$CUSTOM_APP', no '$APP_NAME'"

W="$(mktemp -d)"; trap 'rm -rf "$W"' EXIT
P="$W/pkg"
dpkg-deb -R "$IN" "$P"
D="$P/usr/share/$SLUG"
[ -x "$P/usr/share/$OLD/$OLD" ] || fail "$IN no es el .deb Flutter de RustDesk (falta /usr/share/$OLD/$OLD)"
grep -aq 'Failed to load "%s"' "$P/usr/share/$OLD/$OLD" \
  || fail "el runner de $IN no trae el parche 34 (abre librustdesk.so fijo): recompila el fork"

# 1. programa: /usr/share/{app}/{app}, lib/lib{app}.so, custom.txt al lado del ejecutable
mv "$P/usr/share/$OLD" "$D"
mv "$D/$OLD" "$D/$SLUG"
mv "$D/lib/lib$OLD.so" "$D/lib/lib$SLUG.so"
install -m 644 "$CUSTOM" "$D/custom.txt"

# 2. servicio systemd (postinst lo copia a /usr/lib/systemd/system/{app}.service)
mv "$D/files/systemd/$OLD.service" "$D/files/systemd/$SLUG.service"
sed -i -e "s/RustDesk/$APP_NAME/g" -e "s/$OLD/$SLUG/g" "$D/files/systemd/$SLUG.service"

# 3. /etc/{app}/ (xorg.conf y startwm.sh de la sesión sin monitor) y /etc/pam.d/{app}
for d in "$P/etc/$OLD" "$P/etc/X11/$OLD"; do
  if [ -d "$d" ]; then mv "$d" "${d%/$OLD}/$SLUG"; fi
done
if [ -f "$P/etc/pam.d/$OLD" ]; then mv "$P/etc/pam.d/$OLD" "$P/etc/pam.d/$SLUG"; fi

# 4. íconos (el runner busca en el tema el ícono llamado como el ejecutable)
I="$P/usr/share/icons/hicolor"
mv "$I/256x256/apps/$OLD.png" "$I/256x256/apps/$SLUG.png"
mv "$I/scalable/apps/$OLD.svg" "$I/scalable/apps/$SLUG.svg"
if [ -n "${ICON:-}" ]; then
  install -m 644 "$ICON" "$I/256x256/apps/$SLUG.png"
  rm -f "$I/scalable/apps/$SLUG.svg"   # el SVG es el del cliente
fi

# 5. lanzadores: menú y esquema {app}:// (get_uri_prefix() = nombre de la app en minúsculas)
A="$P/usr/share/applications"
rm -f "$A/$OLD.desktop" "$A/$OLD-link.desktop"
cat > "$A/$SLUG.desktop" <<EOF
[Desktop Entry]
Name=$DISPLAY_NAME
GenericName=Soporte remoto
Comment=$APP_NAME: $SUMMARY
Exec=$SLUG %u
Icon=$SLUG
Terminal=false
Type=Application
StartupNotify=true
Categories=Network;RemoteAccess;GTK;
Keywords=soporte;remoto;escritorio remoto;remote desktop;
StartupWMClass=$SLUG
EOF
cat > "$A/$SLUG-link.desktop" <<EOF
[Desktop Entry]
Name=$DISPLAY_NAME
NoDisplay=true
MimeType=x-scheme-handler/$SLUG;
TryExec=$SLUG
Exec=$SLUG %u
Icon=$SLUG
Terminal=false
Type=Application
StartupNotify=false
StartupWMClass=$SLUG
EOF

# 6. aviso legal (AGPL: crédito a RustDesk y dónde está el código fuente)
mkdir -p "$P/usr/share/doc/$SLUG"
cat > "$P/usr/share/doc/$SLUG/copyright" <<EOF
$APP_NAME — $SUMMARY
Copyright © $(date +%Y) $COMPANY. $HOMEPAGE

Basado en RustDesk © Purslane Tech Pte. Ltd. · AGPL-3.0
Licencia: GNU Affero General Public License v3 (https://www.gnu.org/licenses/agpl-3.0.html)
Código fuente: $SOURCE_URL
EOF
chmod 644 "$P/usr/share/doc/$SLUG/copyright"

# 7. control y scripts del paquete. Upstream decide si hay systemd por el nombre de /proc/1/exe;
# con un PID 1 envuelto (emulación, algunas VMs) no instala ni arranca el servicio. Primero se
# mira /run/systemd/system (la prueba estándar de sd_booted) y si no, lo de upstream.
for s in preinst postinst prerm postrm; do
  if [ -f "$P/DEBIAN/$s" ]; then
    sed -i -e "s/RustDesk/$APP_NAME/g" -e "s/$OLD/$SLUG/g" \
      -e 's#INITSYS=$(ls -al /proc/1/exe#INITSYS=$([ -d /run/systemd/system ] \&\& echo systemd || ls -al /proc/1/exe#' \
      "$P/DEBIAN/$s"
  fi
done
grep -q 'run/systemd/system' "$P/DEBIAN/postinst" || fail "el postinst de upstream cambió: revisa la detección de systemd"
field() { dpkg-deb -f "$IN" "$1"; }
VER="${VERSION:-$(field Version)}"
{
  echo "Package: $PACKAGE"
  echo "Version: $VER"
  echo "Architecture: $(field Architecture)"
  echo "Maintainer: $MAINTAINER"
  echo "Installed-Size: $(du -sk --exclude=DEBIAN "$P" | cut -f1)"
  echo "Homepage: $HOMEPAGE"
  echo "Section: net"
  echo "Priority: optional"
  # Upstream no declara dos cosas que en un Debian mínimo (kiosco con Xorg + Openbox + Chromium)
  # no vienen:
  #  · sudo: el servicio arranca el --server en la sesión X del usuario con `sudo -u` (run_as_user);
  #    sin él no hay captura.
  #  · libegl1: el motor de Flutter abre libEGL.so.1; sin él la app y la ventana de aceptar la
  #    conexión abortan ("Couldn't open libEGL.so.1"), probado en Debian 13.
  DEPENDS="$(field Depends)"
  for dep in sudo libegl1; do
    case ", $DEPENDS," in *", $dep,"*) ;; *) DEPENDS="$DEPENDS, $dep";; esac
  done
  echo "Depends: $DEPENDS"
  if [ -n "$(field Recommends)" ]; then echo "Recommends: $(field Recommends)"; fi
  if [ -n "$CONFLICTS" ]; then echo "Conflicts: $CONFLICTS"; echo "Replaces: $CONFLICTS"; fi
  echo "Description: $DISPLAY_NAME - $SUMMARY"
  echo " $APP_NAME queda conectado al servidor de $COMPANY para recibir soporte"
  echo " remoto. La configuración viene dentro del paquete."
} > "$P/DEBIAN/control"
(cd "$P" && find . -path ./DEBIAN -prune -o -type f -printf '%P\0' | sort -z | xargs -0 md5sum) > "$P/DEBIAN/md5sums"

# 8. nada visible puede decir rustdesk (la línea legal de copyright es la excepción)
LEFT="$(cd "$P" && find . | grep -i "$OLD" || true)"
[ -z "$LEFT" ] || fail "quedan rutas con '$OLD':"$'\n'"$LEFT"
TEXTS=("$P/DEBIAN/control" "$A"/*.desktop "$D/files/systemd/$SLUG.service")
for s in preinst postinst prerm postrm; do
  if [ -f "$P/DEBIAN/$s" ]; then TEXTS+=("$P/DEBIAN/$s"); fi
done
if grep -il "$OLD" "${TEXTS[@]}"; then fail "esos archivos todavía dicen '$OLD'"; fi

mkdir -p "$(dirname "$OUT")"
dpkg-deb --root-owner-group --build "$P" "$OUT" >/dev/null
echo "✓ $OUT ($PACKAGE $VER, $(dpkg-deb -f "$OUT" Architecture))"
