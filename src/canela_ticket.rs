//! CanelaRemote: tickets de conexión firmados por nuestra API.
//!
//! La key del hbbs viaja en el custom.txt público, así que un RustDesk cualquiera puede usar
//! nuestro servidor. Para que solo entre CanelaRemote con un técnico autenticado:
//!
//! * Lado técnico: antes de conectar pide a la API (`POST /api/canela/ticket`, con su token de
//!   sesión) un ticket para ese equipo y lo manda dentro del LoginRequest (campo 100, que el
//!   protocolo de RustDesk no conoce y conserva como "unknown field").
//! * Lado equipo: si su custom.txt trae `canela-ticket-pk`, rechaza todo LoginRequest sin un
//!   ticket válido: firmado con esa llave, para este ID, desde el ID que se conecta y vigente.
//!
//! Ticket = base64(firma ed25519 de 64 bytes ‖ JSON {"v":1,"to","from","u","iat","exp"}), el
//! mismo formato "combined" que usa custom.txt. El vencimiento se mide con la hora de la API que
//! llega en cada latido (parche 28), no con el reloj del equipo; el rechazo dice el motivo exacto.
//!
//! Clave fija: si el equipo tiene una guardada en el servidor, la API la manda junto al ticket
//! (`password`) y `client.rs::handle_hash` la usa sin preguntar (parche 15). Vive solo en memoria
//! y se entrega una vez; si está vieja, el equipo la rechaza y la app pide la clave como siempre.

use hbb_common::{
    config::{Config, LocalConfig},
    log,
    message_proto::LoginRequest,
    protobuf::UnknownValueRef,
    sodiumoxide::crypto::sign,
};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicI64, Ordering},
        Mutex,
    },
};

/// Número de campo del ticket dentro de LoginRequest (fuera del rango que usa RustDesk).
pub const FIELD: u32 = 100;
/// Opción (override-settings del custom.txt) con la llave pública de la API, en base64.
pub const OPTION_PK: &str = "canela-ticket-pk";
/// Tolerancia de reloj entre la API y el equipo.
const SKEW_SECS: i64 = 300;
/// Vida del ticket en la API (ticket.controller.ts): para tickets viejos sin `iat`.
const TTL_SECS: i64 = 600;

/// Lado equipo: diferencia hora de la API − reloj del equipo, del último latido (parche 28).
static SERVER_OFFSET: AtomicI64 = AtomicI64::new(0);
static SERVER_TIME_KNOWN: AtomicBool = AtomicBool::new(false);

