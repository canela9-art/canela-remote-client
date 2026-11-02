//! CanelaRemote: limpia lo que dejó un RustDesk desinstalado en el equipo (parche 31).
//!
//! Hoy: la impresora "RustDesk Printer". La instala el RustDesk original (CanelaRemote no la trae:
//! sus instaladores salen sin el driver) y queda si al desinstalarlo no marcaron quitarla. Sale en
//! la lista de impresoras de los programas del cliente (POS, facturación) con la marca de otro y no
//! imprime a ningún lado.
//!
//! Solo se borra si RustDesk YA NO está en el equipo: ni instalado (clave de desinstalación,
//! servicio, carpeta en Program Files) ni corriendo (portable). Si cualquiera de esas señales está,
//! no se toca: es de ese RustDesk. CanelaRemote usa su propio nombre de servicio y de clave
//! ("CanelaRemote"), así que no se confunde con estas señales. Se apaga con `canela-cleanup=N`.

#![allow(dead_code)]

/// Nombre con el que el RustDesk original crea su impresora: "{app} Printer".
pub const RUSTDESK_APP: &str = "RustDesk";

/// Señales de que RustDesk sigue en el equipo.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct RustDeskSignals {
    pub uninstall_key: bool,
    pub service: bool,
    pub program_dir: bool,
    pub running: bool,
}

impl RustDeskSignals {
    pub fn present(&self) -> bool {
        self.uninstall_key || self.service || self.program_dir || self.running
    }
}

// ─────────────────────────── Lógica pura (probada en CI) ───────────────────────────

/// ¿Hay un rustdesk.exe corriendo? Lee `tasklist /fo csv /nh`: `"rustdesk.exe","4321",...`.
/// Nuestro ejecutable se llama CanelaRemote.exe, así que no cuenta.
pub fn rustdesk_running(tasklist_csv: &str) -> bool {
    tasklist_csv.lines().any(|l| {
        l.split("\",\"")
            .next()
            .map(|c| c.trim().trim_matches('"').eq_ignore_ascii_case("rustdesk.exe"))
            .unwrap_or(false)
    })
}

/// Se borra solo si la impresora está y RustDesk no.
pub fn should_remove(printer_installed: bool, rustdesk: &RustDeskSignals) -> bool {
    printer_installed && !rustdesk.present()
}

// ─────────────────────────── En el equipo (solo Windows) ───────────────────────────

#[cfg(windows)]
fn run(cmd: &str, args: &[&str]) -> Option<std::process::Output> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new(cmd).args(args).creation_flags(CREATE_NO_WINDOW).output().ok()
}

#[cfg(windows)]
fn detect_rustdesk() -> RustDeskSignals {
    let ok = |o: Option<std::process::Output>| o.map(|o| o.status.success()).unwrap_or(false);
    let key = format!(r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\{RUSTDESK_APP}");
    let program_dir = ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"].iter().any(|v| {
        std::env::var(v)
            .map(|pf| std::path::Path::new(&pf).join(RUSTDESK_APP).join("rustdesk.exe").exists())
            .unwrap_or(false)
    });
    let tasks = run("tasklist", &["/fo", "csv", "/nh"])
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    RustDeskSignals {
        uninstall_key: ok(run("reg", &["query", &key])),
        service: ok(run("sc", &["query", RUSTDESK_APP])),
        program_dir,
        // si tasklist falla no sabemos: mejor no tocar nada
        running: tasks.is_empty() || rustdesk_running(&tasks),
    }
}

/// Una pasada: si quedó la impresora de un RustDesk que ya no está, se quita (impresora, puerto y
/// driver; el driver no se borra si otra impresora lo usa).
#[cfg(windows)]
pub fn run_once() {
    if disabled() || !crate::platform::is_win_10_or_greater() {
        return; // RustDesk solo instala la impresora en Windows 10 o más nuevo
    }
    let printer = match remote_printer::is_rd_printer_installed(RUSTDESK_APP) {
        Ok(v) => v,
        Err(e) => {
            hbb_common::log::warn!("canela: no se pudo revisar la impresora de RustDesk: {e}");
            return;
        }
    };
    if !printer {
        return;
    }
    let rd = detect_rustdesk();
    if !should_remove(printer, &rd) {
        hbb_common::log::info!("canela: \"RustDesk Printer\" se deja: RustDesk sigue en el equipo ({rd:?})");
        return;
    }
    remote_printer::uninstall_printer(RUSTDESK_APP);
    match remote_printer::is_rd_printer_installed(RUSTDESK_APP) {
        Ok(false) => hbb_common::log::info!("canela: quitada la \"RustDesk Printer\" que dejó un RustDesk desinstalado"),
        _ => hbb_common::log::warn!("canela: no se pudo quitar la \"RustDesk Printer\" (¿sin permisos de administrador?)"),
    }
}

#[cfg(not(windows))]
pub fn run_once() {}

/// ¿Está apagado por configuración? (`canela-cleanup=N`)
fn disabled() -> bool {
    hbb_common::config::Config::get_option("canela-cleanup").eq_ignore_ascii_case("N")
}

use std::sync::atomic::{AtomicBool, Ordering};
static STARTED: AtomicBool = AtomicBool::new(false);

/// Al minuto y medio de arrancar y luego una vez al día. Lo llama `sync::start()` (el servicio,
/// que corre como SYSTEM y puede borrar impresoras).
pub fn start() {
    if !cfg!(windows) || STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(90));
        loop {
            run_once();
            std::thread::sleep(std::time::Duration::from_secs(24 * 60 * 60));
        }
    });
}
