// Prueba aislada de src/canela_sync.rs: la lógica pura y la escritura REAL de los archivos de
// equipos (peers/<id>.toml) y de las preferencias, en una carpeta de configuración temporal.
// La llamada HTTP no se prueba aquí (la cubre el smoke de la API).
#[path = "../../../src/canela_sync.rs"]
mod canela_sync;
pub use hbb_common::ResultType;
pub fn get_api_server(a: String, _c: String) -> String { a }
pub async fn post_request(_u: String, _b: String, _h: &str) -> ResultType<String> { Ok(String::new()) }

use canela_sync::*;
use hbb_common::config::{keys, LocalConfig, PeerConfig, UserDefaultConfig};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn mtime(id: &str) -> u64 {
    let v = PeerConfig::get_vec_id_modified_time_path(&Some(vec![id.to_owned()]));
    ms(v.first().expect("existe").1)
}

fn local_map() -> HashMap<String, SystemTime> {
    local_peers().into_iter().map(|(id, t, _)| (id, t)).collect()
}

fn main() {
    // carpeta de configuración aislada (antes de tocar cualquier config)
    let tmp = std::env::temp_dir().join(format!("canela-sync-check-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    std::env::set_var("HOME", &tmp);
    std::env::set_var("XDG_CONFIG_HOME", tmp.join(".config"));
    *hbb_common::config::APP_NAME.write().unwrap() = "CanelaSyncCheck".to_owned();

    // ── lógica pura ──
    let mut v = json!({ "password": [1,2,3], "view_style": "adaptive", "size": [0,0,10,10], "ui_flutter": {"x":"1"},
        "transfer": {}, "direct_failures": 3, "options": { "rdp_password": "s", "os-password": "p", "codec-preference": "h264" } });
    sanitize_peer(&mut v);
    assert!(v.get("password").is_none() && v.get("size").is_none() && v.get("ui_flutter").is_none() && v.get("direct_failures").is_none());
    assert!(v["options"].get("rdp_password").is_none() && v["options"].get("os-password").is_none());
    assert_eq!(v["options"]["codec-preference"], "h264");
    println!("sanitize       → no suben contraseñas, credenciales ni ventanas");

    let server = json!({ "view_style": "adaptive", "options": { "codec-preference": "vp9" }, "password": [9] });
    let local = json!({ "view_style": "original", "password": [7,7], "size": [1,2,3,4], "options": { "os-username": "admin", "codec-preference": "h264" } });
    let m = merge_peer(&server, Some(&local));
    assert_eq!(m["view_style"], "adaptive");
    assert_eq!(m["password"], json!([7,7]));
    assert_eq!(m["size"], json!([1,2,3,4]));
    assert_eq!(m["options"]["os-username"], "admin");
    assert_eq!(m["options"]["codec-preference"], "vp9");
    println!("merge          → ajustes de la nube, contraseña y ventana de aquí");

    let mut a = Map::new(); a.insert("l:theme".into(), json!("dark")); a.insert("d:codec-preference".into(), json!("h264"));
    let mut b = Map::new(); b.insert("l:theme".into(), json!("light")); b.insert("d:codec-preference".into(), json!("h264"));
    let d = diff_opts(&a, &b);
    assert_eq!(d.len(), 1); assert_eq!(d["l:theme"], "dark");
    let fav = vec!["1".to_string(), "2".to_string()];
    assert_eq!(fav_to_send(&fav, None), (Some(fav.clone()), "union"));
    assert_eq!(fav_to_send(&fav, Some(&fav)), (None, ""));
    assert_eq!(fav_to_send(&fav, Some(&vec!["1".to_string()])), (Some(fav.clone()), "replace"));
    println!("3 bandas       → solo sube lo cambiado; la primera vez une favoritos");

    // ── archivos reales de equipos ──
    let mut c = PeerConfig::default();
    c.password = b"clave-local".to_vec();
    c.info.platform = "Windows".into();
    c.info.hostname = "CAJA-1".into();
    c.view_style = "original".into();
    c.port_forwards = vec![(14330, "".into(), 50457)];
    c.options.insert("os-password".into(), "secreta".into());
    c.store("111222333");
    let mut j = peer_json("111222333").unwrap();
    assert_eq!(j["port_forwards"], json!([[14330, "", 50457]]), "los túneles van en el JSON");
    sanitize_peer(&mut j);
    assert!(j.get("password").is_none());

    // la nube trae una versión más nueva: se aplica y se conservan los secretos locales
    let now = ms(SystemTime::now());
    let mut nube = j.clone();
    nube["view_style"] = json!("adaptive");
    nube["port_forwards"] = json!([[26380, "", 2638]]);
    let p = json!({ "id": "111222333", "used_at": now + 60_000, "config": nube });
    assert!(apply_peer(&p, &local_map()), "aplica la versión más nueva");
    let c2 = PeerConfig::load("111222333");
    assert_eq!(c2.view_style, "adaptive");
    assert_eq!(c2.port_forwards, vec![(26380, "".to_string(), 2638)]);
    assert_eq!(c2.password, b"clave-local".to_vec(), "la contraseña guardada se queda");
    assert_eq!(c2.options.get("os-password").map(|s| s.as_str()), Some("secreta"));
    assert_eq!(mtime("111222333") / 1000, (now + 60_000) / 1000, "la fecha (orden de Recientes) es la de la nube");
    println!("equipo nuevo   → se aplica, con fecha de la nube y la clave local intacta");

    // una versión más vieja no pisa
    let vieja = json!({ "id": "111222333", "used_at": now - 3_600_000, "config": { "view_style": "original", "info": { "platform": "Windows" } } });
    assert!(!apply_peer(&vieja, &local_map()));
    assert_eq!(PeerConfig::load("111222333").view_style, "adaptive");
    println!("más vieja      → no pisa lo de aquí");

    // equipo que no existe aquí: se crea (aparece en Recientes)
    let otro = json!({ "id": "444555666", "used_at": now, "config": { "view_style": "adaptive", "info": { "platform": "Linux", "hostname": "srv" } } });
    assert!(apply_peer(&otro, &local_map()));
    assert!(PeerConfig::exists("444555666"));
    assert!(local_peers().iter().any(|(id, _, _)| id == "444555666"), "sale en Recientes");
    // sin plataforma no se crea (RustDesk lo borraría)
    assert!(!apply_peer(&json!({ "id": "777", "used_at": now, "config": { "view_style": "x" } }), &local_map()));
    println!("de otra compu  → aparece en Recientes");

    // quitado en otra computadora después de usarlo aquí → se quita
    assert!(apply_peer(&json!({ "id": "444555666", "used_at": now + 1_000, "deleted": true }), &local_map()));
    assert!(!PeerConfig::exists("444555666"));
    // quitado ANTES de que se usara aquí → se queda
    assert!(!apply_peer(&json!({ "id": "111222333", "used_at": now - 1_000, "deleted": true }), &local_map()));
    assert!(PeerConfig::exists("111222333"));
    println!("quitados       → se propagan, salvo si aquí se usó después");

    // ── preferencias ──
    assert!(write_opt("d:codec-preference", &json!("h264")));
    assert!(write_opt("l:theme", &json!("dark")));
    assert!(!write_opt("l:access_token", &json!("robado")), "solo claves conocidas");
    assert!(!write_opt("x:theme", &json!("dark")));
    let o = read_opts();
    assert_eq!(o["d:codec-preference"], "h264");
    assert_eq!(o["l:theme"], "dark");
    assert_eq!(UserDefaultConfig::load().get(keys::OPTION_CODEC_PREFERENCE), "h264");
    assert_eq!(LocalConfig::get_option(keys::OPTION_THEME), "dark");
    assert!(o.keys().all(|k| k.starts_with("l:") || k.starts_with("d:")));
    assert!(!o.contains_key("l:access_token") && !o.contains_key("l:user_info"));
    println!("preferencias   → se escriben y leen; nada de tokens ni credenciales");

    // estado
    let st = State { user: "tec".into(), cursor: 42, fav: Some(fav.clone()), opts: Some(Map::new()), pushed_at: now, known: vec!["111222333".into()] };
    save_state(&st);
    let back = load_state();
    assert_eq!((back.user.as_str(), back.cursor, back.known.len()), ("tec", 42, 1));
    println!("estado         → se guarda y se lee");

    let _ = std::fs::remove_dir_all(&tmp);
    let _ = (UNIX_EPOCH, Duration::from_secs(0), Value::Null);
    println!("\nTODO OK");
}
