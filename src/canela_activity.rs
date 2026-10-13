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
        flush_events();
    });
}

// ── Parche 26: la App Técnico es la fuente de cada soporte ──────────────────────────────────────
// Cada conexión manda "open" al conectar y "close" al terminar (con su hora exacta) a
// POST /api/canela/conns. Así el soporte queda registrado con su técnico aunque el equipo del cliente
// no reporte nada (app vieja o genérica, sin internet). Lo que no se pudo mandar espera en cola, en
// orden, y se reintenta cada minuto.

/// Eventos guardados para reintentar (los más viejos se descartan primero).
pub const MAX_PENDING_EVENTS: usize = 500;

#[derive(Clone, Debug, PartialEq)]
pub struct ConnEvent {
    /// identificador de la conexión, igual en su "open" y su "close"
    pub conn_ref: String,
    pub peer: String,
    /// tipo de conexión (ConnType: 0 control, 1 archivos, 4 cámara…)
    pub kind: i32,
    pub event: &'static str,
    /// segundos desde 1970 (UTC)
    pub at: i64,
}

#[derive(Default)]
pub struct Events {
    pub items: Vec<ConnEvent>,
}

impl Events {
    fn trim(&mut self) {
        if self.items.len() > MAX_PENDING_EVENTS {
            let extra = self.items.len() - MAX_PENDING_EVENTS;
            self.items.drain(..extra);
        }
    }

    pub fn push(&mut self, e: ConnEvent) {
        self.items.push(e);
        self.trim();
    }

    pub fn take(&mut self) -> Vec<ConnEvent> {
        std::mem::take(&mut self.items)
    }

    /// Lo que no se pudo mandar vuelve adelante, para no desordenar "open" y "close".
    pub fn put_back(&mut self, mut rows: Vec<ConnEvent>) {
        rows.append(&mut self.items);
        self.items = rows;
        self.trim();
    }
}

pub fn events_body(rows: &[ConnEvent]) -> Value {
    json!({ "items": rows.iter().map(|e| json!({
        "ref": e.conn_ref, "peer": e.peer, "type": e.kind, "event": e.event, "at": e.at
    })).collect::<Vec<_>>() })
}

hbb_common::lazy_static::lazy_static! {
    static ref EVENTS: Mutex<Events> = Default::default();
    /// un solo envío a la vez: los eventos llegan a la API en el orden en que pasaron
    static ref SENDING: Mutex<()> = Mutex::new(());
}
static REF_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn new_conn_ref() -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let seq = REF_SEQ.fetch_add(1, Ordering::SeqCst);
    format!("{:x}{:x}{:x}", nanos, std::process::id(), seq)
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn record(e: ConnEvent) {
    if LocalConfig::get_option("access_token").is_empty() {
        return;
    }
    EVENTS.lock().unwrap().push(e);
    std::thread::spawn(flush_events);
    start();
}

async fn send_events(rows: &[ConnEvent]) -> bool {
    let token = LocalConfig::get_option("access_token");
    if token.is_empty() {
        return false;
    }
    let api = crate::get_api_server(Config::get_option("api-server"), Config::get_option("custom-rendezvous-server"));
    if api.is_empty() {
        return false;
    }
    let header = format!("Authorization: Bearer {token}");
    match crate::post_request(format!("{api}/api/canela/conns"), events_body(rows).to_string(), &header).await {
        Ok(res) => res.contains("\"ok\""),
        Err(e) => {
            log::debug!("canela_activity conns: {e}");
            false
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn flush_events() {
    let _one = SENDING.lock().unwrap();
    let rows = EVENTS.lock().unwrap().take();
    if rows.is_empty() {
        return;
    }
    if !send_events(&rows).await {
        EVENTS.lock().unwrap().put_back(rows);
    }
}

/// Soportes más cortos que esto no preguntan "qué se hizo" (entrar y salir no es un soporte).
pub const SESSION_DONE_MIN_SECS: u64 = 60;

/// Marca la sesión con `peer` como abierta mientras viva; al soltarse (por donde sea que termine
/// la sesión) la cierra. Así ninguna sesión queda "abierta para siempre" en la medición. Además
/// reporta la conexión a la API (parche 26) al abrir y al cerrar.
pub struct Guard {
    peer: String,
    since: std::time::Instant,
    conn_ref: String,
    kind: i32,
}

impl Guard {
    pub fn new(peer: &str) -> Self {
        Self::with_type(peer, 0)
    }

    pub fn with_type(peer: &str, kind: i32) -> Self {
        open(peer);
        let g = Guard {
            peer: peer.to_owned(),
            since: std::time::Instant::now(),
            conn_ref: new_conn_ref(),
            kind,
        };
        g.report("open");
        g
    }

    fn report(&self, event: &'static str) {
        record(ConnEvent {
            conn_ref: self.conn_ref.clone(),
            peer: peer_key(&self.peer),
            kind: self.kind,
            event,
            at: now_secs(),
        });
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        close(&self.peer);
        self.report("close");
        let secs = self.since.elapsed().as_secs();
        if secs >= SESSION_DONE_MIN_SECS {
            notify_session_done(&self.peer, secs);
        }
    }
}

/// Parche 25: avisa a la ventana principal de la App Técnico que terminó un soporte, para que
/// pregunte qué se hizo (flutter/lib/canela/session_done.dart → POST /api/canela/session-done).
#[cfg(feature = "flutter")]
fn notify_session_done(peer: &str, secs: u64) {
    if LocalConfig::get_option("access_token").is_empty() {
        return;
    }
    let id = peer_key(peer);
    let cfg = hbb_common::config::PeerConfig::load(&id);
    let alias = cfg.options.get("alias").cloned().unwrap_or_default();
    let name = if alias.trim().is_empty() { cfg.info.hostname.clone() } else { alias };
    let evt = json!({ "name": "canela_session_done", "peer": id, "peer_name": name, "seconds": secs });
    let _ = crate::flutter::push_global_event(crate::flutter::APP_TYPE_MAIN, evt.to_string());
}

#[cfg(not(feature = "flutter"))]
fn notify_session_done(_peer: &str, _secs: u64) {}
