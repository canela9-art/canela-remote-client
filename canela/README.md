# canela/ — archivos propios del fork CanelaRemote

- `custom-cliente.txt` — perfil firmado para equipos de clientes (solo recibe, aprobación con clic).
- `custom-tecnico.txt` — perfil firmado para técnicos (entrada y salida, cuenta, libreta).
- `rebrand-macos.sh` — convierte el .dmg de RustDesk en CanelaRemote.app con la config adentro.
- `rebrand-linux.sh` — convierte el .deb de RustDesk (runner con el parche 34) en el paquete
  `canelaremote` (ejecutable, librería, servicio systemd, /etc, pam.d, lanzadores e íconos con ese
  nombre y custom.txt junto al ejecutable). Lo usa el job canela-linux.
- `icon-tecnico-256.png` — ícono del .deb de técnico (lo genera `scripts/make-icons.py`).

Los custom.txt están firmados con la llave de PixelCanela y apuntan a remoto.ufpos.com. No son
secretos (servidor, key pública del hbbs, URL de la API): cualquier instalación los lleva.
Se regeneran desde el repo privado (server/vps-ufpos/build-profiles.sh).
