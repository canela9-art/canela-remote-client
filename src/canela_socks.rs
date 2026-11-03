//! CanelaRemote: acceso a toda la red del equipo por un proxy SOCKS5 (parche 32).
//!
//! El túnel TCP de RustDesk hace un login completo (ticket, clave, relay) por CADA conexión TCP y
//! va a un solo destino. Para trabajar "como si estuvieras en el local" (panel del router, de las
//! impresoras o cámaras, SQL, RDP a otras PCs) eso no sirve: un navegador abre muchas conexiones.
//!
//! Cómo funciona: una regla de túnel con host remoto `canela-red` abre UNA sola sesión de
//! CanelaRemote y lleva adentro muchas conexiones multiplexadas:
//!
//! ```text
//!  programa ──SOCKS5──▶ 127.0.0.1:1080 (App Técnico, MuxClient)
//!                              │  una sesión de CanelaRemote (ticket, cifrado, relay)
//!                              ▼
//!                       equipo del cliente (serve) ──TCP──▶ 192.168.1.x:puerto de su red
//! ```
//!
//! El nombre del destino se resuelve en el equipo del cliente (sirven los nombres de su red). Solo
//! TCP: nada de ping ni UDP. Los permisos son los del túnel normal (opción "túnel" del equipo).
//!
//! Protocolo dentro de la sesión (flujo de bytes): marcos `[tipo u8][id u32][largo u32][datos]`.

#![allow(dead_code)]
use hbb_common::tokio::{
    self,
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
    time::timeout,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Host remoto de la regla de túnel que significa "toda la red" (no es un equipo de verdad).
pub const MARKER: &str = "canela-red";
/// Puerto local que propone la App Técnico para el proxy.
pub const DEFAULT_LOCAL_PORT: u16 = 1080;
/// Datos por marco (lo que se lee de una vez de cada conexión).
pub const MAX_PAYLOAD: usize = 32 * 1024;
/// Conexiones abiertas a la vez por sesión.
pub const MAX_STREAMS: usize = 256;
/// Marcos en cola por conexión antes de frenar al que manda (control de flujo simple).
const STREAM_BUFFER: usize = 64;
const OUT_BUFFER: usize = 256;
/// Cuánto espera el técnico la respuesta del equipo al abrir una conexión.
const OPEN_TIMEOUT: Duration = Duration::from_secs(15);
/// Cuánto espera el equipo del cliente para conectar al destino de su red.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

pub const OPEN: u8 = 1;
pub const OPEN_OK: u8 = 2;
pub const OPEN_ERR: u8 = 3;
pub const DATA: u8 = 4;
pub const CLOSE: u8 = 5;
const HEADER: usize = 9;

/// Códigos de respuesta SOCKS5 (RFC 1928).
pub const REP_OK: u8 = 0;
pub const REP_FAILURE: u8 = 1;
pub const REP_NET_UNREACHABLE: u8 = 3;
pub const REP_HOST_UNREACHABLE: u8 = 4;
pub const REP_REFUSED: u8 = 5;
pub const REP_TTL_EXPIRED: u8 = 6;
pub const REP_CMD_UNSUPPORTED: u8 = 7;
pub const REP_ADDR_UNSUPPORTED: u8 = 8;

#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub kind: u8,
    pub id: u32,
    pub payload: Vec<u8>,
}

// ─────────────────────────── Lógica pura (probada en CI) ───────────────────────────

pub fn encode(f: &Frame) -> Vec<u8> {
    let mut v = Vec::with_capacity(HEADER + f.payload.len());
    v.push(f.kind);
    v.extend_from_slice(&f.id.to_be_bytes());
    v.extend_from_slice(&(f.payload.len() as u32).to_be_bytes());
    v.extend_from_slice(&f.payload);
    v
}

