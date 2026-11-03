//! CanelaRemote: comandos, capturas de pantalla y archivos para las herramientas del dueño (parche 33).
//!
//! Lo usa solo el MCP de CanelaRemote del dueño (superadmin). La API pone `"cmd": true` en el
//! ticket únicamente cuando lo pide el superadmin, y este equipo lo comprueba con la firma
//! (canela_ticket.rs::claims): un técnico, aunque sea admin, no puede usarlo ni lo ve en la app.
//!
//! Cómo llega: una regla de túnel con host remoto `canela-cmd` (tools/canela-tunel, que llama el
//! MCP). La sesión de CanelaRemote, en vez de ir a un puerto del equipo, va a este servidor local:
//!
//! ```text
//!  MCP ──TCP──▶ 127.0.0.1:<puerto> (App Técnico del dueño)
//!                     │  una sesión de CanelaRemote (ticket con cmd, cifrado, relay)
//!                     ▼
//!              equipo del cliente: serve() → comandos, captura, archivos
//! ```
//!
//! Protocolo: una línea JSON por pedido y una por respuesta; varios pedidos por sesión, en orden.
//!
//! ```text
//!  {"op":"info"}
//!  {"op":"exec","cmd":"ipconfig /all","shell":"cmd|powershell|sh","timeout":60,"cwd":"C:\\"}
//!  {"op":"screenshot","display":0,"max_width":1600,"quality":80}   → JPEG en base64
//!  {"op":"read_file","path":"C:\\x.log"}                           → base64 (máx. 20 MB)
//!  {"op":"write_file","path":"C:\\x.txt","b64":"…"}
//! ```
//!
//! En Windows corre dentro del servidor de CanelaRemote: como SYSTEM, en la sesión de la consola.
//! Cada pedido queda en el log del equipo. Se apaga en un equipo con `canela-cmd=N` en custom.txt.

#![allow(dead_code)]
use hbb_common::{
    base64::{engine::general_purpose::STANDARD as B64, Engine},
    config::Config,
    log,
    message_proto::LoginRequest,
    serde_json::{self, json, Value},
    tokio::{
        self,
        io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
        net::TcpStream,
    },
};
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Host remoto de la regla de túnel que significa "comandos del dueño" (no es un equipo).
pub const MARKER: &str = "canela-cmd";
/// Opción de custom.txt que lo apaga en este equipo (`canela-cmd=N`).
pub const OPTION: &str = "canela-cmd";
/// Pedido más largo (un write_file de 20 MB en base64 cabe).
pub const MAX_LINE: usize = 40 * 1024 * 1024;
/// Archivo más grande que se lee o escribe.
pub const MAX_FILE: u64 = 20 * 1024 * 1024;
/// Salida de un comando que se devuelve (stdout y stderr, cada uno); el resto se cuenta y se corta.
pub const MAX_OUTPUT: usize = 1024 * 1024;
pub const DEFAULT_TIMEOUT: u64 = 60;
pub const MAX_TIMEOUT: u64 = 600;
/// Lo que se espera a la salida después de matar un comando vencido (un nieto puede tener el tubo).
const DRAIN_AFTER_KILL: Duration = Duration::from_secs(2);

/// Imagen de una pantalla en RGBA, de canela_cmd_capture.rs.
pub struct Shot {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub displays: usize,
}

/// ¿Esta sesión puede usar los comandos? Ok(usuario del ticket) solo con un ticket válido que
/// traiga `"cmd": true`. El Err va al log del equipo; al técnico le llega un "sin permiso" seco.
pub fn allowed(lr: &LoginRequest) -> Result<String, String> {
    if Config::get_option(OPTION).eq_ignore_ascii_case("N") {
        return Err("apagado en este equipo (canela-cmd=N)".into());
    }
    let t = crate::canela_ticket::claims(lr)?;
    if t.get("cmd").and_then(|v| v.as_bool()) != Some(true) {
        return Err("el ticket no trae permiso de comandos".into());
    }
    Ok(t.get("u").and_then(|v| v.as_str()).unwrap_or_default().to_owned())
}