lazy_static::lazy_static! {
    static ref TICKETS: Mutex<HashMap<String, String>> = Default::default();
    static ref PASSWORDS: Mutex<HashMap<String, String>> = Default::default();
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// Lado equipo: hora de la API que llega en cada latido (`canela.now`). Los tickets se validan
/// con ella y no con el reloj del equipo: una PC con la zona horaria mal (p. ej. UTC-05:00 con la
/// hora corrida a mano) tiene la hora UTC adelantada y veía vencido todo ticket.
pub fn set_server_time(server_now: i64) {
    if server_now <= 0 {
        return;
    }
    let offset = server_now - now();
    let prev = SERVER_OFFSET.swap(offset, Ordering::Relaxed);
    let first = !SERVER_TIME_KNOWN.swap(true, Ordering::Relaxed);
    if offset.abs() > 60 && (first || (prev - offset).abs() > 60) {
        log::warn!("canela: el reloj del equipo difiere {}s de la hora del servidor; se usa la del servidor", -offset);
    }
}

/// Hora para validar tickets: la del servidor si ya llegó un latido, si no la del equipo.
fn trusted_now() -> i64 {
    now() + SERVER_OFFSET.load(Ordering::Relaxed)
}

/// Motivo para el técnico cuando el ticket venció.
fn expired_reason(t: &serde_json::Value, exp: i64) -> String {
    if SERVER_TIME_KNOWN.load(Ordering::Relaxed) {
        return "el ticket venció; vuelve a conectar desde la App Técnico".into();
    }
    // Sin hora del servidor todavía: cuánto va adelantado el reloj del equipo frente a la emisión
    let iat = t.get("iat").and_then(|v| v.as_i64()).unwrap_or(exp - TTL_SECS);
    let mins = (now() - iat) / 60;
    format!(
        "la hora de este equipo va unos {mins} min adelantada; corrige fecha, hora y zona horaria \
         del equipo (República Dominicana = UTC-04:00, hora automática)"
    )
}

/// Lado técnico: pide el ticket para `peer`. Sin sesión iniciada no hace nada (el equipo va a
/// rechazar la conexión y el técnico ve el motivo).
pub async fn fetch(peer: &str) {
    let token = LocalConfig::get_option("access_token");
    if token.is_empty() || peer.is_empty() {
        return;
    }
    let api = crate::get_api_server(
        Config::get_option("api-server"),
        Config::get_option("custom-rendezvous-server"),
    );
    if api.is_empty() {
        return;
    }
    let body = serde_json::json!({ "id": peer, "my_id": Config::get_id() }).to_string();
    let header = format!("Authorization: Bearer {token}");
    match crate::post_request(format!("{api}/api/canela/ticket"), body, &header).await {
        Ok(res) => store(peer, &res),
        Err(e) => log::warn!("canela: no se pudo pedir el ticket: {e}"),
    }
}

/// Guarda lo que contestó la API para `peer`: el ticket y, si viene, la clave fija. Nunca
/// escribe la clave en el log.
pub fn store(peer: &str, res: &str) {
    PASSWORDS.lock().unwrap().remove(peer);
    match serde_json::from_str::<serde_json::Value>(res) {
        Ok(v) => {
            if let Some(t) = v.get("ticket").and_then(|t| t.as_str()) {
                TICKETS.lock().unwrap().insert(peer.to_owned(), t.to_owned());
                if let Some(p) = v.get("password").and_then(|p| p.as_str()).filter(|p| !p.is_empty()) {
                    PASSWORDS.lock().unwrap().insert(peer.to_owned(), p.to_owned());
                }
            } else {
                let err = v.get("error").and_then(|e| e.as_str()).unwrap_or("respuesta sin ticket");
                log::warn!("canela: sin ticket para {peer}: {err}");
            }
        }
        Err(e) => log::warn!("canela: respuesta de ticket inválida: {e}"),
    }
}

/// Lado técnico: la clave fija que mandó la API para `peer` (una sola vez).
pub fn take_password(peer: &str) -> Option<String> {
    let mut m = PASSWORDS.lock().unwrap();
    m.remove(peer).or_else(|| peer.split('@').next().and_then(|id| m.remove(id)))
}

/// Lado técnico: mete el ticket (si hay) en el LoginRequest.
pub fn attach(lr: &mut LoginRequest, peer: &str) {
    if let Some(t) = TICKETS.lock().unwrap().get(peer) {
        lr.special_fields
            .mut_unknown_fields()
            .add_length_delimited(FIELD, t.as_bytes().to_vec());
    }
}

/// Lado equipo: Ok si la conexión trae un ticket válido o si este equipo no exige tickets.
/// El Err es el motivo que ve el técnico ("Conexión no autorizada: <motivo>"): dice qué arreglar.
pub fn verify(lr: &LoginRequest) -> Result<(), String> {
    let pk_b64 = Config::get_option(OPTION_PK);
    if pk_b64.is_empty() {
        return Ok(());
    }
    let pk = crate::decode64(&pk_b64)
        .ok()
        .and_then(|b| sign::PublicKey::from_slice(&b))
        .ok_or("este equipo tiene dañada la llave de tickets; reinstala CanelaRemote en el equipo")?;
    let raw = match lr.special_fields.unknown_fields().get(FIELD) {
        Some(UnknownValueRef::LengthDelimited(b)) => b.to_vec(),
        _ => return Err("usa CanelaRemote técnico con tu sesión iniciada".into()),
    };
    let signed = crate::decode64(&raw).map_err(|_| "el ticket llegó dañado; vuelve a conectar")?;
    let payload = sign::verify(&signed, &pk).map_err(|_| {
        "el ticket no lo firmó el servidor de este equipo; reinstala CanelaRemote en el equipo \
         con el instalador actual"
    })?;
    let t: serde_json::Value =
        serde_json::from_slice(&payload).map_err(|_| "el ticket llegó dañado; vuelve a conectar")?;
    let s = |k: &str| t.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_owned();
    let me = Config::get_id();
    if s("to") != me {
        return Err(format!(
            "el ticket es para el equipo {} y este equipo es {me}; vuelve a conectar por el ID {me}",
            s("to")
        ));
    }
    // my_id puede venir como "id@servidor" si el técnico usa otro servidor: no se acepta
    if s("from") != lr.my_id {
        return Err(format!(
            "el ticket se pidió para el ID {} y tu app se conecta como {}; cierra y abre la App Técnico",
            s("from"),
            lr.my_id
        ));
    }
    let exp = t.get("exp").and_then(|v| v.as_i64()).unwrap_or_default();
    if exp + SKEW_SECS < trusted_now() {
        return Err(expired_reason(&t, exp));
    }
    log::info!("canela: conexión autorizada de {} ({})", lr.my_id, s("u"));
    Ok(())
}
