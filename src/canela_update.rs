//! CanelaRemote: actualización con un clic desde nuestros releases.
//!
//! RustDesk trae el flujo completo (tarjeta "Actualizar" → descarga con progreso → instalación
//! silenciosa con `--update` en Windows o reemplazo del .app en macOS), pero lo apaga en las
//! apps con marca y lo apunta a rustdesk.com. Aquí se reutiliza ese flujo contra nuestra API:
//!
//! * Cada build lleva su número (`CANELA_BUILD` = run_number del workflow "CanelaRemote build").
//! * `check()` pregunta a `POST /api/canela/update` si hay uno más nuevo para esta plataforma,
//!   arquitectura y variante (cliente/técnico); si lo hay, deja la URL "de release" en
//!   `SOFTWARE_UPDATE_URL` y avisa a la UI, que muestra la tarjeta.
//! * La UI arma la descarga como `<url con tag→download>/<download_file()>`; la API responde
//!   con un redirect al asset del release en GitHub.
//! * La versión que se muestra es "1.4.9-<build>" (`get_version_number` suma el sufijo).

use hbb_common::{config::Config, log, tokio, ResultType};

/// Número de build con el que se compiló (0 = build local o de upstream).
pub fn build() -> i64 {
    option_env!("CANELA_BUILD").and_then(|b| b.trim().parse().ok()).unwrap_or(0)
}

/// "cliente" o "técnico": sale del custom.txt (`canela-variant`) o, en perfiles viejos, de
/// si la app solo recibe conexiones.
pub fn variant() -> &'static str {
    match Config::get_option("canela-variant").as_str() {
        "tecnico" => "tecnico",
        "cliente" => "cliente",
        _ if hbb_common::config::is_incoming_only() => "cliente",
        _ => "tecnico",
    }
}

fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        "x86" => "x86",
        other => other,
    }
}

fn platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "android") {
        "android"
    } else {
        "linux"
    }
}

/// Nombre del archivo que pide la UI (la API lo traduce al asset real del release).
pub fn download_file() -> String {
    let suffix = if variant() == "tecnico" { "-tecnico" } else { "" };
    let ext = if cfg!(target_os = "macos") { "dmg" } else { "exe" };
    format!("canelaremote{suffix}-{}.{ext}", arch())
}

/// Pregunta a la API y, si hay versión nueva, publica la URL para la UI.
pub async fn check() -> ResultType<()> {
    let api = crate::get_api_server(
        Config::get_option("api-server"),
        Config::get_option("custom-rendezvous-server"),
    );
    if api.is_empty() {
        return Ok(());
    }
    let body = serde_json::json!({
        "platform": platform(),
        "arch": arch(),
        "variant": variant(),
        "build": build(),
        "version": crate::VERSION,
        "id": Config::get_id(),
    })
    .to_string();
    let res = crate::post_request(format!("{api}/api/canela/update"), body, "").await?;
    let v: serde_json::Value = serde_json::from_str(&res)?;
    let url = v.get("url").and_then(|u| u.as_str()).unwrap_or_default().to_owned();
    if url.is_empty() {
        *crate::common::SOFTWARE_UPDATE_URL.lock().unwrap() = String::new();
        return Ok(());
    }
    log::info!("canela: hay versión nueva: {url}");
    #[cfg(feature = "flutter")]
    {
        let mut m = std::collections::HashMap::new();
        m.insert("name", "check_software_update_finish");
        m.insert("url", url.as_str());
        if let Ok(data) = serde_json::to_string(&m) {
            let _ = crate::flutter::push_global_event(crate::flutter::APP_TYPE_MAIN, data);
        }
    }
    *crate::common::SOFTWARE_UPDATE_URL.lock().unwrap() = url;
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
pub async fn check_blocking() -> ResultType<()> {
    check().await
}