/// Saca un marco del principio de `buf`: `Ok(None)` si todavía falta; error si viene basura.
pub fn decode(buf: &[u8]) -> Result<Option<(Frame, usize)>, String> {
    if buf.len() < HEADER {
        return Ok(None);
    }
    let kind = buf[0];
    if !(OPEN..=CLOSE).contains(&kind) {
        return Err(format!("marco desconocido: {kind}"));
    }
    let id = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]);
    let len = u32::from_be_bytes([buf[5], buf[6], buf[7], buf[8]]) as usize;
    if len > MAX_PAYLOAD + 512 {
        return Err(format!("marco muy grande: {len}"));
    }
    if buf.len() < HEADER + len {
        return Ok(None);
    }
    let payload = buf[HEADER..HEADER + len].to_vec();
    Ok(Some((Frame { kind, id, payload }, HEADER + len)))
}

/// OPEN: `[puerto u16][host utf8]`.
pub fn open_payload(host: &str, port: u16) -> Vec<u8> {
    let mut v = port.to_be_bytes().to_vec();
    v.extend_from_slice(host.as_bytes());
    v
}

pub fn parse_open(p: &[u8]) -> Option<(String, u16)> {
    if p.len() < 3 || p.len() > 2 + 255 {
        return None;
    }
    let port = u16::from_be_bytes([p[0], p[1]]);
    let host = std::str::from_utf8(&p[2..]).ok()?.trim().to_owned();
    if port == 0 || host.is_empty() {
        return None;
    }
    Some((host, port))
}

/// OPEN_ERR: `[código SOCKS u8][motivo utf8]`.
pub fn err_payload(rep: u8, reason: &str) -> Vec<u8> {
    let mut v = vec![rep];
    v.extend_from_slice(reason.as_bytes());
    v
}

pub fn parse_err(p: &[u8]) -> (u8, String) {
    match p.split_first() {
        Some((rep, rest)) => (*rep, String::from_utf8_lossy(rest).into_owned()),
        None => (REP_FAILURE, String::new()),
    }
}

/// Por qué no se pudo conectar en el equipo del cliente → código SOCKS para el programa.
pub fn rep_for(kind: std::io::ErrorKind) -> u8 {
    use std::io::ErrorKind::*;
    match kind {
        ConnectionRefused => REP_REFUSED,
        TimedOut => REP_TTL_EXPIRED,
        AddrNotAvailable | NotFound => REP_HOST_UNREACHABLE,
        _ => REP_FAILURE,
    }
}

/// Respuesta SOCKS5 (dirección de enlace 0.0.0.0:0: los programas no la usan).
pub fn socks_reply(rep: u8) -> [u8; 10] {
    [5, rep, 0, 1, 0, 0, 0, 0, 0, 0]
}

/// Destino de un pedido SOCKS5 según su tipo de dirección (1 IPv4, 3 nombre, 4 IPv6).
pub fn parse_addr(atyp: u8, addr: &[u8]) -> Option<String> {
    match atyp {
        1 if addr.len() == 4 => Some(std::net::Ipv4Addr::new(addr[0], addr[1], addr[2], addr[3]).to_string()),
        3 if !addr.is_empty() => std::str::from_utf8(addr).ok().map(|s| s.to_owned()),
        4 if addr.len() == 16 => {
            let mut b = [0u8; 16];
            b.copy_from_slice(addr);
            Some(std::net::Ipv6Addr::from(b).to_string())
        }
        _ => None,
    }
}

// ─────────────────────────── SOCKS5 (lado del técnico) ───────────────────────────

