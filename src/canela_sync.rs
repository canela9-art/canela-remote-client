//! CanelaRemote: sincroniza con la cuenta del técnico (la API de CanelaRemote) lo que RustDesk
//! guarda solo en la computadora, para que sea igual en todas las computadoras donde entra:
//!
//! * **Favoritos** (la ★).
//! * **Recientes con los ajustes de cada equipo**: cada equipo es un archivo `peers/<id>.toml`
//!   (vista, calidad, túneles, resolución, alias del equipo…) y su fecha de modificación es la
//!   que ordena la pestaña Recientes. Se sube y se baja el archivo entero, MENOS lo que es de esta
//!   computadora o secreto: contraseña guardada, credenciales de RDP/sistema, tamaño y posición de
//!   ventanas, carpetas de transferencia. Esos se conservan siempre del archivo local.
//! * **Preferencias de la app**: las de `KEYS_LOCAL_SETTINGS` que no dependen de la máquina
//!   (tema, idioma, pestañas…) y los valores por defecto de las sesiones (`KEYS_DISPLAY_SETTINGS`:
//!   códec, calidad, FPS…). Nunca las opciones de seguridad del equipo (`KEYS_SETTINGS`).
//!
//! Cómo: cada pocos segundos compara una huella del estado local; si cambió (o pasó un minuto)
//! hace UNA llamada `POST /api/canela/sync` que sube lo cambiado y baja lo nuevo.
//! * Favoritos y preferencias: fusión a 3 bandas contra la última versión acordada con el
//!   servidor (se guarda en `canela_sync.json`): lo cambiado aquí se sube; lo cambiado en otra
//!   computadora se baja. La primera vez de un usuario en esta computadora se UNEN (favoritos) o
//!   se completan (preferencias) en vez de pisar lo que ya hay en la nube.
//! * Equipos: gana el más reciente (fecha del archivo). Quitar un equipo de Recientes se propaga.
//!
//! Solo corre con sesión iniciada (token de la cuenta). Se apaga con `canela-sync=N` en custom.txt.

#![allow(dead_code)]
use hbb_common::{
    config::{keys, Config, LocalConfig, PeerConfig, UserDefaultConfig},
    log, tokio, ResultType,
};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Preferencias locales que viajan: las de KEYS_LOCAL_SETTINGS sin lo propio de cada máquina
/// (render por textura/D3D, carpeta de grabaciones, ventana flotante y modo táctil de Android,
/// elevar servicio, la pestaña abierta ahora mismo).
pub const LOCAL_KEYS: &[&str] = &[
    keys::OPTION_THEME,
    keys::OPTION_LANGUAGE,
    keys::OPTION_ENABLE_CONFIRM_CLOSING_TABS,
    keys::OPTION_ENABLE_OPEN_NEW_CONNECTIONS_IN_TABS,
    keys::OPTION_SYNC_AB_WITH_RECENT_SESSIONS,
    keys::OPTION_SYNC_AB_TAGS,
    keys::OPTION_FILTER_AB_BY_INTERSECTION,
    keys::OPTION_REMOTE_MENUBAR_DRAG_LEFT,
    keys::OPTION_REMOTE_MENUBAR_DRAG_RIGHT,
    keys::OPTION_HIDE_AB_TAGS_PANEL,
    keys::OPTION_FLUTTER_REMOTE_MENUBAR_STATE,
    keys::OPTION_FLUTTER_PEER_SORTING,
    keys::OPTION_FLUTTER_PEER_TAB_ORDER,
    keys::OPTION_FLUTTER_PEER_TAB_VISIBLE,
    keys::OPTION_FLUTTER_PEER_CARD_UI_TYLE,
    keys::OPTION_KEEP_AWAKE_DURING_OUTGOING_SESSIONS,
    keys::OPTION_DISABLE_GROUP_PANEL,
    keys::OPTION_DISABLE_DISCOVERY_PANEL,
    keys::OPTION_ALLOW_AUTO_RECORD_OUTGOING,
    keys::OPTION_ENABLE_UDP_PUNCH,
    keys::OPTION_ENABLE_IPV6_PUNCH,
    keys::OPTION_ALLOW_ASK_FOR_NOTE,
];