/// Equipo del cliente: la punta que va a la sesión de RustDesk; la otra la atiende `serve`.
pub async fn serve_local(user: String) -> std::io::Result<TcpStream> {
    let (near, far) = crate::canela_socks::loopback_pair().await?;
    tokio::spawn(serve(far, user));
    Ok(near)
}

/// Atiende los pedidos de una sesión, uno detrás de otro, hasta que se cierre.
pub async fn serve(stream: TcpStream, user: String) {
    let (rd, mut wr) = stream.into_split();
    let mut rd = BufReader::new(rd);
    loop {
        let line = match read_line(&mut rd).await {
            Ok(Some(l)) => l,
            Ok(None) => break,
            Err(e) => {
                let _ = wr.write_all(format!("{}\n", err(e.to_string())).as_bytes()).await;
                break;
            }
        };
        let who = user.clone();
        let resp = tokio::task::spawn_blocking(move || handle(&line, &who))
            .await
            .unwrap_or_else(|e| err(format!("el pedido falló en el equipo: {e}")));
        if wr.write_all(format!("{resp}\n").as_bytes()).await.is_err() {
            break;
        }
    }
    log::info!("canela-cmd: sesión de {user} cerrada");
}

/// Una línea (sin el salto); None al cerrarse la sesión. Más larga que MAX_LINE es error.
pub async fn read_line<R: AsyncBufRead + Unpin>(rd: &mut R) -> std::io::Result<Option<Vec<u8>>> {
    let mut buf = Vec::new();
    let n = (&mut *rd).take(MAX_LINE as u64 + 1).read_until(b'\n', &mut buf).await?;
    if n == 0 {
        return Ok(None);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
    } else if buf.len() > MAX_LINE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "pedido demasiado grande (máx. 40 MB)",
        ));
    }
    Ok(Some(buf))
}

fn err(e: impl Into<String>) -> Value {
    json!({ "ok": false, "error": e.into() })
}

/// Un pedido → su respuesta (siempre JSON, con "ok").
pub fn handle(line: &[u8], user: &str) -> Value {
    let req: Value = match serde_json::from_slice(line) {
        Ok(v) => v,
        Err(_) => return err("pedido inválido (no es JSON)"),
    };
    let op = req.get("op").and_then(|v| v.as_str()).unwrap_or_default();
    match op {
        "exec" => log::info!("canela-cmd: {user} ejecuta: {}", clip(str_of(&req, "cmd"), 300)),
        "read_file" | "write_file" => log::info!("canela-cmd: {user} {op} {}", str_of(&req, "path")),
        _ => log::info!("canela-cmd: {user} {op}"),
    }
    let res = match op {
        "info" => Ok(info()),
        "exec" => exec(&req),
        "screenshot" => screenshot(&req),
        "read_file" => read_file(&req),
        "write_file" => write_file(&req),
        _ => Err(format!("operación desconocida: '{op}'")),
    };
    match res {
        Ok(mut v) => {
            v["ok"] = json!(true);
            v
        }
        Err(e) => err(e),
    }
}

fn str_of<'a>(req: &'a Value, k: &str) -> &'a str {
    req.get(k).and_then(|v| v.as_str()).unwrap_or_default()
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    s.chars().take(max).collect::<String>() + "…"
}

fn info() -> Value {
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    json!({
        "id": Config::get_id(),
        "hostname": if cfg!(windows) { env("COMPUTERNAME") } else { hbb_common::whoami::devicename() },
        "user": if cfg!(windows) { env("USERNAME") } else { env("USER") },
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "displays": crate::canela_cmd_capture::displays(),
    })
}

// ─────────────────────────── comandos ───────────────────────────

fn exec(req: &Value) -> Result<Value, String> {
    let cmd = str_of(req, "cmd");
    if cmd.trim().is_empty() {
        return Err("falta el comando".into());
    }
    let secs = req.get("timeout").and_then(|v| v.as_u64()).unwrap_or(DEFAULT_TIMEOUT).clamp(1, MAX_TIMEOUT);
    let mut c = shell_command(str_of(req, "shell"), cmd)?;
    let cwd = str_of(req, "cwd");
    if !cwd.is_empty() {
        if !std::path::Path::new(cwd).is_dir() {
            return Err(format!("la carpeta no existe: {cwd}"));
        }
        c.current_dir(cwd);
    }
    run_limited(c, Duration::from_secs(secs))
}