/// Saludo y pedido SOCKS5: devuelve el destino (host, puerto) o responde el error y corta.
pub async fn socks_handshake<S: AsyncRead + AsyncWrite + Unpin>(s: &mut S) -> std::io::Result<Option<(String, u16)>> {
    let mut hdr = [0u8; 2];
    s.read_exact(&mut hdr).await?;
    if hdr[0] != 5 {
        return Ok(None); // no es SOCKS5
    }
    let mut methods = vec![0u8; hdr[1] as usize];
    s.read_exact(&mut methods).await?;
    if !methods.contains(&0) {
        s.write_all(&[5, 0xff]).await?; // solo "sin autenticación": el proxy escucha en 127.0.0.1
        return Ok(None);
    }
    s.write_all(&[5, 0]).await?;
    let mut req = [0u8; 4];
    s.read_exact(&mut req).await?;
    let addr = match req[3] {
        1 => {
            let mut a = vec![0u8; 4];
            s.read_exact(&mut a).await?;
            a
        }
        3 => {
            let mut n = [0u8; 1];
            s.read_exact(&mut n).await?;
            let mut a = vec![0u8; n[0] as usize];
            s.read_exact(&mut a).await?;
            a
        }
        4 => {
            let mut a = vec![0u8; 16];
            s.read_exact(&mut a).await?;
            a
        }
        _ => {
            s.write_all(&socks_reply(REP_ADDR_UNSUPPORTED)).await?;
            return Ok(None);
        }
    };
    let mut port = [0u8; 2];
    s.read_exact(&mut port).await?;
    if req[1] != 1 {
        s.write_all(&socks_reply(REP_CMD_UNSUPPORTED)).await?; // solo CONNECT (nada de UDP ni BIND)
        return Ok(None);
    }
    match parse_addr(req[3], &addr) {
        Some(host) => Ok(Some((host, u16::from_be_bytes(port)))),
        None => {
            s.write_all(&socks_reply(REP_ADDR_UNSUPPORTED)).await?;
            Ok(None)
        }
    }
}

// ─────────────────────────── Multiplexor (los dos lados) ───────────────────────────

type Streams = Arc<Mutex<HashMap<u32, mpsc::Sender<Vec<u8>>>>>;
type Pending = Arc<Mutex<HashMap<u32, oneshot::Sender<Result<(), (u8, String)>>>>>;

struct Core {
    out: mpsc::Sender<Frame>,
    streams: Streams,
    pending: Pending,
    closed: AtomicBool,
}

impl Core {
    /// Separa el flujo en lector y escritor; el escritor manda todo lo que salga por `out`.
    fn start<IO>(io: IO, is_server: bool) -> Arc<Core>
    where
        IO: AsyncRead + AsyncWrite + Send + 'static,
    {
        let (rd, mut wr) = tokio::io::split(io);
        let (out, mut rx) = mpsc::channel::<Frame>(OUT_BUFFER);
        let core = Arc::new(Core {
            out,
            streams: Default::default(),
            pending: Default::default(),
            closed: AtomicBool::new(false),
        });
        tokio::spawn(async move {
            while let Some(f) = rx.recv().await {
                if wr.write_all(&encode(&f)).await.is_err() {
                    break;
                }
            }
            let _ = wr.shutdown().await;
        });
        let c = core.clone();
        tokio::spawn(async move {
            if let Err(e) = c.read_loop(rd, is_server).await {
                hbb_common::log::info!("canela-red: sesión terminada: {e}");
            }
            // al caer la sesión se sueltan todas las conexiones
            c.closed.store(true, Ordering::SeqCst);
            c.streams.lock().unwrap().clear();
            c.pending.lock().unwrap().clear();
        });
        core
    }