/// Campos del archivo de cada equipo que son de ESTA computadora: nunca suben, y al bajar la
/// versión de la nube se conservan los locales.
pub const PEER_LOCAL_FIELDS: &[&str] =
    &["password", "size", "size_ft", "size_pf", "ui_flutter", "transfer", "direct_failures"];
/// Opciones secretas dentro de `options` del equipo (van cifradas en el archivo): tampoco suben.
pub const PEER_SECRET_OPTIONS: &[&str] = &["rdp_password", "os-username", "os-password"];

/// Máximo de equipos que se suben (los más recientes); la API tiene el mismo tope por llamada.
pub const MAX_PEERS: usize = 300;
/// Tamaño de cada tanda de equipos por llamada (el servidor acepta cuerpos de hasta 100 KB).
pub const BATCH_BYTES: usize = 60_000;

// ─────────────────────────── Lógica pura (probada en CI) ───────────────────────────

/// Quita de la configuración de un equipo lo local y lo secreto antes de subirla.
pub fn sanitize_peer(v: &mut Value) {
    if let Some(o) = v.as_object_mut() {
        for f in PEER_LOCAL_FIELDS {
            o.remove(*f);
        }
        if let Some(opts) = o.get_mut("options").and_then(|x| x.as_object_mut()) {
            for k in PEER_SECRET_OPTIONS {
                opts.remove(*k);
            }
        }
    }
}

/// Configuración de la nube + lo local/secreto del archivo de esta computadora (si existe).
pub fn merge_peer(server: &Value, local: Option<&Value>) -> Value {
    let mut out = server.clone();
    sanitize_peer(&mut out);
    let Some(o) = out.as_object_mut() else {
        return out;
    };
    let Some(l) = local.and_then(|l| l.as_object()) else {
        return out;
    };
    for f in PEER_LOCAL_FIELDS {
        if let Some(x) = l.get(*f) {
            o.insert(f.to_string(), x.clone());
        }
    }
    if let Some(lopts) = l.get("options").and_then(|x| x.as_object()) {
        let opts = o.entry("options").or_insert_with(|| json!({}));
        if let Some(opts) = opts.as_object_mut() {
            for k in PEER_SECRET_OPTIONS {
                if let Some(x) = lopts.get(*k) {
                    opts.insert(k.to_string(), x.clone());
                }
            }
        }
    }
    out
}

