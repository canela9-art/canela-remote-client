//! CanelaRemote: mide cuánto del tiempo conectado el técnico de verdad está trabajando.
//!
//! Una sesión puede quedar abierta el día entero sin que nadie la toque; para medir el
//! rendimiento hace falta separar el tiempo con interacción del tiempo solo conectado. La App
//! Técnico anota, por minuto y por equipo remoto:
//!
//! * `open`/`close`: hay una sesión abierta con ese equipo (control remoto o archivos; los túneles
//!   no pasan por aquí);
//! * `touch`: el técnico mandó teclado, mouse, toque o una acción de archivos a ese equipo.
//!
//! Cada minuto cerrado se reporta a `POST /api/canela/activity` como {peer, minute, active}. Lo que
//! no se pudo mandar se reintenta (hasta 2 días, el límite de la API). Solo con sesión iniciada.

#![allow(dead_code)]
use hbb_common::{
    config::{Config, LocalConfig},
    log, tokio,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Minutos que se guardan para reintentar (2 días, lo que acepta la API).
pub const MAX_PENDING_MINUTES: i64 = 2 * 1440;
/// Minutos por llamada (la API acepta hasta 5000).
pub const BATCH: usize = 2000;

#[derive(Default)]
pub struct Tracker {
    /// sesiones abiertas por equipo (puede haber más de una ventana al mismo equipo)
    pub open: HashMap<String, u32>,
    /// minutos con interacción por equipo, aún no cerrados/mandados
    pub touched: HashMap<String, BTreeSet<i64>>,
    /// listo para mandar: (equipo, minuto) → activo
    pub pending: BTreeMap<(String, i64), bool>,
}

/// "123456789@servidor" → "123456789"
pub fn peer_key(peer: &str) -> String {
    peer.split('@').next().unwrap_or(peer).trim().to_owned()
}

impl Tracker {
    pub fn open(&mut self, peer: &str) {
        *self.open.entry(peer_key(peer)).or_insert(0) += 1;
    }

    pub fn close(&mut self, peer: &str, now_min: i64) {
        let k = peer_key(peer);
        // el minuto en curso también cuenta como conectado
        self.pending.entry((k.clone(), now_min)).or_insert(false);
        if let Some(n) = self.open.get_mut(&k) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                self.open.remove(&k);
            }
        }
    }

    pub fn touch(&mut self, peer: &str, now_min: i64) {
        self.touched.entry(peer_key(peer)).or_default().insert(now_min);
    }

    /// Cierra el minuto `minute`: cada equipo abierto suma ese minuto (activo si hubo interacción) y
    /// los minutos con interacción ya terminados pasan a pendientes.
    pub fn tick(&mut self, minute: i64) {
        for peer in self.open.keys() {
            self.pending.entry((peer.clone(), minute)).or_insert(false);
        }
        for (peer, mins) in self.touched.iter_mut() {
            let done: Vec<i64> = mins.iter().copied().filter(|m| *m <= minute).collect();
            for m in done {
                mins.remove(&m);
                self.pending.insert((peer.clone(), m), true);
            }
        }
        self.touched.retain(|_, m| !m.is_empty());
        // no acumular para siempre si no hay red
        let oldest = minute - MAX_PENDING_MINUTES;
        self.pending.retain(|(_, m), _| *m > oldest);
    }

    /// Saca hasta `n` minutos pendientes para mandar.
    pub fn take(&mut self, n: usize) -> Vec<(String, i64, bool)> {
        let keys: Vec<(String, i64)> = self.pending.keys().take(n).cloned().collect();
        keys.into_iter()
            .filter_map(|k| self.pending.remove(&k).map(|a| (k.0, k.1, a)))
            .collect()
    }

    /// Devuelve lo que no se pudo mandar (gana "activo" si ya había otro valor).
    pub fn put_back(&mut self, rows: Vec<(String, i64, bool)>) {
        for (p, m, a) in rows {
            let e = self.pending.entry((p, m)).or_insert(false);
            *e = *e || a;
        }
    }
}

pub fn body(rows: &[(String, i64, bool)]) -> Value {
    json!({ "items": rows.iter().map(|(p, m, a)| json!({ "peer": p, "minute": m, "active": a })).collect::<Vec<_>>() })
}

pub fn now_min() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| (d.as_secs() / 60) as i64).unwrap_or(0)
}

hbb_common::lazy_static::lazy_static! {
    static ref TRACKER: Mutex<Tracker> = Default::default();
}
static STARTED: AtomicBool = AtomicBool::new(false);

/// Se abrió una sesión (control o archivos) con `peer`.
pub fn open(peer: &str) {
    TRACKER.lock().unwrap().open(peer);
    start();
}

/// Se cerró la sesión con `peer`.
pub fn close(peer: &str) {
    TRACKER.lock().unwrap().close(peer, now_min());
}

/// El técnico mandó teclado/mouse/toque/archivos a `peer`.
pub fn touch(peer: &str) {
    TRACKER.lock().unwrap().touch(peer, now_min());
}

async fn send(rows: &[(String, i64, bool)]) -> bool {
    let token = LocalConfig::get_option("access_token");
    if token.is_empty() {
        return false;
    }
    let api = crate::get_api_server(Config::get_option("api-server"), Config::get_option("custom-rendezvous-server"));
    if api.is_empty() {
        return false;
    }
    let header = format!("Authorization: Bearer {token}");
    match crate::post_request(format!("{api}/api/canela/activity"), body(rows).to_string(), &header).await {
        Ok(res) => res.contains("\"ok\""),
        Err(e) => {
            log::debug!("canela_activity: {e}");
            false
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn flush() {
    loop {
        let rows = TRACKER.lock().unwrap().take(BATCH);
        if rows.is_empty() {
            return;
        }
        if !send(&rows).await {
            TRACKER.lock().unwrap().put_back(rows);
            return;
        }
    }
}

/// Arranca el reloj de minutos (una sola vez, al abrir la primera sesión).
pub fn start() {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| loop {
        // esperar al cambio de minuto y cerrar el anterior
        let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        std::thread::sleep(Duration::from_secs(60 - secs % 60 + 1));
        TRACKER.lock().unwrap().tick(now_min() - 1);
        flush();
    });
}

/// Marca la sesión con `peer` como abierta mientras viva; al soltarse (por donde sea que termine
/// la sesión) la cierra. Así ninguna sesión queda "abierta para siempre" en la medición.
pub struct Guard(String);

impl Guard {
    pub fn new(peer: &str) -> Self {
        open(peer);
        Guard(peer.to_owned())
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        close(&self.0);
    }
}