    async fn read_loop<R: AsyncRead + Unpin>(self: &Arc<Self>, mut rd: R, is_server: bool) -> std::io::Result<()> {
        let mut buf: Vec<u8> = Vec::with_capacity(2 * MAX_PAYLOAD);
        let mut chunk = vec![0u8; MAX_PAYLOAD];
        loop {
            let n = rd.read(&mut chunk).await?;
            if n == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&chunk[..n]);
            loop {
                let (f, used) = match decode(&buf) {
                    Ok(Some(x)) => x,
                    Ok(None) => break,
                    Err(e) => return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
                };
                buf.drain(..used);
                self.dispatch(f, is_server).await;
            }
        }
    }

    async fn dispatch(self: &Arc<Self>, f: Frame, is_server: bool) {
        match f.kind {
            DATA => {
                let tx = self.streams.lock().unwrap().get(&f.id).cloned();
                if let Some(tx) = tx {
                    // si esa conexión va lenta, esto frena la sesión: control de flujo simple
                    let _ = tx.send(f.payload).await;
                }
            }
            CLOSE => {
                self.streams.lock().unwrap().remove(&f.id);
            }
            OPEN if is_server => self.open_remote(f.id, &f.payload),
            OPEN_OK if !is_server => {
                if let Some(tx) = self.pending.lock().unwrap().remove(&f.id) {
                    let _ = tx.send(Ok(()));
                }
            }
            OPEN_ERR if !is_server => {
                if let Some(tx) = self.pending.lock().unwrap().remove(&f.id) {
                    let _ = tx.send(Err(parse_err(&f.payload)));
                }
            }
            _ => {}
        }
    }

    /// Equipo del cliente: conectar al destino de su red y empezar a pasar datos.
    fn open_remote(self: &Arc<Self>, id: u32, payload: &[u8]) {
        let target = parse_open(payload);
        let inbox = {
            let mut s = self.streams.lock().unwrap();
            if target.is_none() || s.len() >= MAX_STREAMS || s.contains_key(&id) {
                None
            } else {
                let (tx, rx) = mpsc::channel(STREAM_BUFFER);
                s.insert(id, tx);
                Some(rx)
            }
        };
        let core = self.clone();
        tokio::spawn(async move {
            let (Some((host, port)), Some(inbox)) = (target, inbox) else {
                let why = err_payload(REP_FAILURE, "destino inválido o demasiadas conexiones");
                let _ = core.out.send(Frame { kind: OPEN_ERR, id, payload: why }).await;
                return;
            };
            match timeout(CONNECT_TIMEOUT, TcpStream::connect((host.as_str(), port))).await {
                Ok(Ok(sock)) => {
                    let _ = sock.set_nodelay(true);
                    if core.out.send(Frame { kind: OPEN_OK, id, payload: vec![] }).await.is_ok() {
                        core.pump(id, sock, inbox).await;
                    }
                }
                Ok(Err(e)) => {
                    core.streams.lock().unwrap().remove(&id);
                    let why = err_payload(rep_for(e.kind()), &e.to_string());
                    let _ = core.out.send(Frame { kind: OPEN_ERR, id, payload: why }).await;
                }
                Err(_) => {
                    core.streams.lock().unwrap().remove(&id);
                    let why = err_payload(REP_TTL_EXPIRED, "tiempo agotado");
                    let _ = core.out.send(Frame { kind: OPEN_ERR, id, payload: why }).await;
                }
            }
        });
    }

    /// Pasa datos entre una conexión TCP local y su canal en la sesión, hasta que las dos puntas
    /// cierran. Lo que se lee va en marcos DATA; al terminar de leer se avisa con CLOSE.
    async fn pump(self: &Arc<Self>, id: u32, sock: TcpStream, mut inbox: mpsc::Receiver<Vec<u8>>) {
        let (mut rd, mut wr) = sock.into_split();
        let out = self.out.clone();
        let up = async move {
            let mut buf = vec![0u8; MAX_PAYLOAD];
            loop {
                match rd.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if out.send(Frame { kind: DATA, id, payload: buf[..n].to_vec() }).await.is_err() {
                            break;
                        }
                    }
                }
            }
            let _ = out.send(Frame { kind: CLOSE, id, payload: vec![] }).await;
        };
        let down = async move {
            while let Some(data) = inbox.recv().await {
                if wr.write_all(&data).await.is_err() {
                    break;
                }
            }
            let _ = wr.shutdown().await;
        };
        tokio::join!(up, down);
        self.streams.lock().unwrap().remove(&id);
    }
}