/// El intérprete con salida en UTF-8 (cmd y PowerShell de Windows usan la página OEM si no).
pub fn shell_command(shell: &str, cmd: &str) -> Result<Command, String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut c = match shell {
            "" | "cmd" => {
                // /S /C "…": cmd quita solo las comillas de afuera y corre el resto tal cual
                let mut c = Command::new("cmd.exe");
                c.raw_arg(format!("/D /S /C \"chcp 65001>nul & {cmd}\""));
                c
            }
            "powershell" => {
                let mut c = Command::new("powershell.exe");
                c.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand"])
                    .arg(powershell_encoded(cmd));
                c
            }
            other => return Err(format!("shell desconocido: '{other}' (cmd o powershell)")),
        };
        c.creation_flags(CREATE_NO_WINDOW);
        Ok(c)
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::process::CommandExt;
        let mut c = match shell {
            "" | "sh" | "cmd" => {
                let mut c = Command::new("/bin/sh");
                c.args(["-c", cmd]);
                c
            }
            "powershell" => {
                let mut c = Command::new("pwsh");
                c.args(["-NoProfile", "-NonInteractive", "-EncodedCommand"]).arg(powershell_encoded(cmd));
                c
            }
            other => return Err(format!("shell desconocido: '{other}' (sh o powershell)")),
        };
        c.process_group(0); // para matar también a los hijos si se vence
        Ok(c)
    }
}

/// -EncodedCommand: el script en UTF-16LE y base64, sin problemas de comillas.
pub fn powershell_encoded(cmd: &str) -> String {
    let script = format!("[Console]::OutputEncoding=[Text.Encoding]::UTF8; $ProgressPreference='SilentlyContinue'; {cmd}");
    let utf16: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    B64.encode(utf16)
}

/// Corre el comando con tiempo límite y salida cortada en MAX_OUTPUT.
pub fn run_limited(mut c: Command, limit: Duration) -> Result<Value, String> {
    c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let start = Instant::now();
    let mut child = c.spawn().map_err(|e| format!("no se pudo ejecutar: {e}"))?;
    let out = child.stdout.take().map(drain);
    let errs = child.stderr.take().map(drain);
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if start.elapsed() >= limit => {
                timed_out = true;
                kill_tree(&mut child);
                break child.wait().ok();
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(30)),
            Err(e) => return Err(format!("se perdió el comando: {e}")),
        }
    };
    let wait = if timed_out { DRAIN_AFTER_KILL } else { Duration::from_secs(30) };
    let take = |rx: Option<mpsc::Receiver<(Vec<u8>, usize)>>| {
        rx.and_then(|rx| rx.recv_timeout(wait).ok()).unwrap_or_default()
    };
    let (so, so_total) = take(out);
    let (se, se_total) = take(errs);
    Ok(json!({
        "code": status.and_then(|s| s.code()),
        "stdout": String::from_utf8_lossy(&so),
        "stderr": String::from_utf8_lossy(&se),
        "timed_out": timed_out,
        "truncated": so_total > so.len() || se_total > se.len(),
        "ms": start.elapsed().as_millis() as u64,
    }))
}

/// Lee todo el tubo en otro hilo: guarda hasta MAX_OUTPUT y cuenta el total.
fn drain<R: Read + Send + 'static>(mut r: R) -> mpsc::Receiver<(Vec<u8>, usize)> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut kept, mut total, mut buf) = (Vec::new(), 0usize, [0u8; 8192]);
        while let Ok(n) = r.read(&mut buf) {
            if n == 0 {
                break;
            }
            total += n;
            let room = MAX_OUTPUT.saturating_sub(kept.len());
            kept.extend_from_slice(&buf[..n.min(room)]);
        }
        let _ = tx.send((kept, total));
    });
    rx
}

