//! CanelaRemote: el agente detecta las bases de datos del equipo y le dice a la API en qué
//! puerto TCP escucha cada una.
//!
//! Para qué: al equipo se le da soporte por túnel (SQL Server de POS, la base de PixelPoint).
//! SQL Server a menudo es una instancia con nombre en un puerto dinámico que cambia; sin saber el
//! número, el técnico no puede tunelizar. El agente corre como servicio (SYSTEM) en el equipo, así
//! que lee el puerto real del registro y de los sockets en escucha, y lo reporta. El técnico abre
//! el túnel al puerto correcto sin activar ni reiniciar nada.
//!
//! Seguridad: solo detección FIJA de SQL Server y SQL Anywhere, de solo lectura. No ejecuta
//! comandos que venga de nadie ni manda credenciales: únicamente tipo, puerto y una etiqueta. Se
//! puede apagar con la opción `canela-services=N` del custom.txt.

#![allow(dead_code)]
use hbb_common::tokio; // tokio re-exportado por hbb_common (features full): para #[tokio::main]
use serde_json::{json, Value};
use std::collections::HashMap;

/// Un servicio de base de datos alcanzable por túnel.
#[derive(Debug, Clone, PartialEq)]
pub struct Service {
    pub kind: &'static str, // "sqlserver" | "sqlanywhere"
    pub port: u16,          // 0 = sin puerto TCP (p. ej. SQL Server con TCP/IP apagado)
    pub instance: Option<String>,
    pub process: Option<String>,
    pub tcp_enabled: Option<bool>,
}

impl Service {
    fn to_json(&self) -> Value {
        let mut v = json!({ "kind": self.kind, "port": self.port });
        if let Some(i) = &self.instance {
            v["instance"] = json!(i);
        }
        if let Some(p) = &self.process {
            v["process"] = json!(p);
        }
        if let Some(t) = self.tcp_enabled {
            v["tcp_enabled"] = json!(t);
        }
        v
    }
}

// ─────────────────────────── Parsers (puros, probados en CI) ───────────────────────────

/// `reg query …\Instance Names\SQL` → [(nombre visible, nombre interno)].
/// Líneas: `    SQLEXPRESS    REG_SZ    MSSQL16.SQLEXPRESS`
pub fn parse_instances(out: &str) -> Vec<(String, String)> {
    let mut v = vec![];
    for line in out.lines() {
        let p: Vec<&str> = line.split_whitespace().collect();
        if p.len() >= 3 && p[1].eq_ignore_ascii_case("REG_SZ") {
            v.push((p[0].to_string(), p[2..].join(" ")));
        }
    }
    v
}

/// Valor de un `reg query … /v <name>`. Devuelve el último token (el dato) de la línea del valor.
/// `    TcpDynamicPorts    REG_SZ    54213` → "54213"; `    Enabled    REG_DWORD    0x1` → "0x1".
pub fn parse_reg_value(out: &str, name: &str) -> Option<String> {
    for line in out.lines() {
        let p: Vec<&str> = line.split_whitespace().collect();
        if p.len() >= 3 && p[0].eq_ignore_ascii_case(name) && p[1].to_ascii_uppercase().starts_with("REG_") {
            return Some(p[p.len() - 1].to_string());
        }
    }
    None
}

/// Un puerto de un valor del registro: "54213" o "1433". Vacío/ausente → None.
pub fn reg_port(v: Option<String>) -> Option<u16> {
    v.and_then(|s| s.trim().parse::<u16>().ok()).filter(|p| *p > 0)
}

/// REG_DWORD "0x1"/"0x0" o decimal → bool.
pub fn reg_bool(v: Option<String>) -> Option<bool> {
    let s = v?;
    let s = s.trim().to_ascii_lowercase();
    if let Some(h) = s.strip_prefix("0x") {
        u32::from_str_radix(h, 16).ok().map(|n| n != 0)
    } else {
        s.parse::<u32>().ok().map(|n| n != 0)
    }
}

/// `netstat -ano -p tcp` → [(puerto local, pid)] solo de las líneas LISTENING.
/// `  TCP    0.0.0.0:2638    0.0.0.0:0    LISTENING    4321`
pub fn parse_netstat_listen(out: &str) -> Vec<(u16, u32)> {
    let mut v = vec![];
    for line in out.lines() {
        let p: Vec<&str> = line.split_whitespace().collect();
        if p.len() < 4 || !p[0].eq_ignore_ascii_case("TCP") {
            continue;
        }
        if !p.iter().any(|w| w.eq_ignore_ascii_case("LISTENING")) {
            continue;
        }
        let port = p[1].rsplit(':').next().and_then(|s| s.parse::<u16>().ok());
        let pid = p[p.len() - 1].parse::<u32>().ok();
        if let (Some(port), Some(pid)) = (port, pid) {
            if port > 0 {
                v.push((port, pid));
            }
        }
    }
    v
}

/// `tasklist /fo csv /nh` → {pid: nombre.exe}. `"dbsrv17.exe","4321","Services","0","120,000 K"`
pub fn parse_tasklist_csv(out: &str) -> HashMap<u32, String> {
    let mut m = HashMap::new();
    for line in out.lines() {
        let cols: Vec<String> = line
            .split("\",\"")
            .map(|c| c.trim_matches('"').to_string())
            .collect();
        if cols.len() >= 2 {
            if let Ok(pid) = cols[1].trim().parse::<u32>() {
                m.insert(pid, cols[0].clone());
            }
        }
    }
    m
}