/// Preferencias que cambiaron aquí desde la última versión acordada con el servidor.
pub fn diff_opts(local: &Map<String, Value>, snap: &Map<String, Value>) -> Map<String, Value> {
    local
        .iter()
        .filter(|(k, v)| snap.get(*k) != Some(*v))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Qué favoritos subir y cómo: la primera vez se unen con los de la nube; después solo si
/// cambiaron aquí (y entonces reemplazan).
pub fn fav_to_send(local: &[String], snap: Option<&Vec<String>>) -> (Option<Vec<String>>, &'static str) {
    match snap {
        None => (Some(local.to_vec()), "union"),
        Some(s) if s.as_slice() != local => (Some(local.to_vec()), "replace"),
        _ => (None, ""),
    }
}

/// Clave de una preferencia en la nube: `l:` local (LocalConfig), `d:` por defecto de sesiones.
pub fn opt_key(kind: char, key: &str) -> String {
    format!("{kind}:{key}")
}

/// Reparte los equipos en tandas de ~`max_bytes` (siempre al menos uno por tanda).
pub fn batches(peers: Vec<Value>, max_bytes: usize) -> Vec<Vec<Value>> {
    let mut out: Vec<Vec<Value>> = vec![];
    let mut cur: Vec<Value> = vec![];
    let mut size = 0;
    for p in peers {
        let n = p.to_string().len();
        if !cur.is_empty() && size + n > max_bytes {
            out.push(std::mem::take(&mut cur));
            size = 0;
        }
        size += n;
        cur.push(p);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub fn ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

// ─────────────────────────── Estado local ───────────────────────────

#[derive(serde::Serialize, serde::Deserialize, Default, Debug, Clone)]
pub struct State {
    /// usuario de la cuenta con que se sincronizó (si cambia, se empieza de cero)
    pub user: String,
    /// hasta dónde se bajaron cambios de equipos
    pub cursor: i64,
    /// favoritos y preferencias tal como quedaron en el servidor la última vez
    pub fav: Option<Vec<String>>,
    pub opts: Option<Map<String, Value>>,
    /// equipos modificados después de esto (ms) se suben
    pub pushed_at: u64,
    /// equipos que había aquí tras la última sincronización (para propagar los quitados)
    pub known: Vec<String>,
}

fn state_path() -> PathBuf {
    Config::path("canela_sync.json")
}

pub fn load_state() -> State {
    std::fs::read_to_string(state_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_state(st: &State) {
    if let Ok(s) = serde_json::to_string(st) {
        if let Err(e) = std::fs::write(state_path(), s) {
            log::warn!("canela_sync: no se pudo guardar el estado: {e}");
        }
    }
}

// ─────────────────────────── Lectura y escritura local ───────────────────────────

pub fn read_opts() -> Map<String, Value> {
    let mut m = Map::new();
    for k in LOCAL_KEYS {
        m.insert(opt_key('l', k), json!(LocalConfig::get_option(k)));
    }
    let d = UserDefaultConfig::load();
    for k in keys::KEYS_DISPLAY_SETTINGS {
        m.insert(opt_key('d', k), json!(d.get(k)));
    }
    m
}

/// Aplica una preferencia que vino de la nube. Solo las claves que esta versión conoce.
pub fn write_opt(k: &str, v: &Value) -> bool {
    let Some(v) = v.as_str() else { return false };
    if let Some(key) = k.strip_prefix("l:") {
        if LOCAL_KEYS.contains(&key) {
            LocalConfig::set_option(key.to_owned(), v.to_owned());
            return true;
        }
    } else if let Some(key) = k.strip_prefix("d:") {
        if keys::KEYS_DISPLAY_SETTINGS.contains(&key) {
            UserDefaultConfig::load().set(key.to_owned(), v.to_owned());
            return true;
        }
    }
    false
}

/// Equipos de Recientes: id → (fecha de modificación, archivo). Del más reciente al más viejo.
pub fn local_peers() -> Vec<(String, SystemTime, PathBuf)> {
    PeerConfig::get_vec_id_modified_time_path(&None)
}

pub fn peer_json(id: &str) -> Option<Value> {
    serde_json::to_value(PeerConfig::load(id)).ok()
}

fn set_mtime(id: &str, at_ms: u64) {
    let Some((_, _, path)) = PeerConfig::get_vec_id_modified_time_path(&Some(vec![id.to_owned()])).into_iter().next() else {
        return;
    };
    if let Err(e) = set_file_mtime(&path, at_ms) {
        log::warn!("canela_sync: fecha de {id} ({}): {e}", path.display());
    }
}

/// Pone la fecha de modificación (la que ordena Recientes). `File::set_modified` no existe en el
/// Rust 1.75 con que se compila Windows 32 bits (Sciter): se usa la API de cada sistema.
pub fn set_file_mtime(path: &std::path::Path, at_ms: u64) -> std::io::Result<()> {
    let f = std::fs::File::options().write(true).open(path)?;
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        // FILETIME = intervalos de 100 ns desde 1601-01-01
        let v = at_ms * 10_000 + 116_444_736_000_000_000;
        let ft = winapi::shared::minwindef::FILETIME {
            dwLowDateTime: v as u32,
            dwHighDateTime: (v >> 32) as u32,
        };
        let ok = unsafe {
            winapi::um::fileapi::SetFileTime(f.as_raw_handle() as _, std::ptr::null(), std::ptr::null(), &ft)
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    #[cfg(unix)]
    {
        use hbb_common::libc;
        use std::os::unix::io::AsRawFd;
        let ts = [
            libc::timespec { tv_sec: 0, tv_nsec: libc::UTIME_OMIT },
            libc::timespec { tv_sec: (at_ms / 1000) as libc::time_t, tv_nsec: ((at_ms % 1000) * 1_000_000) as _ },
        ];
        if unsafe { libc::futimens(f.as_raw_fd(), ts.as_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    let _ = &f;
    Ok(())
}

/// Aplica un equipo que vino de la nube. Devuelve true si cambió algo aquí.
pub fn apply_peer(p: &Value, local: &HashMap<String, SystemTime>) -> bool {
    let Some(id) = p.get("id").and_then(|x| x.as_str()) else { return false };
    let used = p.get("used_at").and_then(|x| x.as_u64()).unwrap_or(0);
    let mine = local.get(id).map(|t| ms(*t));
    if p.get("deleted").and_then(|x| x.as_bool()).unwrap_or(false) {
        // quitado en otra computadora: se quita aquí si no se usó después
        if let Some(m) = mine {
            if m <= used {
                PeerConfig::remove(id);
                return true;
            }
        }
        return false;
    }
    if let Some(m) = mine {
        if m >= used {
            return false; // lo de aquí es igual o más nuevo
        }
    }
    let Some(server) = p.get("config") else { return false };
    let local_v = if mine.is_some() { peer_json(id) } else { None };
    let merged = merge_peer(server, local_v.as_ref());
    let cfg: PeerConfig = match serde_json::from_value(merged) {
        Ok(c) => c,
        Err(e) => {
            log::warn!("canela_sync: equipo {id} con formato inválido: {e}");
            return false;
        }
    };
    if cfg.info.platform.is_empty() {
        return false; // RustDesk borra los archivos sin plataforma: no vale la pena
    }
    cfg.store(id);
    set_mtime(id, used);
    true
}

/// Usuario con sesión iniciada en la app (vacío = sin sesión).
pub fn current_user() -> String {
    serde_json::from_str::<Value>(&LocalConfig::get_option("user_info"))
        .ok()
        .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(|s| s.to_owned()))
        .unwrap_or_default()
}

fn token() -> String {
    LocalConfig::get_option("access_token")
}

/// Huella barata del estado local: si cambia, hay algo que subir.
pub fn fingerprint() -> String {
    let peers = local_peers();
    let newest = peers.first().map(|(_, t, _)| ms(*t)).unwrap_or(0);
    format!(
        "{}|{}|{}|{}|{}|{}",
        token().len(),
        current_user(),
        LocalConfig::get_fav().join(","),
        Value::Object(read_opts()),
        peers.len(),
        newest
    )
}

fn disabled() -> bool {
    Config::get_option("canela-sync").eq_ignore_ascii_case("N")
}

// ─────────────────────────── Sincronización ───────────────────────────

/// Una vuelta completa. Devuelve (cambiaron favoritos aquí, cambiaron recientes aquí).
pub async fn sync_once() -> ResultType<(bool, bool)> {
    let token = token();
    let user = current_user();
    if token.is_empty() || user.is_empty() || disabled() {
        return Ok((false, false));
    }
    let api = crate::get_api_server(Config::get_option("api-server"), Config::get_option("custom-rendezvous-server"));
    if api.is_empty() {
        return Ok((false, false));
    }
    let mut st = load_state();
    if st.user != user {
        st = State { user: user.clone(), ..Default::default() };
    }
    let started = ms(SystemTime::now());

    // favoritos y preferencias: lo cambiado aquí
    let local_fav = LocalConfig::get_fav();
    let (fav_send, fav_mode) = fav_to_send(&local_fav, st.fav.as_ref());
    let local_opts = read_opts();
    let (opts_send, opts_mode) = match &st.opts {
        None => (local_opts.clone(), "fill"),
        Some(snap) => (diff_opts(&local_opts, snap), "set"),
    };

    // equipos: los modificados desde la última subida y los quitados de Recientes
    let peers_now = local_peers();
    let ids_now: HashSet<String> = peers_now.iter().map(|(id, _, _)| id.clone()).collect();
    let mut peers = vec![];
    for (id, t, _) in peers_now.iter().take(MAX_PEERS) {
        let m = ms(*t);
        if m > st.pushed_at {
            if let Some(mut v) = peer_json(id) {
                sanitize_peer(&mut v);
                peers.push(json!({ "id": id, "used_at": m, "config": v }));
            }
        }
    }
    for id in st.known.iter().filter(|k| !ids_now.contains(*k)) {
        peers.push(json!({ "id": id, "used_at": started, "deleted": true }));
    }

    let header = format!("Authorization: Bearer {token}");
    let mut pending = batches(peers, BATCH_BYTES).into_iter();
    let mut body = json!({
        "cursor": st.cursor,
        "fav": fav_send, "fav_mode": fav_mode,
        "opts": opts_send, "opts_mode": opts_mode,
        "peers": pending.next().unwrap_or_default(),
    });
    let (mut fav_changed, mut recent_changed) = (false, false);
    let mut mtimes: HashMap<String, SystemTime> = peers_now.iter().map(|(id, t, _)| (id.clone(), *t)).collect();
    for _page in 0..100 {
        let text = crate::post_request(format!("{api}/api/canela/sync"), body.to_string(), &header).await?;
        let rsp: Value = serde_json::from_str(&text)?;
        if let Some(e) = rsp.get("error").and_then(|e| e.as_str()) {
            hbb_common::bail!("{e}");
        }
        if let Some(fav) = rsp.get("fav").and_then(|f| f.as_array()) {
            let fav: Vec<String> = fav.iter().filter_map(|x| x.as_str().map(|s| s.to_owned())).collect();
            if LocalConfig::get_fav() != fav {
                LocalConfig::set_fav(fav.clone());
                fav_changed = true;
            }
            st.fav = Some(fav);
        }
        if let Some(opts) = rsp.get("opts").and_then(|o| o.as_object()) {
            let mut snap = Map::new();
            for (k, v) in opts {
                if local_opts.get(k) != Some(v) {
                    write_opt(k, v);
                }
                if local_opts.contains_key(k) {
                    snap.insert(k.clone(), v.clone());
                }
            }
            st.opts = Some(snap);
        }
        for p in rsp.get("peers").and_then(|p| p.as_array()).into_iter().flatten() {
            if apply_peer(p, &mtimes) {
                recent_changed = true;
                if let Some(id) = p.get("id").and_then(|x| x.as_str()) {
                    if p.get("deleted").and_then(|x| x.as_bool()).unwrap_or(false) {
                        mtimes.remove(id);
                    } else {
                        let used = p.get("used_at").and_then(|x| x.as_u64()).unwrap_or(0);
                        mtimes.insert(id.to_owned(), UNIX_EPOCH + Duration::from_millis(used));
                    }
                }
            }
        }
        if let Some(c) = rsp.get("cursor").and_then(|c| c.as_i64()) {
            st.cursor = c;
        }
        let next = pending.next();
        if next.is_none() && !rsp.get("more").and_then(|m| m.as_bool()).unwrap_or(false) {
            break;
        }
        // llamadas siguientes: la próxima tanda de equipos (si queda) y seguir bajando
        body = json!({ "cursor": st.cursor, "peers": next.unwrap_or_default() });
    }
    st.pushed_at = started;
    st.known = local_peers().into_iter().map(|(id, _, _)| id).collect();
    save_state(&st);
    Ok((fav_changed, recent_changed))
}

#[tokio::main(flavor = "current_thread")]
async fn sync_blocking() -> ResultType<(bool, bool)> {
    sync_once().await
}

/// Avisa a la ventana principal que recargue Recientes/Favoritos.
fn refresh_ui(fav: bool, recent: bool) {
    #[cfg(any(target_os = "android", target_os = "ios", feature = "flutter"))]
    {
        if recent {
            crate::flutter_ffi::main_load_recent_peers();
        }
        if fav || recent {
            crate::flutter_ffi::main_load_fav_peers();
        }
    }
    let _ = (fav, recent);
}

use std::sync::atomic::{AtomicBool, Ordering};
static STARTED: AtomicBool = AtomicBool::new(false);

/// Arranca la sincronización (una sola vez, en el proceso de la ventana principal).
pub fn start() {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(5));
        let mut last_fp = String::new();
        let mut last_run: Option<Instant> = None;
        loop {
            let fp = fingerprint();
            let due = last_run.map(|t| t.elapsed() >= Duration::from_secs(60)).unwrap_or(true);
            if fp != last_fp || due {
                match sync_blocking() {
                    Ok((fav, recent)) => refresh_ui(fav, recent),
                    Err(e) => log::warn!("canela_sync: {e}"),
                }
                last_run = Some(Instant::now());
                last_fp = fingerprint();
            }
            std::thread::sleep(Duration::from_secs(5));
        }
    });
}
