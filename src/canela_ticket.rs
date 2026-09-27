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
//! Ticket = base64(firma ed25519 de 64 bytes ‖ JSON {"v":1,"to","from","u","exp"}), el mismo
//! formato "combined" que usa custom.txt.
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
use std::{collections::HashMap, sync::Mutex};

/// Número de campo del ticket dentro de LoginRequest (fuera del rango que usa RustDesk).
pub const FIELD: u32 = 100;
/// Opción (override-settings del custom.txt) con la llave pública de la API, en base64.
pub const OPTION_PK: &str = "canela-ticket-pk";
/// Tolerancia de reloj entre la API y el equipo.
const SKEW_SECS: i64 = 300;

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
pub fn verify(lr: &LoginRequest) -> Result<(), String> {
    let pk_b64 = Config::get_option(OPTION_PK);
    if pk_b64.is_empty() {
        return Ok(());
    }
    let pk = crate::decode64(&pk_b64)
        .ok()
        .and_then(|b| sign::PublicKey::from_slice(&b))
        .ok_or("llave de tickets inválida en la configuración")?;
    let raw = match lr.special_fields.unknown_fields().get(FIELD) {
        Some(UnknownValueRef::LengthDelimited(b)) => b.to_vec(),
        _ => return Err("sin ticket (no es CanelaRemote técnico con sesión iniciada)".into()),
    };
    let signed = crate::decode64(&raw).map_err(|_| "ticket mal codificado")?;
    let payload = sign::verify(&signed, &pk).map_err(|_| "firma del ticket inválida")?;
    let t: serde_json::Value =
        serde_json::from_slice(&payload).map_err(|_| "contenido del ticket inválido")?;
    let s = |k: &str| t.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_owned();
    if s("to") != Config::get_id() {
        return Err(format!("ticket para otro equipo ({})", s("to")));
    }
    // my_id puede venir como "id@servidor" si el técnico usa otro servidor: no se acepta
    if s("from") != lr.my_id {
        return Err(format!("ticket de otro técnico ({} ≠ {})", s("from"), lr.my_id));
    }
    let exp = t.get("exp").and_then(|v| v.as_i64()).unwrap_or_default();
    if exp + SKEW_SECS < now() {
        return Err("ticket vencido".into());
    }
    log::info!("canela: conexión autorizada de {} ({})", lr.my_id, s("u"));
    Ok(())
}