fn kill_tree(child: &mut std::process::Child) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .creation_flags(0x0800_0000)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(windows))]
    unsafe {
        hbb_common::libc::kill(-(child.id() as i32), hbb_common::libc::SIGKILL);
    }
    let _ = child.kill();
}

// ─────────────────────────── pantalla ───────────────────────────

fn screenshot(req: &Value) -> Result<Value, String> {
    let idx = req.get("display").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let max_w = req.get("max_width").and_then(|v| v.as_u64()).unwrap_or(1600).clamp(320, 7680) as u32;
    let quality = req.get("quality").and_then(|v| v.as_u64()).unwrap_or(80).clamp(30, 95) as u8;
    let shot = crate::canela_cmd_capture::capture(idx)?;
    let (jpeg, w, h) = encode_jpeg(&shot.rgba, shot.width, shot.height, max_w, quality)?;
    Ok(json!({
        "display": idx,
        "displays": shot.displays,
        "width": shot.width,
        "height": shot.height,
        "sent_width": w,
        "sent_height": h,
        "jpeg_b64": B64.encode(jpeg),
    }))
}

/// BGRA o RGBA con relleno al final de cada fila (stride) → RGBA compacto.
pub fn to_rgba(data: &[u8], width: usize, height: usize, stride: usize, bgra: bool) -> Result<Vec<u8>, String> {
    if stride < width * 4 || data.len() < stride * height.saturating_sub(1) + width * 4 {
        return Err("la imagen de la pantalla vino incompleta".into());
    }
    let mut out = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        let row = &data[y * stride..y * stride + width * 4];
        if bgra {
            for p in row.chunks_exact(4) {
                out.extend_from_slice(&[p[2], p[1], p[0], 255]);
            }
        } else {
            out.extend_from_slice(row);
        }
    }
    Ok(out)
}

/// RGBA → JPEG, achicado a `max_w` de ancho si hace falta (para leerla no hace falta 4K).
pub fn encode_jpeg(rgba: &[u8], w: u32, h: u32, max_w: u32, quality: u8) -> Result<(Vec<u8>, u32, u32), String> {
    let img = image::RgbaImage::from_raw(w, h, rgba.to_vec()).ok_or("la imagen no cuadra con su tamaño")?;
    let mut rgb = image::DynamicImage::ImageRgba8(img).to_rgb8();
    if w > max_w {
        let nh = ((h as u64 * max_w as u64) / w as u64).max(1) as u32;
        rgb = image::imageops::resize(&rgb, max_w, nh, image::imageops::FilterType::Triangle);
    }
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
        .encode_image(&rgb)
        .map_err(|e| format!("no se pudo comprimir la captura: {e}"))?;
    Ok((out, rgb.width(), rgb.height()))
}

// ─────────────────────────── archivos ───────────────────────────

fn path_of(req: &Value) -> Result<std::path::PathBuf, String> {
    let p = str_of(req, "path");
    if p.trim().is_empty() {
        return Err("falta la ruta".into());
    }
    Ok(std::path::PathBuf::from(p))
}

fn read_file(req: &Value) -> Result<Value, String> {
    let p = path_of(req)?;
    let meta = std::fs::metadata(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    if meta.is_dir() {
        return Err(format!("{} es una carpeta (usa un comando: dir / Get-ChildItem)", p.display()));
    }
    if meta.len() > MAX_FILE {
        return Err(format!("{} pesa {} MB; el máximo es 20 MB", p.display(), meta.len() / 1_048_576));
    }
    let data = std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    Ok(json!({ "path": p.display().to_string(), "size": data.len(), "b64": B64.encode(&data) }))
}

fn write_file(req: &Value) -> Result<Value, String> {
    let p = path_of(req)?;
    let data = B64.decode(str_of(req, "b64")).map_err(|_| "el contenido no es base64 válido")?;
    if data.len() as u64 > MAX_FILE {
        return Err("el archivo pasa de 20 MB".into());
    }
    if p.is_dir() {
        return Err(format!("{} es una carpeta; pon la ruta completa del archivo", p.display()));
    }
    std::fs::write(&p, &data).map_err(|e| format!("{}: {e}", p.display()))?;
    Ok(json!({ "path": p.display().to_string(), "size": data.len() }))
}