/// Lado del técnico: atiende las conexiones SOCKS5 locales sobre una sesión ya abierta.
pub struct MuxClient {
    core: Arc<Core>,
    next_id: AtomicU32,
}

impl MuxClient {
    pub fn start<IO>(io: IO) -> Arc<MuxClient>
    where
        IO: AsyncRead + AsyncWrite + Send + 'static,
    {
        Arc::new(MuxClient { core: Core::start(io, false), next_id: AtomicU32::new(1) })
    }

    /// La sesión se cayó: hay que abrir otra antes de atender más conexiones.
    pub fn is_closed(&self) -> bool {
        self.core.closed.load(Ordering::SeqCst)
    }

    pub fn handle_socks(self: &Arc<Self>, sock: TcpStream) {
        let this = self.clone();
        tokio::spawn(async move {
            if let Err(e) = this.socks(sock).await {
                hbb_common::log::debug!("canela-red: conexión SOCKS: {e}");
            }
        });
    }

    async fn socks(&self, mut sock: TcpStream) -> std::io::Result<()> {
        let _ = sock.set_nodelay(true);
        let Some((host, port)) = socks_handshake(&mut sock).await? else {
            return Ok(());
        };
        if self.is_closed() {
            sock.write_all(&socks_reply(REP_NET_UNREACHABLE)).await?;
            return Ok(());
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        // se registra ANTES de pedir: los datos pueden llegar pegados a la respuesta
        let (tx, inbox) = mpsc::channel(STREAM_BUFFER);
        let (otx, orx) = oneshot::channel();
        self.core.streams.lock().unwrap().insert(id, tx);
        self.core.pending.lock().unwrap().insert(id, otx);
        let open = Frame { kind: OPEN, id, payload: open_payload(&host, port) };
        let failed: Option<u8> = if self.core.out.send(open).await.is_err() {
            Some(REP_NET_UNREACHABLE)
        } else {
            match timeout(OPEN_TIMEOUT, orx).await {
                Ok(Ok(Ok(()))) => None,
                Ok(Ok(Err((rep, reason)))) => {
                    hbb_common::log::info!("canela-red: {host}:{port} → {reason}");
                    Some(rep)
                }
                Ok(Err(_)) => Some(REP_NET_UNREACHABLE), // se cayó la sesión
                Err(_) => Some(REP_TTL_EXPIRED),
            }
        };
        match failed {
            None => {
                sock.write_all(&socks_reply(REP_OK)).await?;
                self.core.pump(id, sock, inbox).await;
            }
            Some(rep) => {
                self.core.streams.lock().unwrap().remove(&id);
                self.core.pending.lock().unwrap().remove(&id);
                sock.write_all(&socks_reply(rep)).await?;
            }
        }
        Ok(())
    }
}

/// Lado del equipo del cliente: atiende los pedidos de la sesión sobre `io`.
pub fn serve<IO>(io: IO)
where
    IO: AsyncRead + AsyncWrite + Send + 'static,
{
    let _ = Core::start(io, true);
}

/// Dos puntas TCP conectadas entre sí en 127.0.0.1 (la sesión de RustDesk pide un TcpStream).
/// Se comprueba que la punta aceptada sea la nuestra y no otro programa que se coló.
pub async fn loopback_pair() -> std::io::Result<(TcpStream, TcpStream)> {
    let l = TcpListener::bind("127.0.0.1:0").await?;
    let addr = l.local_addr()?;
    let (a, b) = tokio::join!(TcpStream::connect(addr), l.accept());
    let (a, (b, peer)) = (a?, b?);
    if peer != a.local_addr()? {
        return Err(std::io::Error::new(std::io::ErrorKind::Other, "conexión local inesperada"));
    }
    Ok((a, b))
}

/// Equipo del cliente: la punta que va a la sesión de RustDesk; la otra la atiende `serve`.
pub async fn serve_local() -> std::io::Result<TcpStream> {
    let (near, far) = loopback_pair().await?;
    serve(far);
    Ok(near)
}
