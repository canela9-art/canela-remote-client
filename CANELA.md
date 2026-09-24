# CanelaRemote client

Fork de [RustDesk](https://github.com/rustdesk/rustdesk) (AGPL-3.0) usado por PixelCanela
como cliente de soporte remoto **CanelaRemote**. Todo el crédito del software base es de los
autores de RustDesk; este repositorio existe para cumplir la AGPL publicando nuestros cambios.

Cambios respecto a upstream (rama `canela`):

1. `src/common.rs` — la clave pública que verifica `custom.txt` es la de PixelCanela, así que
   solo aceptamos configuraciones firmadas por nosotros.
2. `src/hbbs_http/sync.rs` — la respuesta del heartbeat puede traer
   `{"canela":{"set_password":"…"}}` para rotar la contraseña del equipo (contraseña de un
   solo uso por sesión de soporte).
3. Enlace "Acerca de" y descripción del paquete apuntan a este repositorio.
4. `.github/workflows/canela-build.yml` — compilación manual reutilizando `flutter-build.yml`.

El nombre, servidor, llave y políticas no están en el código: los pone `custom.txt`.