/// El proceso es un servidor de SQL Anywhere (el motor de PixelPoint): dbsrvNN / dbengNN.
pub fn is_sqlanywhere(proc: &str) -> bool {
    let p = proc.to_ascii_lowercase();
    p.starts_with("dbsrv") || p.starts_with("dbeng")
}

/// Compone los servicios a partir de las salidas ya obtenidas. Puro: es lo que se prueba.
pub fn compose(
    instances: &[(String, String, Option<u16>, Option<bool>)], // (visible, interno, puerto, tcp_enabled)
    listen: &[(u16, u32)],
    procs: &HashMap<u32, String>,
) -> Vec<Service> {
    let mut out = vec![];
    for (visible, _interno, port, tcp) in instances {
        // Solo interesa reportar si hay puerto, o si sabemos que TCP está apagado (para avisar).
        if port.is_none() && *tcp != Some(false) {
            continue;
        }
        out.push(Service {
            kind: "sqlserver",
            port: port.unwrap_or(0),
            instance: Some(visible.clone()),
            process: None,
            tcp_enabled: *tcp,
        });
    }
    let mut vistos = std::collections::HashSet::new();
    for (port, pid) in listen {
        if let Some(name) = procs.get(pid) {
            if is_sqlanywhere(name) && vistos.insert(*port) {
                out.push(Service {
                    kind: "sqlanywhere",
                    port: *port,
                    instance: None,
                    process: Some(name.clone()),
                    tcp_enabled: Some(true),
                });
            }
        }
    }
    out
}

pub fn to_json_array(services: &[Service]) -> Value {
    Value::Array(services.iter().map(|s| s.to_json()).collect())
}

// ─────────────────────────── Detección real (solo Windows) ───────────────────────────

#[cfg(windows)]
fn run(cmd: &str, args: &[&str]) -> String {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new(cmd)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

#[cfg(windows)]
pub fn detect() -> Vec<Service> {
    const BASE: &str = r"HKLM\SOFTWARE\Microsoft\Microsoft SQL Server";
    let mut instances = vec![];
    for (visible, interno) in parse_instances(&run("reg", &["query", &format!(r"{BASE}\Instance Names\SQL")])) {
        let tcp = format!(r"{BASE}\{interno}\MSSQLServer\SuperSocketNetLib\Tcp");
        let enabled = reg_bool(parse_reg_value(&run("reg", &["query", &tcp, "/v", "Enabled"]), "Enabled"));
        let ipall = format!(r"{tcp}\IPAll");
        let port = reg_port(parse_reg_value(&run("reg", &["query", &ipall, "/v", "TcpPort"]), "TcpPort"))
            .or_else(|| reg_port(parse_reg_value(&run("reg", &["query", &ipall, "/v", "TcpDynamicPorts"]), "TcpDynamicPorts")));
        instances.push((visible, interno, port, enabled));
    }
    let listen = parse_netstat_listen(&run("netstat", &["-ano", "-p", "tcp"]));
    let procs = parse_tasklist_csv(&run("tasklist", &["/fo", "csv", "/nh"]));
    compose(&instances, &listen, &procs)
}

#[cfg(not(windows))]
pub fn detect() -> Vec<Service> {
    // SQL Server y PixelPoint son de Windows; en macOS/Linux/Android no hay nada que detectar.
    vec![]
}

// ─────────────────────────── Reporte a la API ───────────────────────────

/// ¿Está apagada la detección por configuración? (`canela-services=N`)
fn disabled() -> bool {
    hbb_common::config::Config::get_option("canela-services").eq_ignore_ascii_case("N")
}

/// Detecta y, si hay algo (o cambió), lo POSTea a /api/canela/services. Guarda un hash para no
/// repetir el POST cuando no cambió nada.
pub async fn report() {
    if disabled() {
        return;
    }
    let api = crate::get_api_server(
        hbb_common::config::Config::get_option("api-server"),
        hbb_common::config::Config::get_option("custom-rendezvous-server"),
    );
    if api.is_empty() {
        return;
    }
    let services = detect();
    let arr = to_json_array(&services);
    let hash = format!("{:x}", md5_of(&arr.to_string()));
    if hash == hbb_common::config::Status::get("canela_services_hash") {
        return; // sin cambios: no repetir
    }
    let body = json!({ "id": hbb_common::config::Config::get_id(), "services": arr }).to_string();
    match crate::post_request(format!("{api}/api/canela/services"), body, "").await {
        Ok(_) => {
            hbb_common::config::Status::set("canela_services_hash", hash);
            hbb_common::log::info!("canela: {} servicio(s) de BD reportado(s)", services.len());
        }
        Err(e) => hbb_common::log::warn!("canela: no se pudo reportar servicios: {e}"),
    }
}

fn md5_of(s: &str) -> u128 {
    // hash simple y estable (no criptográfico): solo para saber si cambió el reporte.
    let mut h: u128 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h = h.wrapping_mul(0x1000_0000_01b3).wrapping_add(b as u128);
    }
    h
}

use std::sync::atomic::{AtomicBool, Ordering};
static STARTED: AtomicBool = AtomicBool::new(false);

/// Arranca el reporte periódico (una sola vez). Lo llama `sync::start()`, que corre en todo equipo.
pub fn start() {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(25)); // dejar que el agente arranque
        loop {
            report_blocking();
            std::thread::sleep(std::time::Duration::from_secs(30 * 60));
        }
    });
}

#[tokio::main(flavor = "current_thread")]
async fn report_blocking() {
    report().await;
}
